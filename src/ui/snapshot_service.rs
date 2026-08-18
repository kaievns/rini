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
use objc2_io_surface::IOSurfaceRef;
use objc2_screen_capture_kit::{
    SCCaptureResolutionType, SCContentFilter, SCScreenshotManager, SCShareableContent,
    SCStreamConfiguration, SCWindow,
};

use tracing::debug;

use crate::actor::app::WindowId;
use crate::sys::window_server::WindowServerId;
use crate::ui::window_snapshot::{Coverage, SnapshotImage, SnapshotSource, WindowSnapshot};

/// Concurrent captures. Measured: wall clock stops improving past four, because ScreenCaptureKit
/// serialises internally. Going wider only queues work and delays the first result.
const MAX_CONCURRENT: usize = 4;

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
                    config.setCaptureResolution(SCCaptureResolutionType::Nominal);
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

    /// A real 4x4 IOSurface, which is what the production path actually stores. No test here
    /// inspects the pixels; the surface only has to exist so a `WindowSnapshot` can be built.
    fn tiny_surface() -> CFRetained<IOSurfaceRef> {
        use objc2_core_foundation::{CFDictionary, CFNumber, CFString};

        let pairs: [(&str, i64); 4] = [
            ("IOSurfaceWidth", 4),
            ("IOSurfaceHeight", 4),
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
