//! Bitmap snapshots of windows, for the capture-based animation overlay.
//!
//! SkyLight captures what is on screen, fresh; ScreenCaptureKit serves everything else from a
//! background cache. Constraints and costs of both: `docs/capture-overlay-research.md`.

use std::collections::HashMap;
use std::ffi::c_int;

use objc2_core_foundation::{CFArray, CFRetained, CGSize};
use objc2_core_graphics::CGImage;
use objc2_io_surface::IOSurfaceRef;

use rini_windows::ids::WindowId;
use rini_skylight_sys::{SLSHWCaptureWindowList, SLSMainConnectionID};
use crate::edge_dressing::dressing_after_insert;
use rini_windows::ids::WindowServerId;

pub use crate::motion::fit::{
    Coverage, fits_frame, is_a_resize, is_backdrop_worth_drawing, needs_capture, outgrows,
    picture_is_stale, should_replace, spans_display,
};

/// Undocumented `SLSHWCaptureWindowList` option bits, as yabai passes them. See "What yabai
/// actually does" in `docs/capture-overlay-research.md`.
const CAPTURE_OPTIONS: u32 = (1 << 11) | (1 << 8);

/// A window's pixels, whichever API produced them. Core Animation accepts either form.
#[derive(Clone, Debug)]
pub enum SnapshotImage {
    /// CPU-side bitmap, from `SLSHWCaptureWindowList`.
    Bitmap(CFRetained<CGImage>),
    /// GPU-side surface, from ScreenCaptureKit; off the process's heap.
    Surface(CFRetained<IOSurfaceRef>),
}

// SAFETY: CGImage is immutable and IOSurface is shareable across threads and processes.
unsafe impl Send for SnapshotImage {}

/// A window's pixels, plus how much of the window they cover.
#[derive(Clone, Debug)]
pub struct WindowSnapshot {
    pub image: SnapshotImage,
    pub coverage: Coverage,
    pub source: SnapshotSource,
    /// The hairline this window wore when last seen composited; carried across cache refreshes by
    /// [`SnapshotCache::insert`]. See [`crate::edge_dressing`].
    pub dressing: Option<crate::edge_dressing::EdgeDressing>,
    /// When the pixels were captured; staleness is a reason to re-warm.
    pub taken: std::time::Instant,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SnapshotSource {
    /// Captured fresh from the framebuffer. Only valid for a fully visible window.
    SkyLight,
    /// Captured from the window's own surface. Valid at any visibility, but expensive.
    ScreenCaptureKit,
}

impl WindowSnapshot {
    pub fn is_usable(&self) -> bool {
        self.coverage.is_usable()
    }

    /// Can this picture be drawn at `size` without visibly distorting it? See [`fits_frame`].
    pub fn fits(&self, size: CGSize) -> bool {
        fits_frame(self.coverage.covered, (size.width, size.height))
    }
}

/// Captures one window from the framebuffer through SkyLight. One window per call: a list returns
/// a single flattened composite. `None` is normal (display asleep); callers fall back to the cache.
pub fn capture_via_skylight(
    window: WindowServerId,
    window_size: (f64, f64),
    scale: f64,
) -> Option<WindowSnapshot> {
    let cid = unsafe { SLSMainConnectionID() };
    let id = window.as_u32();
    let array: *mut CFArray<CGImage> =
        unsafe { SLSHWCaptureWindowList(cid, &id as *const u32, 1 as c_int, CAPTURE_OPTIONS) };
    if array.is_null() {
        return None;
    }
    // SAFETY: SLSHWCaptureWindowList returns a +1 CFArray of CGImage, so ownership transfers here.
    let array = unsafe { CFRetained::from_raw(std::ptr::NonNull::new(array)?) };
    let image = array.iter().next()?;

    let px_w = CGImage::width(Some(&image)) as f64;
    let px_h = CGImage::height(Some(&image)) as f64;
    let scale = if scale > 0.0 { scale } else { 1.0 };

    Some(WindowSnapshot {
        image: SnapshotImage::Bitmap(image),
        coverage: Coverage {
            covered: (px_w / scale, px_h / scale),
            window: window_size,
        },
        source: SnapshotSource::SkyLight,
        dressing: None,
        taken: std::time::Instant::now(),
    })
}

/// Captures one window through `CGWindowListCreateImage`, which only renders a composited window.
/// See "The hairline is composited outside every capture" in `docs/capture-overlay-research.md`.
pub fn capture_via_framed(window: WindowServerId, scale: f64) -> Option<WindowSnapshot> {
    let frame = rini_windows::window_server::get_window(window)?.frame;
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 || scale <= 0.0 {
        return None;
    }
    #[allow(deprecated)]
    let image = objc2_core_graphics::CGWindowListCreateImage(
        frame,
        objc2_core_graphics::CGWindowListOption::OptionIncludingWindow,
        window.as_u32(),
        objc2_core_graphics::CGWindowImageOption::empty(),
    )?;
    let px_w = CGImage::width(Some(&image)) as f64;
    let px_h = CGImage::height(Some(&image)) as f64;
    let scale = if scale > 0.0 { scale } else { 1.0 };
    Some(WindowSnapshot {
        image: SnapshotImage::Bitmap(image),
        coverage: Coverage {
            covered: (px_w / scale, px_h / scale),
            window: (frame.size.width, frame.size.height),
        },
        source: SnapshotSource::SkyLight,
        dressing: None,
        taken: std::time::Instant::now(),
    })
}

/// One framed capture that yields the picture and its hairline: the reveal chase's capture.
/// See "A grow holds, then reveals" in `docs/animation-smoothness.md`.
pub fn capture_via_framed_with_dressing(
    window: WindowServerId,
    scale: f64,
) -> Option<WindowSnapshot> {
    use crate::edge_dressing::{
        capture_ring_expanded, harvest_from_capture, picture_within_ring,
    };
    let frame = rini_windows::window_server::get_window(window)?.frame;
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 || scale <= 0.0 {
        return None;
    }
    let framed = capture_ring_expanded(window, frame, scale)?;
    let px_w = CGImage::width(Some(&framed)) as f64;
    let px_h = CGImage::height(Some(&framed)) as f64;
    let inner = picture_within_ring(px_w, px_h, scale);
    let image = CGImage::with_image_in_rect(Some(&framed), inner)?;
    let dressing = harvest_from_capture(&framed, frame.size, scale);
    Some(WindowSnapshot {
        image: SnapshotImage::Bitmap(image),
        coverage: Coverage {
            covered: (inner.size.width / scale, inner.size.height / scale),
            window: (frame.size.width, frame.size.height),
        },
        source: SnapshotSource::SkyLight,
        dressing,
        taken: std::time::Instant::now(),
    })
}

/// Anything the cache can hold and judge, so the replacement policy can be tested on plain sizes.
pub trait HasCoverage {
    fn coverage(&self) -> Coverage;
}

/// State a cache payload keeps alive across captures, independently of the pixel replacement rule.
/// The dressing comes from the screen composite, not the capture buffer, so it replaces on its own.
pub trait CarriesOver: Sized {
    /// Called on an incoming payload that is about to replace `previous`.
    fn inherit(&mut self, _previous: &Self) {}
    /// Called on the kept payload when `refused` lost to the replacement rule.
    fn absorb(&mut self, _refused: Self) {}
}

impl HasCoverage for WindowSnapshot {
    fn coverage(&self) -> Coverage {
        self.coverage
    }
}

impl CarriesOver for WindowSnapshot {
    fn inherit(&mut self, previous: &Self) {
        self.dressing = dressing_after_insert(previous.dressing.clone(), self.dressing.take());
    }

    fn absorb(&mut self, refused: Self) {
        self.dressing = dressing_after_insert(self.dressing.take(), refused.dressing);
    }
}

impl HasCoverage for Coverage {
    fn coverage(&self) -> Coverage {
        *self
    }
}

impl CarriesOver for Coverage {}

/// Captures several windows as one composited image, for the backdrop.
pub fn capture_composite_via_skylight(
    windows: &[WindowServerId],
    covers: (f64, f64),
    scale: f64,
) -> Option<WindowSnapshot> {
    if windows.is_empty() {
        return None;
    }
    let cid = unsafe { SLSMainConnectionID() };
    let ids: Vec<u32> = windows.iter().map(|w| w.as_u32()).collect();
    let array: *mut CFArray<CGImage> = unsafe {
        SLSHWCaptureWindowList(cid, ids.as_ptr(), ids.len() as c_int, CAPTURE_OPTIONS)
    };
    if array.is_null() {
        return None;
    }
    // SAFETY: SLSHWCaptureWindowList returns a +1 CFArray of CGImage, so ownership transfers here.
    let array = unsafe { CFRetained::from_raw(std::ptr::NonNull::new(array)?) };
    let image = array.iter().next()?;
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let px_w = CGImage::width(Some(&image)) as f64;
    let px_h = CGImage::height(Some(&image)) as f64;
    Some(WindowSnapshot {
        image: SnapshotImage::Bitmap(image),
        coverage: Coverage { covered: (px_w / scale, px_h / scale), window: covers },
        source: SnapshotSource::SkyLight,
        dressing: None,
        taken: std::time::Instant::now(),
    })
}

/// Snapshots held per window, so a switch can composite without capturing anything synchronously.
/// Keyed by [`WindowId`] because window server ids are recycled when a window closes and reopens.
pub struct SnapshotCache<T = WindowSnapshot> {
    entries: HashMap<WindowId, T>,
}

impl<T: HasCoverage> Default for SnapshotCache<T> {
    fn default() -> Self {
        Self { entries: HashMap::new() }
    }
}

impl<T: HasCoverage + CarriesOver> SnapshotCache<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores a snapshot, subject to [`should_replace`] for its pixels, with [`CarriesOver`]
    /// deciding what survives of the rest either way.
    pub fn insert(&mut self, window: WindowId, mut snapshot: T) {
        if let Some(existing) = self.entries.get_mut(&window) {
            if !should_replace(Some(existing.coverage()), snapshot.coverage()) {
                existing.absorb(snapshot);
                return;
            }
            snapshot.inherit(existing);
        }
        self.entries.insert(window, snapshot);
    }

    pub fn get(&self, window: WindowId) -> Option<&T> {
        self.entries.get(&window)
    }

    pub fn get_mut(&mut self, window: WindowId) -> Option<&mut T> {
        self.entries.get_mut(&window)
    }

    /// The snapshot to actually draw, or `None` if what we hold is not worth drawing.
    pub fn usable(&self, window: WindowId) -> Option<&T> {
        self.entries.get(&window).filter(|s| s.coverage().is_usable())
    }

    pub fn forget(&mut self, window: WindowId) {
        self.entries.remove(&window);
    }

    /// Drops snapshots for windows rini no longer manages.
    pub fn retain_only(&mut self, live: &dyn Fn(WindowId) -> bool) {
        self.entries.retain(|wid, _| live(*wid));
    }

    /// Every cached entry, for the debug dump.
    pub fn iter(&self) -> impl Iterator<Item = (&WindowId, &T)> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A 1x1 bitmap for tests that need a real `CGImage` but never read its pixels.
#[cfg(test)]
pub(crate) fn test_bitmap() -> CFRetained<CGImage> {
    use objc2_core_graphics::{CGBitmapInfo, CGColorSpace, CGDataProvider, CGImageAlphaInfo};
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

/// A usable snapshot that claims to cover a window of `size`, for tests about geometry.
#[cfg(test)]
pub(crate) fn test_snapshot(size: CGSize) -> WindowSnapshot {
    WindowSnapshot {
        image: SnapshotImage::Bitmap(test_bitmap()),
        coverage: Coverage {
            covered: (size.width, size.height),
            window: (size.width, size.height),
        },
        source: SnapshotSource::ScreenCaptureKit,
        dressing: None,
        taken: std::time::Instant::now(),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    fn wid(idx: u32) -> WindowId {
        WindowId { pid: 1, idx: NonZeroU32::new(idx).unwrap() }
    }

    fn coverage(covered: (f64, f64), window: (f64, f64)) -> Coverage {
        Coverage { covered, window }
    }

    fn cache() -> SnapshotCache<Coverage> {
        SnapshotCache::new()
    }

    /// Stands in for `WindowSnapshot` and its dressing without needing a bitmap.
    #[derive(Clone, Copy)]
    struct Dressed {
        coverage: Coverage,
        dressing: Option<u32>,
    }

    impl HasCoverage for Dressed {
        fn coverage(&self) -> Coverage {
            self.coverage
        }
    }

    impl CarriesOver for Dressed {
        fn inherit(&mut self, previous: &Self) {
            self.dressing =
                crate::edge_dressing::dressing_after_insert(previous.dressing, self.dressing);
        }

        fn absorb(&mut self, refused: Self) {
            self.dressing =
                crate::edge_dressing::dressing_after_insert(self.dressing, refused.dressing);
        }
    }

    #[test]
    fn a_refused_capture_still_delivers_its_dressing() {
        let mut cache: SnapshotCache<Dressed> = SnapshotCache::new();
        let good = coverage((859.0, 1081.0), (859.0, 1081.0));
        let sliver = coverage((40.0, 1081.0), (859.0, 1081.0));
        cache.insert(wid(1), Dressed { coverage: good, dressing: Some(1) });
        cache.insert(wid(1), Dressed { coverage: sliver, dressing: Some(2) });
        let held = cache.get(wid(1)).unwrap();
        assert_eq!(held.coverage.covered.0, 859.0, "the pixels were refused");
        assert_eq!(held.dressing, Some(2), "but the fresher ring was kept");
    }

    #[test]
    fn an_accepted_capture_without_a_harvest_inherits_the_worn_dressing() {
        let mut cache: SnapshotCache<Dressed> = SnapshotCache::new();
        let good = coverage((859.0, 1081.0), (859.0, 1081.0));
        cache.insert(wid(1), Dressed { coverage: good, dressing: Some(1) });
        cache.insert(wid(1), Dressed { coverage: good, dressing: None });
        assert_eq!(cache.get(wid(1)).unwrap().dressing, Some(1));
    }

    #[test]
    fn cache_applies_the_no_downgrade_rule() {
        let mut cache = cache();
        cache.insert(wid(1), coverage((859.0, 1081.0), (859.0, 1081.0)));
        cache.insert(wid(1), coverage((40.0, 1081.0), (859.0, 1081.0)));
        assert_eq!(cache.get(wid(1)).unwrap().covered.0, 859.0);
        assert!(cache.usable(wid(1)).is_some());
    }

    #[test]
    fn cache_holds_a_sliver_but_reports_it_as_not_worth_drawing() {
        let mut cache = cache();
        cache.insert(wid(1), coverage((40.0, 1081.0), (859.0, 1081.0)));
        assert!(cache.get(wid(1)).is_some(), "held");
        assert!(cache.usable(wid(1)).is_none(), "but not drawable");
    }

    #[test]
    fn forget_drops_one_window() {
        let mut cache = cache();
        cache.insert(wid(1), coverage((859.0, 1081.0), (859.0, 1081.0)));
        cache.forget(wid(1));
        assert!(cache.is_empty());
    }

    #[test]
    fn retain_only_drops_windows_that_are_gone() {
        let mut cache = cache();
        cache.insert(wid(1), coverage((859.0, 1081.0), (859.0, 1081.0)));
        cache.insert(wid(2), coverage((859.0, 1081.0), (859.0, 1081.0)));
        cache.retain_only(&|w| w == wid(1));
        assert_eq!(cache.len(), 1);
        assert!(cache.get(wid(1)).is_some());
        assert!(cache.get(wid(2)).is_none());
    }
}
