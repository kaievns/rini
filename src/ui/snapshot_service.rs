//! Background window capture through ScreenCaptureKit, for windows SkyLight cannot serve.
//!
//! # Why this exists
//!
//! `SLSHWCaptureWindowList` is cheap but reads the FRAMEBUFFER, so it only ever returns the visible
//! portion of a window. Measured on a real scrolling strip, of 11 windows reported on screen only 2
//! captured at full size; the rest came back as 1pt to 2pt slivers because they were parked off-strip,
//! or clipped because a neighbour overlapped them. A window on a workspace that is not showing comes
//! back 1x28.
//!
//! ScreenCaptureKit captures the window's own surface, at full size, whatever its visibility. That is
//! the only way to get pixels for a window that is about to slide into view. It costs about 40ms fixed
//! plus 14.5ms per window and does not get faster past four concurrent captures, so it cannot run at
//! switch time and has to fill a cache ahead of the need.
//!
//! # Cost
//!
//! Nothing here polls. Captures happen only when asked for, so an idle rini costs zero: no timer, no
//! wakeups, no CPU. A refresh of one to three windows is roughly 20ms to 70ms of work on a background
//! queue. Even a pathological full sweep once a minute is well under one percent of a core.
//!
//! Results are `IOSurface`, not bitmaps, so a warm cache does not sit on the process heap. Twenty
//! windows as CPU bitmaps would be around 280MB.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_foundation::{CFRetained, CGSize};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::CVPixelBufferGetIOSurface;
use objc2_foundation::{NSArray, NSError};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};
use objc2_screen_capture_kit::{
    SCCaptureResolutionType, SCContentFilter, SCScreenshotManager, SCShareableContent,
    SCStreamConfiguration, SCWindow,
};

use tracing::{debug, warn};

use crate::actor::app::WindowId;
use crate::sys::window_server::WindowServerId;
use crate::ui::window_snapshot::{Coverage, SnapshotImage, SnapshotSource, WindowSnapshot};

/// Concurrent captures. Measured: wall clock stops improving past four, because ScreenCaptureKit
/// serialises internally. Going wider only queues work and delays the first result.
const MAX_CONCURRENT: usize = 4;

/// Alpha above which a pixel counts as painted. Window corners are rounded and shadows are excluded, so
/// a genuinely painted edge is opaque; anything at or below this is untouched buffer.
const PAINTED_ALPHA: u8 = 8;

/// Does a capture's content actually reach the far edge of the buffer it was given?
///
/// This is the invariant that broke, and it broke silently. `SCStreamConfiguration.width` and `.height`
/// are in PIXELS, and a capture is requested at the window's point size times the backing scale. With
/// `captureResolution` set to `.nominal`, ScreenCaptureKit renders the window at its POINT size into
/// that pixel-sized buffer, so on a 2x display the window fills only the top-left quarter and the rest
/// stays transparent. A layer draws the whole surface, so every window appeared at half size pinned to a
/// corner of a tile that was itself exactly the right size.
///
/// Nothing about the surface's declared dimensions reveals it: the buffer is exactly the size asked
/// for. Only the pixels tell the truth, so they are sampled here. A band inside the right edge and one
/// inside the bottom edge is enough, because underfilling leaves entire edges untouched; there is no
/// need to walk the whole surface.
///
/// Returns `None` when the surface cannot be inspected, which is not a failure: the caller treats an
/// unknown result as fine rather than throwing away a capture it cannot check.
fn content_reaches_edges(surface: &IOSurfaceRef) -> Option<bool> {
    let width = unsafe { surface.width() };
    let height = unsafe { surface.height() };
    if width == 0 || height == 0 {
        return None;
    }
    // SAFETY: read-only lock, released before returning, and the base address is only read inside it.
    let locked = unsafe { surface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
    if locked != 0 {
        return None;
    }
    let stride = surface.bytes_per_row();
    let base = surface.base_address().as_ptr() as *const u8;
    if stride < width * 4 {
        // SAFETY: matching unlock for the successful lock above.
        unsafe { surface.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        return None;
    }

    /// How far in from an edge to sample. Far enough past a rounded corner to be inside real content.
    const INSET: usize = 4;
    let sample = |x: usize, y: usize| -> u8 {
        // SAFETY: x and y are bounded by the surface's own dimensions and stride, checked above, and
        // the surface is locked for reading for the duration of this closure.
        unsafe { *base.add(y * stride + x * 4 + 3) }
    };
    let right = width.saturating_sub(1 + INSET);
    let bottom = height.saturating_sub(1 + INSET);
    // Both edges have to be reached, not either one. A buffer underfilled in only one direction still
    // has content along the other edge, so accepting either was enough to miss a capture that was full
    // width and half height. A test caught that.
    //
    // Several samples per edge rather than one, because a rounded corner or a dark scrollbar can leave
    // any single point transparent on a perfectly good capture.
    let mut reaches_right = false;
    let mut reaches_bottom = false;
    for i in 1..8 {
        let y = height * i / 8;
        if y < height && sample(right, y) > PAINTED_ALPHA {
            reaches_right = true;
        }
        let x = width * i / 8;
        if x < width && sample(x, bottom) > PAINTED_ALPHA {
            reaches_bottom = true;
        }
    }
    let painted = reaches_right && reaches_bottom;
    // SAFETY: matching unlock for the successful lock above.
    unsafe { surface.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
    Some(painted)
}

/// A window to capture, and the size its pixels should represent.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotTarget {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// The window's full size in points. Captured at this size times the backing scale, so a tile
    /// drawn at the window's own size is pixel-exact.
    pub size: CGSize,
}

struct PendingCapture {
    target: SnapshotTarget,
    filter: Retained<SCContentFilter>,
    config: Retained<SCStreamConfiguration>,
    revision: u64,
}

// The filter and configuration are immutable once queued here, and are consumed only by
// ScreenCaptureKit's thread-safe class capture method.
unsafe impl Send for PendingCapture {}

#[derive(Default)]
struct ServiceState {
    /// Completed captures waiting to be collected by the owner of the cache.
    ready: HashMap<WindowId, WindowSnapshot>,
    /// Targets with a capture in flight, so a burst of events cannot queue the same window twice.
    in_flight: HashSet<WindowId>,
    queued: VecDeque<PendingCapture>,
    active: usize,
    /// The most recent desktop capture, waiting to be collected.
    desktop: Option<WindowSnapshot>,
    /// Whether a desktop capture is already running, so a burst of switches queues only one.
    desktop_in_flight: bool,
}

/// Captures windows in the background and holds the results until collected.
///
/// Cloneable: every clone shares one queue and one result set, so the ScreenCaptureKit completion
/// handlers can hand results back without the caller holding a lock.
#[derive(Clone)]
pub struct SnapshotService {
    state: Arc<Mutex<ServiceState>>,
    /// Bumped whenever the display configuration changes, so results captured against stale geometry
    /// are discarded rather than cached at the wrong size.
    revision: Arc<AtomicU64>,
    scale: Arc<Mutex<f64>>,
    /// Called on the capturing queue when at least one result has landed, so the owner knows there is
    /// something to collect without polling.
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl SnapshotService {
    pub fn new(scale: f64, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ServiceState::default())),
            revision: Arc::new(AtomicU64::new(0)),
            scale: Arc::new(Mutex::new(scale)),
            notify,
        }
    }

    /// Invalidates everything in flight. Called when the display configuration changes, because a
    /// capture sized for the old geometry would be cached at the wrong resolution.
    pub fn set_scale(&self, scale: f64) {
        let mut current = self.scale.lock().unwrap();
        if (*current - scale).abs() < f64::EPSILON {
            return;
        }
        *current = scale;
        self.revision.fetch_add(1, Ordering::Release);
    }

    fn scale(&self) -> f64 {
        let scale = *self.scale.lock().unwrap();
        if scale > 0.0 { scale } else { 2.0 }
    }

    /// Takes every completed capture, leaving the service empty.
    ///
    /// The caller owns the cache; this only holds results long enough to hand them over, so a capture
    /// that lands while an animation is running cannot mutate the cache mid-frame.
    pub fn collect(&self) -> Vec<(WindowId, WindowSnapshot)> {
        let mut state = self.state.lock().unwrap();
        state.ready.drain().collect()
    }

    /// Requests captures for `targets`, skipping any already in flight.
    ///
    /// Enumeration is the expensive part of ScreenCaptureKit, so every target in one call shares a
    /// single `SCShareableContent` lookup. `onScreenWindowsOnly` is false, without which windows on a
    /// workspace that is not showing are not enumerated at all and so cannot be captured.
    pub fn request(&self, targets: Vec<SnapshotTarget>) {
        let revision = self.revision.load(Ordering::Acquire);
        let targets: Vec<SnapshotTarget> = {
            let mut state = self.state.lock().unwrap();
            targets
                .into_iter()
                .filter(|target| state.in_flight.insert(target.window))
                .collect()
        };
        if targets.is_empty() {
            return;
        }

        let service = self.clone();
        let scale = self.scale();
        let block = RcBlock::new(move |content: *mut SCShareableContent, _error: *mut NSError| {
            let Some(content) = NonNull::new(content) else {
                debug!(count = targets.len(), "ScreenCaptureKit enumeration returned nothing");
                service.abandon(&targets);
                return;
            };
            if revision != service.revision.load(Ordering::Acquire) {
                service.abandon(&targets);
                return;
            }
            let windows = unsafe { content.as_ref().windows() };
            let mut queued = Vec::with_capacity(targets.len());
            for target in &targets {
                let found = windows
                    .iter()
                    .find(|window| unsafe { window.windowID() } == target.server_id.as_u32());
                let Some(window) = found else {
                    // The window closed between the request and the enumeration. Not an error.
                    debug!(
                        wsid = target.server_id.as_u32(),
                        pid = target.window.pid,
                        "capture target not enumerated by ScreenCaptureKit"
                    );
                    service.abandon(std::slice::from_ref(target));
                    continue;
                };

                let filter = unsafe {
                    SCContentFilter::initWithDesktopIndependentWindow(
                        SCContentFilter::alloc(),
                        &window,
                    )
                };
                let config = unsafe { SCStreamConfiguration::new() };
                unsafe {
                    config.setWidth(((target.size.width * scale) as usize).max(1));
                    config.setHeight(((target.size.height * scale) as usize).max(1));
                    config.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
                    config.setShowsCursor(false);
                    config.setCapturesAudio(false);
                    // Shadows would be baked into the bitmap and then drawn again by the compositor,
                    // giving every animated window a doubled shadow.
                    config.setIgnoreShadowsSingleWindow(true);
                    config.setIgnoreGlobalClipSingleWindow(true);
                    // Not opaque: rounded corners must stay transparent, or every tile animates as a
                    // rectangle with black corners.
                    config.setShouldBeOpaque(false);
                    // Best, not Nominal. Nominal renders the window at its POINT size into a buffer
                    // sized in PIXELS, so on a 2x display the window occupies the top-left quarter of
                    // the surface and the rest is transparent. Drawing that surface into a layer then
                    // shows the window at half size in a corner, which is the "windows are scaled"
                    // report. Measured on one window, same 1718x2162 buffer each time:
                    //
                    //   nominal   -> painted  859x1081 of 1718x2162   50% x 50%
                    //   best      -> painted 1718x2162 of 1718x2162  100% x 100%
                    //   automatic -> painted 1718x2162 of 1718x2162  100% x 100%
                    //
                    // Cropping the surface to the painted quarter instead gets the size right but at
                    // half the resolution, which reads as blurry. This gets the pixels.
                    config.setCaptureResolution(SCCaptureResolutionType::Best);
                }
                queued.push(PendingCapture { target: *target, filter, config, revision });
            }
            service.enqueue(queued);
        });

        unsafe {
            SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                true, false, &block,
            );
        }
    }

    /// Takes the most recent desktop capture, if one has landed since the last call.
    pub fn take_desktop(&self) -> Option<WindowSnapshot> {
        self.state.lock().unwrap().desktop.take()
    }

    /// Requests a capture of the desktop: the display with every app window excluded.
    ///
    /// This exists because the wallpaper is not reliably a window. Compositing everything at or below
    /// the desktop level through SkyLight worked while the wallpaper had its own window, but with a
    /// second display attached that window disappears from the window server listing altogether, and
    /// the composite comes back holding the icons on black. Measured: brightness 24.7 with one display
    /// against 7.8 with two, and ScreenCaptureKit rendering the same desktop at 23.4.
    ///
    /// ScreenCaptureKit renders a DISPLAY rather than a set of windows, so the wallpaper is included
    /// whether or not anything lists it. The cost is why this is a background request feeding a cache:
    /// the desktop changes rarely, so a capture a few seconds old is indistinguishable from a fresh one.
    pub fn request_desktop(&self, display_id: u32, size: CGSize) {
        {
            let mut state = self.state.lock().unwrap();
            if state.desktop_in_flight {
                return;
            }
            state.desktop_in_flight = true;
        }

        let revision = self.revision.load(Ordering::Acquire);
        let service = self.clone();
        let scale = self.scale();
        let block = RcBlock::new(move |content: *mut SCShareableContent, _error: *mut NSError| {
            let Some(content) = NonNull::new(content) else {
                debug!("ScreenCaptureKit enumeration returned nothing for the desktop");
                service.finish_desktop(revision, scale, size, None);
                return;
            };
            let content = unsafe { content.as_ref() };
            let displays = unsafe { content.displays() };
            let Some(display) =
                displays.iter().find(|display| unsafe { display.displayID() } == display_id)
            else {
                debug!(display_id, "display not enumerated by ScreenCaptureKit");
                service.finish_desktop(revision, scale, size, None);
                return;
            };

            // Everything at or above the normal window level, which is every window the overlay might
            // animate. What is left is the wallpaper, the desktop icons and the widgets.
            let windows = unsafe { content.windows() };
            let excluded: Vec<Retained<SCWindow>> =
                windows.iter().filter(|window| unsafe { window.windowLayer() } >= 0).collect();
            let excluded_refs: Vec<&SCWindow> = excluded.iter().map(|window| &**window).collect();
            let excluded = NSArray::from_slice(&excluded_refs);

            let filter = unsafe {
                SCContentFilter::initWithDisplay_excludingWindows(
                    SCContentFilter::alloc(),
                    &display,
                    &excluded,
                )
            };
            let config = unsafe { SCStreamConfiguration::new() };
            unsafe {
                config.setWidth(((size.width * scale) as usize).max(1));
                config.setHeight(((size.height * scale) as usize).max(1));
                config.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
                config.setShowsCursor(false);
                config.setCapturesAudio(false);
                // The desktop is drawn as the bottom layer, so it wants no transparency of its own.
                config.setShouldBeOpaque(true);
                config.setCaptureResolution(SCCaptureResolutionType::Nominal);
            }

            let service = service.clone();
            let completion = RcBlock::new(move |sample: *mut CMSampleBuffer, _error: *mut NSError| {
                let surface = NonNull::new(sample)
                    .and_then(|sample| unsafe { sample.as_ref().image_buffer() })
                    .and_then(|buffer| CVPixelBufferGetIOSurface(Some(&buffer)));
                service.finish_desktop(revision, scale, size, surface);
            });
            unsafe {
                SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(
                    &filter,
                    &config,
                    Some(&completion),
                );
            }
        });

        unsafe {
            SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                false, false, &block,
            );
        }
    }

    fn finish_desktop(
        &self,
        revision: u64,
        scale: f64,
        size: CGSize,
        surface: Option<CFRetained<IOSurfaceRef>>,
    ) {
        let landed = {
            let mut state = self.state.lock().unwrap();
            state.desktop_in_flight = false;
            match surface {
                Some(surface) if revision == self.revision.load(Ordering::Acquire) => {
                    let width = unsafe { surface.width() } as f64 / scale;
                    let height = unsafe { surface.height() } as f64 / scale;
                    state.desktop = Some(WindowSnapshot {
                        image: SnapshotImage::Surface(surface),
                        coverage: Coverage {
                            covered: (width, height),
                            window: (size.width, size.height),
                        },
                        source: SnapshotSource::ScreenCaptureKit,
                    });
                    true
                }
                _ => false,
            }
        };
        if landed {
            (self.notify)();
        }
    }

    fn enqueue(&self, captures: Vec<PendingCapture>) {
        {
            let mut state = self.state.lock().unwrap();
            state.queued.extend(captures);
        }
        self.pump();
    }

    /// Starts as many queued captures as the concurrency limit allows.
    fn pump(&self) {
        let starting = {
            let mut state = self.state.lock().unwrap();
            let room = MAX_CONCURRENT.saturating_sub(state.active).min(state.queued.len());
            let starting: Vec<PendingCapture> = state.queued.drain(..room).collect();
            state.active += starting.len();
            starting
        };

        for capture in starting {
            let service = self.clone();
            let target = capture.target;
            let revision = capture.revision;
            let scale = self.scale();
            let completion =
                RcBlock::new(move |sample: *mut CMSampleBuffer, _error: *mut NSError| {
                    let surface = NonNull::new(sample)
                        .and_then(|sample| unsafe { sample.as_ref().image_buffer() })
                        .and_then(|buffer| CVPixelBufferGetIOSurface(Some(&buffer)));
                    service.finish(target, revision, scale, surface);
                });
            unsafe {
                SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(
                    &capture.filter,
                    &capture.config,
                    Some(&completion),
                );
            }
        }
    }

    fn finish(
        &self,
        target: SnapshotTarget,
        revision: u64,
        scale: f64,
        surface: Option<CFRetained<IOSurfaceRef>>,
    ) {
        let landed = {
            let mut state = self.state.lock().unwrap();
            state.in_flight.remove(&target.window);
            state.active = state.active.saturating_sub(1);

            if revision != self.revision.load(Ordering::Acquire) {
                false
            } else if surface.is_none() {
                debug!(
                    wsid = target.server_id.as_u32(),
                    pid = target.window.pid,
                    "capture produced no surface"
                );
                false
            } else if let Some(surface) = surface {
                // ScreenCaptureKit returns the window's own surface at the requested size, so the
                // capture covers the whole window by construction. That is the entire reason for
                // using it over SkyLight, which returns only what is on screen.
                //
                // "At the requested size" is only true of the BUFFER, and that is what made this
                // worth checking rather than assuming. See `content_reaches_edges`.
                if content_reaches_edges(&surface) == Some(false) {
                    warn!(
                        wsid = target.server_id.as_u32(),
                        pid = target.window.pid,
                        surface = format!(
                            "{}x{}",
                            unsafe { surface.width() },
                            unsafe { surface.height() }
                        ),
                        "capture did not fill its buffer; windows will draw small and in a corner. \
                         Check SCStreamConfiguration captureResolution and whether width and height \
                         are being given in pixels"
                    );
                }
                let width = unsafe { surface.width() } as f64 / scale;
                let height = unsafe { surface.height() } as f64 / scale;
                state.ready.insert(
                    target.window,
                    WindowSnapshot {
                        image: SnapshotImage::Surface(surface),
                        coverage: Coverage {
                            covered: (width, height),
                            window: (target.size.width, target.size.height),
                        },
                        source: SnapshotSource::ScreenCaptureKit,
                    },
                );
                true
            } else {
                false
            }
        };
        if landed {
            (self.notify)();
        }
        // Keep the queue moving whether or not this one produced pixels.
        self.pump();
    }

    /// Releases targets that will never produce a result, so they can be requested again later.
    fn abandon(&self, targets: &[SnapshotTarget]) {
        let mut state = self.state.lock().unwrap();
        for target in targets {
            state.in_flight.remove(&target.window);
        }
    }

    #[cfg(test)]
    pub fn in_flight_count(&self) -> usize {
        self.state.lock().unwrap().in_flight.len()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    fn wid(idx: u32) -> WindowId {
        WindowId { pid: 1, idx: NonZeroU32::new(idx).unwrap() }
    }

    fn target(idx: u32) -> SnapshotTarget {
        SnapshotTarget {
            window: wid(idx),
            server_id: WindowServerId::new(idx),
            size: CGSize::new(859.0, 1081.0),
        }
    }

    fn service() -> SnapshotService {
        SnapshotService::new(2.0, Arc::new(|| {}))
    }

    #[test]
    fn collect_is_empty_before_anything_lands() {
        assert!(service().collect().is_empty());
    }

    #[test]
    fn a_scale_change_bumps_the_revision_so_stale_captures_are_dropped() {
        // Without this, a capture requested at one backing scale could land after a display change
        // and be cached at the wrong resolution, which would draw the tile at the wrong size.
        let service = service();
        let before = service.revision.load(Ordering::Acquire);
        service.set_scale(1.0);
        assert!(service.revision.load(Ordering::Acquire) > before);
    }

    #[test]
    fn setting_the_same_scale_does_not_invalidate_anything() {
        // A config reload or a redundant screen event must not throw away work in flight.
        let service = service();
        let before = service.revision.load(Ordering::Acquire);
        service.set_scale(2.0);
        assert_eq!(service.revision.load(Ordering::Acquire), before);
    }

    #[test]
    fn abandoning_a_target_lets_it_be_requested_again() {
        // A window that closes mid-capture must not be stuck in flight forever, or it can never be
        // captured again if it reopens with the same id.
        let service = service();
        {
            let mut state = service.state.lock().unwrap();
            state.in_flight.insert(wid(1));
        }
        assert_eq!(service.in_flight_count(), 1);
        service.abandon(&[target(1)]);
        assert_eq!(service.in_flight_count(), 0);
    }

    #[test]
    fn results_are_taken_once_and_only_once() {
        // The owner drains results into its own cache, so a capture landing mid-animation cannot
        // mutate what is being drawn.
        let service = service();
        {
            let mut state = service.state.lock().unwrap();
            state.ready.insert(
                wid(7),
                WindowSnapshot {
                    image: SnapshotImage::Surface(tiny_surface()),
                    coverage: Coverage { covered: (859.0, 1081.0), window: (859.0, 1081.0) },
                    source: SnapshotSource::ScreenCaptureKit,
                },
            );
        }
        assert_eq!(service.collect().len(), 1);
        assert!(service.collect().is_empty(), "a second collect must not repeat results");
    }

    #[test]
    fn notify_fires_only_when_a_capture_actually_produced_pixels() {
        // A failed capture must not wake the owner, or a window that cannot be captured would cause
        // a wakeup on every attempt.
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let service =
            SnapshotService::new(2.0, Arc::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            }));
        service.finish(target(1), service.revision.load(Ordering::Acquire), 2.0, None);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    /// Builds a real IOSurface and paints its top-left `fill_w` x `fill_h` pixels opaque, leaving the
    /// rest untouched. That is exactly the shape of the regression: a buffer of the right size holding
    /// the window in one corner.
    fn surface_painted(width: usize, height: usize, fill_w: usize, fill_h: usize) -> CFRetained<IOSurfaceRef> {
        let surface = sized_surface(width, height);
        // SAFETY: read-write lock released below, and writes stay inside the surface's own bounds.
        let locked = unsafe { surface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
        assert_eq!(locked, 0, "could not lock the test surface");
        let stride = surface.bytes_per_row();
        let base = surface.base_address().as_ptr() as *mut u8;
        for y in 0..height {
            for x in 0..width {
                let painted = x < fill_w && y < fill_h;
                // SAFETY: bounded by the surface's own dimensions and its stride.
                unsafe { *base.add(y * stride + x * 4 + 3) = if painted { 255 } else { 0 } };
            }
        }
        // SAFETY: matching unlock.
        unsafe { surface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
        surface
    }

    /// The regression: `captureResolution` set to nominal renders the window at its POINT size into a
    /// buffer sized in PIXELS, so on a 2x display three quarters of the buffer stays transparent. The
    /// buffer's own dimensions look correct, so only the pixels can catch it.
    #[test]
    fn a_capture_filling_only_a_corner_of_its_buffer_is_detected() {
        let surface = surface_painted(64, 64, 32, 32);
        assert_eq!(content_reaches_edges(&surface), Some(false));
    }

    #[test]
    fn a_capture_filling_its_buffer_passes() {
        let surface = surface_painted(64, 64, 64, 64);
        assert_eq!(content_reaches_edges(&surface), Some(true));
    }

    #[test]
    fn a_capture_a_few_pixels_short_still_passes() {
        // Rounded corners and rounding leave the very edge transparent on a correct capture, so the
        // check samples inside the edge. Being strict here would reject every real window.
        let surface = surface_painted(64, 64, 62, 62);
        assert_eq!(content_reaches_edges(&surface), Some(true));
    }

    #[test]
    fn a_capture_short_in_only_one_direction_is_detected() {
        // Width right, height half: what a buffer sized in pixels but rendered in points looks like if
        // only one axis were wrong. Checking a single corner would miss half of these.
        assert_eq!(content_reaches_edges(&surface_painted(64, 64, 64, 32)), Some(false));
        assert_eq!(content_reaches_edges(&surface_painted(64, 64, 32, 64)), Some(false));
    }

    #[test]
    fn a_fully_transparent_capture_is_detected() {
        assert_eq!(content_reaches_edges(&surface_painted(64, 64, 0, 0)), Some(false));
    }

    /// A real 4x4 IOSurface, which is what the production path actually stores. No test here
    /// inspects the pixels; the surface only has to exist so a `WindowSnapshot` can be built.
    fn tiny_surface() -> CFRetained<IOSurfaceRef> {
        sized_surface(4, 4)
    }

    fn sized_surface(width: usize, height: usize) -> CFRetained<IOSurfaceRef> {
        use objc2_core_foundation::{CFDictionary, CFNumber, CFString};

        let pairs: [(&str, i64); 4] = [
            ("IOSurfaceWidth", width as i64),
            ("IOSurfaceHeight", height as i64),
            ("IOSurfaceBytesPerElement", 4),
            ("IOSurfacePixelFormat", i64::from(u32::from_be_bytes(*b"BGRA"))),
        ];
        let keys: Vec<CFRetained<CFString>> =
            pairs.iter().map(|(key, _)| CFString::from_str(key)).collect();
        let values: Vec<CFRetained<CFNumber>> =
            pairs.iter().map(|(_, value)| CFNumber::new_i64(*value)).collect();
        let key_refs: Vec<&CFString> = keys.iter().map(|key| &**key).collect();
        let value_refs: Vec<&CFNumber> = values.iter().map(|value| &**value).collect();
        let properties = CFDictionary::from_slices(&key_refs, &value_refs);
        // IOSurfaceRef::new takes an untyped CFDictionary, so drop the key and value types.
        // SAFETY: the dictionary holds the documented IOSurface creation keys, and casting only
        // erases the generic parameters of a type that is already a CFDictionary.
        let untyped: CFRetained<CFDictionary> = unsafe { CFRetained::cast_unchecked(properties) };
        unsafe { IOSurfaceRef::new(&untyped) }.expect("IOSurface creation failed")
    }
}
