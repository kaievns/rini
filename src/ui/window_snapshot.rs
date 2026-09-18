//! Bitmap snapshots of windows, for the capture-based animation overlay.
//!
//! Two capture APIs, because neither is sufficient alone: SkyLight for what is on screen, captured
//! fresh, and ScreenCaptureKit for everything else, served from a background cache.
//!
//! Constraints and costs of both are measured in `docs/capture-overlay-research.md`.

use std::collections::HashMap;
use std::ffi::c_int;

use objc2_core_foundation::{CFArray, CFRetained, CGSize};
use objc2_core_graphics::CGImage;
use objc2_io_surface::IOSurfaceRef;

use rini_shared::ids::WindowId;
use rini_macos::skylight::{SLSHWCaptureWindowList, SLSMainConnectionID};
use crate::ui::edge_dressing::dressing_after_insert;
use rini_shared::ids::WindowServerId;

pub use rini_motion::fit::{
    Coverage, fits_frame, is_a_resize, is_backdrop_worth_drawing, needs_capture, outgrows,
    picture_is_stale, should_replace, spans_display,
};

/// Capture options cribbed verbatim from yabai (`window_manager.c:521`). The bits are undocumented,
/// so they are not named: 1 << 11 asks for nominal resolution and 1 << 8 for best. Measured to make
/// no difference to the visible-portion clipping, but kept identical to the one implementation known
/// to work in production.
const CAPTURE_OPTIONS: u32 = (1 << 11) | (1 << 8);

/// A window's pixels, whichever API produced them.
///
/// Two cases rather than one normalised form: Core Animation accepts either, and converting would cost
/// exactly what each API is good at avoiding. Surfaces are preferred because they stay off the heap.
#[derive(Clone, Debug)]
pub enum SnapshotImage {
    /// CPU-side bitmap, from `SLSHWCaptureWindowList`.
    Bitmap(CFRetained<CGImage>),
    /// GPU-side surface, from ScreenCaptureKit. Does not occupy the process's heap.
    Surface(CFRetained<IOSurfaceRef>),
}

// IOSurface is explicitly shareable across threads and processes, and the ScreenCaptureKit capture
// completes on a background queue. The retained reference keeps it alive until the main thread
// attaches it to a layer.
unsafe impl Send for SnapshotImage {}

/// A window's pixels, plus how much of the window they cover.
#[derive(Clone, Debug)]
pub struct WindowSnapshot {
    pub image: SnapshotImage,
    pub coverage: Coverage,
    pub source: SnapshotSource,
    /// The window-server hairline this window wore when last seen composited, or `None` if it has
    /// not been harvested yet. Carried across cache refreshes by [`SnapshotCache::insert`]: see
    /// [`crate::ui::edge_dressing`].
    pub dressing: Option<crate::ui::edge_dressing::EdgeDressing>,
    /// When the pixels were captured. Staleness is a reason to re-warm: a fitting picture used to
    /// be kept forever, so an off-strip window's tile showed week-old content on every animation
    /// and snapped to the live window at each handover.
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

/// Captures one window from the framebuffer through SkyLight.
///
/// One window per call: a list returns a single flattened composite, which cannot drive per-window
/// animation. `None` is normal, including whenever the display is asleep, and callers fall back to the
/// cache rather than treating it as an error.
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

/// Captures one window through the framed legacy API, at whatever size it really is right now.
///
/// The reveal chase's capture: measured at 16-24ms against 170-300ms for the SkyLight route under
/// load. The image carries the window-server hairline baked into its outermost point — harmless,
/// because the dressing sublayers draw the same pixels over it. Only works for a window that is
/// actually composited on screen; the chase only calls it for one that is.
pub fn capture_via_framed(window: WindowServerId, scale: f64) -> Option<WindowSnapshot> {
    let frame = rini_macos::window_server::get_window(window)?.frame;
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

/// One framed capture that yields the picture AND its hairline: the window plus one ring, the
/// picture cropped back out of the same pixels. The reveal chase's capture: one window-server
/// call per attempt. See "A grow holds, then reveals" in `docs/animation-smoothness.md`.
pub fn capture_via_framed_with_dressing(
    window: WindowServerId,
    scale: f64,
) -> Option<WindowSnapshot> {
    use crate::ui::edge_dressing::{
        capture_ring_expanded, harvest_from_capture, picture_within_ring,
    };
    let frame = rini_macos::window_server::get_window(window)?.frame;
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

/// Anything the cache can hold and judge. Exists so the cache's replacement policy can be tested
/// against plain sizes, without constructing bitmaps for a rule that never looks at pixels.
pub trait HasCoverage {
    fn coverage(&self) -> Coverage;
}

/// State a cache payload keeps alive across captures, independently of the pixel replacement rule.
///
/// The hairline dressing is harvested from the screen composite rather than from the capture
/// buffer, so the two replace independently: a clipped capture can carry a good ring, and a good
/// capture of a parked window carries none. Both hooks default to nothing for payloads that carry
/// nothing.
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

/// Captures several windows as ONE composited image.
///
/// The one case where SkyLight's flattening is wanted: a backdrop needs the wallpaper and the icon
/// layer in a single picture.
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
    // SAFETY: returns a +1 CFArray of CGImage, so ownership transfers here.
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
///
/// Keyed by [`WindowId`] rather than [`WindowServerId`] because window server ids are recycled when
/// a window is closed and reopened, which would serve one window's pixels for another.
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

    /// Drops snapshots for windows rini no longer manages, so the cache cannot outgrow the window
    /// set. Each entry holds a full-resolution bitmap, so a leak here is measured in tens of MB.
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

    /// The cache is exercised with `Coverage` as its payload. Every rule it enforces keys off sizes,
    /// so a bitmap would add nothing but the need for graphics features in a unit test.
    fn cache() -> SnapshotCache<Coverage> {
        SnapshotCache::new()
    }

    /// A payload with carried state, standing in for `WindowSnapshot` and its dressing: the same
    /// `dressing_after_insert` rule, without needing a bitmap.
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
                crate::ui::edge_dressing::dressing_after_insert(previous.dressing, self.dressing);
        }

        fn absorb(&mut self, refused: Self) {
            self.dressing =
                crate::ui::edge_dressing::dressing_after_insert(self.dressing, refused.dressing);
        }
    }

    #[test]
    fn a_refused_capture_still_delivers_its_dressing() {
        // The harvest reads the screen composite, not the capture buffer: a clipped capture of an
        // on-screen window carries a perfectly good ring, and dropping it with the pixels would
        // leave the tile wearing last week's hairline.
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
        // A window captured while parked harvests nothing; it keeps the ring from when it was last
        // composited, the same staleness model as the pictures themselves.
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
        // Each entry holds a full-resolution bitmap, so failing to prune leaks tens of MB.
        let mut cache = cache();
        cache.insert(wid(1), coverage((859.0, 1081.0), (859.0, 1081.0)));
        cache.insert(wid(2), coverage((859.0, 1081.0), (859.0, 1081.0)));
        cache.retain_only(&|w| w == wid(1));
        assert_eq!(cache.len(), 1);
        assert!(cache.get(wid(1)).is_some());
        assert!(cache.get(wid(2)).is_none());
    }
}
