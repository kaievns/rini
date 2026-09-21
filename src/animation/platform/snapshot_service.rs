//! Background window capture through ScreenCaptureKit, for windows SkyLight cannot serve.
//!
//! Fills a cache rather than capturing on demand: a capture is too slow to run at switch time.
//! Results are `IOSurface` to keep a warm cache off the heap. See `docs/animation/capture-overlay-research.md`.

use crate::animation::domain::request::SnapshotTarget;
#[cfg(test)]
use rini_core::ids::WindowServerId;
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
use objc2_io_surface::IOSurfaceRef;
use objc2_screen_capture_kit::{
    SCCaptureResolutionType, SCContentFilter, SCScreenshotManager, SCShareableContent,
    SCStreamConfiguration, SCWindow,
};

use tracing::{debug, warn};

use rini_core::ids::WindowId;
use crate::animation::platform::window_snapshot::{Coverage, SnapshotImage, SnapshotSource, WindowSnapshot};

/// Concurrent captures. ScreenCaptureKit serialises internally, so wall clock stops improving past
/// four. See "Capture cost" in `docs/animation/capture-overlay-research.md`.
const MAX_CONCURRENT: usize = 4;

/// Alpha above which a pixel counts as painted; shadows are excluded, so a painted edge is opaque.
const PAINTED_ALPHA: u8 = 8;

/// Do both far edges of a capture have painted pixels? Several samples per edge, since a rounded
/// corner can leave any single point clear.
fn edges_are_painted(width: usize, height: usize, alpha_at: impl Fn(usize, usize) -> u8) -> bool {
    if width == 0 || height == 0 {
        return false;
    }
    /// How far in from an edge to sample, to clear a rounded corner.
    const INSET: usize = 4;
    let right = width.saturating_sub(1 + INSET);
    let bottom = height.saturating_sub(1 + INSET);
    let mut reaches_right = false;
    let mut reaches_bottom = false;
    for i in 1..8 {
        let y = height * i / 8;
        if y < height && alpha_at(right, y) > PAINTED_ALPHA {
            reaches_right = true;
        }
        let x = width * i / 8;
        if x < width && alpha_at(x, bottom) > PAINTED_ALPHA {
            reaches_bottom = true;
        }
    }
    reaches_right && reaches_bottom
}

/// Does a capture's content reach the far edge of its buffer? `None` means it could not be inspected.
/// See "Nominal capture resolution paints a quarter of the buffer" in `docs/animation/capture-overlay-research.md`.
fn content_reaches_edges(buffer: &objc2_core_video::CVPixelBuffer) -> Option<bool> {
    use objc2_core_video::{
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
        CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
        CVPixelBufferUnlockBaseAddress,
    };

    let width = CVPixelBufferGetWidth(buffer);
    let height = CVPixelBufferGetHeight(buffer);
    if width == 0 || height == 0 {
        return None;
    }
    // The pixel buffer's lock, not the IOSurface's: an IOSurface lock off the main thread races
    // SkyLight's lazy initialisation.
    if unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return None;
    }
    let stride = CVPixelBufferGetBytesPerRow(buffer);
    let base = CVPixelBufferGetBaseAddress(buffer) as *const u8;
    let painted = if base.is_null() || stride < width * 4 {
        None
    } else {
        Some(edges_are_painted(width, height, |x, y| {
            // SAFETY: bounded by the buffer's dimensions and stride, checked above, while locked.
            unsafe { *base.add(y * stride + x * 4 + 3) }
        }))
    };
    unsafe { CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    painted
}

/// Copies a capture's pixels into an IOSurface this process owns: ScreenCaptureKit recycles its pool
/// surfaces once the completion returns. See "Cached surfaces have to be marked in use" in the research doc.
fn own_copy(source: &IOSurfaceRef) -> Option<CFRetained<IOSurfaceRef>> {
    use objc2_core_foundation::{CFDictionary, CFNumber, CFString};
    use objc2_io_surface::{
        IOSurfaceLockOptions, kIOSurfaceBytesPerElement, kIOSurfaceBytesPerRow, kIOSurfaceHeight,
        kIOSurfacePixelFormat, kIOSurfaceWidth,
    };
    let (width, height) = (source.width(), source.height());
    let stride = source.bytes_per_row();
    if width == 0 || height == 0 || stride < width * 4 {
        return None;
    }
    let keys: [&CFString; 5] = [
        unsafe { kIOSurfaceWidth },
        unsafe { kIOSurfaceHeight },
        unsafe { kIOSurfaceBytesPerElement },
        unsafe { kIOSurfaceBytesPerRow },
        unsafe { kIOSurfacePixelFormat },
    ];
    let values = [
        CFNumber::new_i64(width as i64),
        CFNumber::new_i64(height as i64),
        CFNumber::new_i64(4),
        CFNumber::new_i64(stride as i64),
        CFNumber::new_i64(source.pixel_format() as i64),
    ];
    let value_refs: [&CFNumber; 5] = std::array::from_fn(|i| &*values[i]);
    let properties = CFDictionary::from_slices(&keys, &value_refs);
    // SAFETY: locks are paired below and the copy stays inside both buffers' stride * height extents.
    let copy = unsafe { IOSurfaceRef::new(properties.as_opaque()) }?;
    let copy_stride = copy.bytes_per_row();
    let row_bytes = stride.min(copy_stride);
    unsafe {
        if source.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) != 0 {
            return None;
        }
        if copy.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) != 0 {
            source.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
            return None;
        }
        let src = source.base_address().as_ptr() as *const u8;
        let dst = copy.base_address().as_ptr() as *mut u8;
        for row in 0..height {
            std::ptr::copy_nonoverlapping(src.add(row * stride), dst.add(row * copy_stride), row_bytes);
        }
        copy.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut());
        source.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
    }
    Some(copy)
}

/// A window to capture, and the size its pixels should represent.
struct PendingCapture {
    target: SnapshotTarget,
    /// The window's frame size at enumeration: what the capture is of. Mid-resize it differs from
    /// `target.size`.
    size: CGSize,
    filter: Retained<SCContentFilter>,
    config: Retained<SCStreamConfiguration>,
    revision: u64,
}

// SAFETY: the filter and configuration are immutable once queued, and consumed only by a
// thread-safe class method.
unsafe impl Send for PendingCapture {}

#[derive(Default)]
struct ServiceState {
    /// Completed captures waiting to be collected.
    ready: HashMap<WindowId, WindowSnapshot>,
    /// Targets with a capture in flight, so a burst of events cannot queue the same window twice.
    in_flight: HashSet<WindowId>,
    queued: VecDeque<PendingCapture>,
    active: usize,
    /// The most recent desktop capture, waiting to be collected.
    desktop: Option<WindowSnapshot>,
    desktop_in_flight: bool,
}

/// Captures windows in the background and holds the results until collected. Every clone shares
/// one queue and one result set.
#[derive(Clone)]
pub struct SnapshotService {
    state: Arc<Mutex<ServiceState>>,
    /// Bumped on display or scale changes; results captured against an older revision are dropped.
    revision: Arc<AtomicU64>,
    scale: Arc<Mutex<f64>>,
    /// Called on the capturing queue when a result has landed.
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

    /// Invalidates everything in flight on a display change. See "A render of the wrong display,
    /// drawn at its own size" in `docs/animation/capture-overlay-research.md`.
    pub fn invalidate(&self) {
        self.revision.fetch_add(1, Ordering::Release);
    }

    /// Invalidates everything in flight when the backing scale changes.
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

    /// Takes every completed capture, leaving the service empty. The caller owns the cache, so a
    /// capture landing mid-animation cannot mutate it mid-frame.
    pub fn collect(&self) -> Vec<(WindowId, WindowSnapshot)> {
        let mut state = self.state.lock().unwrap();
        state.ready.drain().collect()
    }

    /// Requests captures for `targets`, skipping any already in flight. `onScreenWindowsOnly` must be
    /// false or hidden-workspace windows are never enumerated.
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
                    debug!(
                        wsid = target.server_id.as_u32(),
                        pid = target.window.pid,
                        "capture target not enumerated by ScreenCaptureKit"
                    );
                    service.abandon(std::slice::from_ref(target));
                    continue;
                };

                // Buffer sized from the window as it is, not `target.size`: ScreenCaptureKit
                // aspect-fits the real window into whatever buffer it is given.
                let actual = unsafe { window.frame() }.size;
                let size = if actual.width >= 1.0 && actual.height >= 1.0 {
                    actual
                } else {
                    target.size
                };
                let filter = unsafe {
                    SCContentFilter::initWithDesktopIndependentWindow(
                        SCContentFilter::alloc(),
                        &window,
                    )
                };
                let config = unsafe { SCStreamConfiguration::new() };
                unsafe {
                    config.setWidth(((size.width * scale) as usize).max(1));
                    config.setHeight(((size.height * scale) as usize).max(1));
                    config.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
                    config.setShowsCursor(false);
                    config.setCapturesAudio(false);
                    // The compositor draws the tile's shadow; a baked one would double it.
                    config.setIgnoreShadowsSingleWindow(true);
                    config.setIgnoreGlobalClipSingleWindow(true);
                    // Rounded corners must stay transparent.
                    config.setShouldBeOpaque(false);
                    // Nominal renders at point size into a pixel-sized buffer. See "Nominal capture
                    // resolution paints a quarter of the buffer" in docs/animation/capture-overlay-research.md.
                    config.setCaptureResolution(SCCaptureResolutionType::Best);
                }
                queued.push(PendingCapture { target: *target, size, filter, config, revision });
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

    /// Requests a render of the display with every app window excluded. See "The wallpaper is not
    /// reliably a window" in `docs/animation/capture-overlay-research.md`.
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

            // The bar is excluded too, although below layer 0: the overlay draws it from its own
            // capture. See "The bar has to be captured on its own" in docs/animation/capture-overlay-research.md.
            let windows = unsafe { content.windows() };
            let excluded: Vec<Retained<SCWindow>> = windows
                .iter()
                .filter(|window| {
                    let layer = unsafe { window.windowLayer() } as i64;
                    layer >= 0 || crate::animation::platform::backdrop::is_bar_layer(layer)
                })
                .collect();
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
                    let Some(surface) = own_copy(&surface) else {
                        return;
                    };
                    let width = surface.width() as f64 / scale;
                    let height = surface.height() as f64 / scale;
                    state.desktop = Some(WindowSnapshot {
                        image: SnapshotImage::Surface(surface),
                        coverage: Coverage {
                            covered: (width, height),
                            window: (size.width, size.height),
                        },
                        source: SnapshotSource::ScreenCaptureKit,
                        dressing: None,
                        taken: std::time::Instant::now(),
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
            let size = capture.size;
            let revision = capture.revision;
            let scale = self.scale();
            let completion =
                RcBlock::new(move |sample: *mut CMSampleBuffer, _error: *mut NSError| {
                    let buffer = NonNull::new(sample)
                        .and_then(|sample| unsafe { sample.as_ref().image_buffer() });
                    let filled = buffer.as_ref().and_then(|b| content_reaches_edges(b));
                    let surface = buffer.and_then(|b| CVPixelBufferGetIOSurface(Some(&b)));
                    // No capture calls in here: `CGWindowListCreateImage` is proxied through this
                    // same delivery queue and deadlocks until a ~20s timeout. The owner harvests later.
                    service.finish(target, size, revision, scale, surface, filled, None);
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
        size: CGSize,
        revision: u64,
        scale: f64,
        surface: Option<CFRetained<IOSurfaceRef>>,
        filled: Option<bool>,
        dressing: Option<crate::animation::platform::edge_dressing::EdgeDressing>,
    ) {
        let landed = {
            let mut state = self.state.lock().unwrap();
            state.in_flight.remove(&target.window);
            state.active = state.active.saturating_sub(1);

            if revision != self.revision.load(Ordering::Acquire) {
                false
            } else if filled == Some(false) {
                // Rejected rather than cached: an underfilled capture's coverage would claim the
                // full buffer and pass every fit test.
                warn!(
                    wsid = target.server_id.as_u32(),
                    pid = target.window.pid,
                    "capture did not fill its buffer; rejected. A persistent repeat means the \
                     buffer is sized wrongly (captureResolution, or points given as pixels)"
                );
                false
            } else if let Some(surface) = surface.as_deref().and_then(own_copy) {
                let width = surface.width() as f64 / scale;
                let height = surface.height() as f64 / scale;
                state.ready.insert(
                    target.window,
                    WindowSnapshot {
                        image: SnapshotImage::Surface(surface),
                        coverage: Coverage {
                            covered: (width, height),
                            window: (size.width, size.height),
                        },
                        source: SnapshotSource::ScreenCaptureKit,
                        dressing,
                        taken: std::time::Instant::now(),
                    },
                );
                true
            } else {
                debug!(
                    wsid = target.server_id.as_u32(),
                    pid = target.window.pid,
                    "capture produced no surface"
                );
                false
            }
        };
        if landed {
            (self.notify)();
        }
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

    #[test]
    fn a_captures_copy_is_its_own_surface_with_the_same_pixels() {
        use objc2_core_foundation::{CFDictionary, CFNumber, CFString};
        use objc2_io_surface::{
            IOSurfaceLockOptions, kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfacePixelFormat,
            kIOSurfaceWidth,
        };
        let keys: [&CFString; 4] = [
            unsafe { kIOSurfaceWidth },
            unsafe { kIOSurfaceHeight },
            unsafe { kIOSurfaceBytesPerElement },
            unsafe { kIOSurfacePixelFormat },
        ];
        let values = [
            CFNumber::new_i64(7),
            CFNumber::new_i64(5),
            CFNumber::new_i64(4),
            CFNumber::new_i64(u32::from_be_bytes(*b"BGRA") as i64),
        ];
        let value_refs: [&CFNumber; 4] = std::array::from_fn(|i| &*values[i]);
        let source = unsafe { IOSurfaceRef::new(CFDictionary::from_slices(&keys, &value_refs).as_opaque()) }
            .expect("a source surface");
        let stride = source.bytes_per_row();
        unsafe {
            assert_eq!(source.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()), 0);
            let base = source.base_address().as_ptr() as *mut u8;
            for y in 0..5 {
                for x in 0..7 * 4 {
                    *base.add(y * stride + x) = (y * 31 + x) as u8;
                }
            }
            source.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut());
        }

        let copy = own_copy(&source).expect("a copy");
        assert_ne!(copy.id(), source.id(), "a surface of our own");
        assert_eq!((copy.width(), copy.height()), (7, 5));
        assert_eq!(copy.pixel_format(), source.pixel_format());
        unsafe {
            assert_eq!(copy.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()), 0);
            let base = copy.base_address().as_ptr() as *const u8;
            let copy_stride = copy.bytes_per_row();
            for y in 0..5 {
                for x in 0..7 * 4 {
                    assert_eq!(*base.add(y * copy_stride + x), (y * 31 + x) as u8, "({x},{y})");
                }
            }
            copy.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
        }
    }

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
        let service = service();
        let before = service.revision.load(Ordering::Acquire);
        service.set_scale(1.0);
        assert!(service.revision.load(Ordering::Acquire) > before);
    }

    #[test]
    fn setting_the_same_scale_does_not_invalidate_anything() {
        let service = service();
        let before = service.revision.load(Ordering::Acquire);
        service.set_scale(2.0);
        assert_eq!(service.revision.load(Ordering::Acquire), before);
    }

    #[test]
    fn abandoning_a_target_lets_it_be_requested_again() {
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
        let service = service();
        {
            let mut state = service.state.lock().unwrap();
            state.ready.insert(
                wid(7),
                WindowSnapshot {
                    image: SnapshotImage::Bitmap(tiny_bitmap()),
                    coverage: Coverage { covered: (859.0, 1081.0), window: (859.0, 1081.0) },
                    source: SnapshotSource::ScreenCaptureKit,
                    dressing: None,
                    taken: std::time::Instant::now(),
                },
            );
        }
        assert_eq!(service.collect().len(), 1);
        assert!(service.collect().is_empty(), "a second collect must not repeat results");
    }

    #[test]
    fn notify_fires_only_when_a_capture_actually_produced_pixels() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let service =
            SnapshotService::new(2.0, Arc::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            }));
        service.finish(
            target(1),
            CGSize::new(859.0, 1081.0),
            service.revision.load(Ordering::Acquire),
            2.0,
            None,
            None,
            None,
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    /// Alpha reader over a plain buffer whose top-left `fill_w` x `fill_h` pixels are painted.
    /// Not a real IOSurface: creating one from a test thread races SkyLight's lazy initialisation.
    fn painted(fill_w: usize, fill_h: usize) -> impl Fn(usize, usize) -> u8 {
        move |x, y| if x < fill_w && y < fill_h { 255 } else { 0 }
    }

    #[test]
    fn a_capture_filling_only_a_corner_of_its_buffer_is_detected() {
        assert!(!edges_are_painted(64, 64, painted(32, 32)));
    }

    #[test]
    fn a_capture_filling_its_buffer_passes() {
        assert!(edges_are_painted(64, 64, painted(64, 64)));
    }

    #[test]
    fn a_capture_a_few_pixels_short_still_passes() {
        assert!(edges_are_painted(64, 64, painted(62, 62)));
    }

    #[test]
    fn a_capture_short_in_only_one_direction_is_detected() {
        assert!(!edges_are_painted(64, 64, painted(64, 32)));
        assert!(!edges_are_painted(64, 64, painted(32, 64)));
    }

    #[test]
    fn a_fully_transparent_capture_is_detected() {
        assert!(!edges_are_painted(64, 64, painted(0, 0)));
    }

    #[test]
    fn a_zero_sized_buffer_is_not_painted() {
        assert!(!edges_are_painted(0, 0, painted(0, 0)));
    }

    /// A 1x1 CPU bitmap. Not an IOSurface: creating one from a test thread races SkyLight's lazy
    /// initialisation and aborts the suite.
    fn tiny_bitmap() -> CFRetained<objc2_core_graphics::CGImage> {
        use objc2_core_graphics::{
            CGBitmapInfo, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
        };

        static PIXEL: [u8; 4] = [0, 0, 0, 255];
        // SAFETY: the data outlives the provider, being a static, so no release callback is needed.
        let provider = unsafe {
            CGDataProvider::with_data(
                std::ptr::null_mut(),
                PIXEL.as_ptr() as *const std::ffi::c_void,
                PIXEL.len(),
                None,
            )
        }
        .expect("data provider");
        let space = CGColorSpace::new_device_rgb().expect("colour space");
        // SAFETY: 1x1 BGRA, and the provider holds exactly those four bytes.
        unsafe {
            CGImage::new(
                1,
                1,
                8,
                32,
                4,
                Some(&space),
                CGBitmapInfo(CGImageAlphaInfo::PremultipliedLast.0),
                Some(&provider),
                std::ptr::null(),
                false,
                objc2_core_graphics::CGColorRenderingIntent::RenderingIntentDefault,
            )
        }
        .expect("image")
    }
}
