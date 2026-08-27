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

use crate::actor::app::WindowId;
use crate::sys::skylight::{SLSHWCaptureWindowList, SLSMainConnectionID};
use crate::ui::edge_dressing::dressing_after_insert;
use crate::sys::window_server::WindowServerId;

/// Capture options cribbed verbatim from yabai (`window_manager.c:521`). The bits are undocumented,
/// so they are not named: 1 << 11 asks for nominal resolution and 1 << 8 for best. Measured to make
/// no difference to the visible-portion clipping, but kept identical to the one implementation known
/// to work in production.
const CAPTURE_OPTIONS: u32 = (1 << 11) | (1 << 8);

/// How much smaller than its real size a capture may be before it is not worth drawing.
///
/// Strict, because contents stretch to fill and a clipped capture then sits visibly out of register
/// with the real window. Short of 1.0 only to absorb a pixel or two of rounding.
const MIN_USABLE_COVERAGE: f64 = 0.995;

/// How much of a window a capture actually covers.
///
/// Deliberately separate from the pixels. The rule for whether a capture is worth drawing is about
/// sizes alone, so keeping it here lets it be stated once and tested without building images.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Coverage {
    /// Size in points that the captured pixels cover, which is NOT always the window's own size: a
    /// SkyLight capture of a partly visible window covers only the visible part.
    pub covered: (f64, f64),
    /// The window's full size in points at capture time, for comparison against `covered`.
    pub window: (f64, f64),
}

impl Coverage {
    /// Does this capture cover enough of the window to be worth drawing?
    ///
    /// A clipped capture is not merely lower quality, it is wrong: stretching a 40pt sliver across
    /// 859pt produces a smear that reads as a rendering bug.
    pub fn is_usable(&self) -> bool {
        if self.window.0 <= 0.0 || self.window.1 <= 0.0 {
            return false;
        }
        let wide = self.covered.0 / self.window.0;
        let tall = self.covered.1 / self.window.1;
        wide >= MIN_USABLE_COVERAGE && tall >= MIN_USABLE_COVERAGE
    }
}

/// Should an incoming capture replace what is already cached?
///
/// Never downgrades a usable capture to a clipped one, but accepts a clipped one when there is
/// nothing better, since the caller can decline to draw it and a later refresh can upgrade it.
pub fn should_replace(existing: Option<Coverage>, incoming: Coverage) -> bool {
    match existing {
        Some(existing) if existing.is_usable() && !incoming.is_usable() => false,
        _ => true,
    }
}

/// How far a desktop capture may be from the display's size and still be drawable. A point or two of
/// rounding is normal; anything more means it does not describe this display.
const BACKDROP_SIZE_TOLERANCE: f64 = 2.0;

/// How far a picture may be from the size it is drawn at before the stretch is visible.
///
/// A layer's `contentsGravity` defaults to resize, so contents are stretched to fill the layer with no
/// regard for their own proportions. Half a percent is rounding; more than that is distortion.
const MAX_STRETCH: f64 = 1.005;
const MIN_STRETCH: f64 = 0.995;

/// Can this picture be drawn at `frame` without visibly distorting it?
///
/// Distinct from [`Coverage::is_usable`], which compares a capture against the window it was taken
/// FROM. This compares it against the frame it is drawn INTO, which a cached picture routinely no
/// longer matches. See "A capture can be usable and still be the wrong shape" in
/// `docs/capture-overlay-research.md`.
pub fn fits_frame(covered: (f64, f64), frame: (f64, f64)) -> bool {
    if frame.0 <= 0.0 || frame.1 <= 0.0 {
        return false;
    }
    let wide = covered.0 / frame.0;
    let tall = covered.1 / frame.1;
    (MIN_STRETCH..=MAX_STRETCH).contains(&wide) && (MIN_STRETCH..=MAX_STRETCH).contains(&tall)
}

/// Whether a window needs a fresh capture before it can be drawn at `size`.
///
/// Having a drawable picture is not enough: it also has to match the size the window is now. A window
/// resized from 859pt to 1147pt keeps a perfectly usable 859pt picture, and skipping it on that basis left
/// it permanently the wrong shape, so every animation either stretched it or dropped it.
pub fn needs_capture(cached: Option<Coverage>, size: (f64, f64)) -> bool {
    match cached {
        None => true,
        Some(coverage) => !fits_frame(coverage.covered, size),
    }
}

/// Whether a fresh desktop capture is worth drawing behind the moving strips.
///
/// Rejects a composite whose wallpaper window was missing, and one that does not span the display,
/// since either draws as a black screen for the length of an animation. Rejecting means keeping
/// whatever is already drawn, so a wallpaperless capture is still accepted when there is nothing to
/// keep. See "The wallpaper is not reliably a window" in `docs/capture-overlay-research.md`.
pub fn is_backdrop_worth_drawing(
    have_one_already: bool,
    has_wallpaper: bool,
    covered: (f64, f64),
    display: (f64, f64),
) -> bool {
    spans_display(covered, display) && (has_wallpaper || !have_one_already)
}

/// Whether a desktop picture is the size of the display it is about to be drawn on.
///
/// The backdrop layer is sized from the picture rather than from the overlay, which keeps it in register
/// with the real desktop. That only holds if the two agree: a picture of the EXTERNAL display, 3008x1692,
/// drawn on the built-in display's overlay is laid out at its own size, so only its top-left corner is
/// visible and the wallpaper looks zoomed in. That happened intermittently, because a render requested for
/// one display could land after the overlay had moved to the other.
pub fn spans_display(covered: (f64, f64), display: (f64, f64)) -> bool {
    (covered.0 - display.0).abs() <= BACKDROP_SIZE_TOLERANCE
        && (covered.1 - display.1).abs() <= BACKDROP_SIZE_TOLERANCE
}

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

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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

    #[test]
    fn full_size_capture_is_usable() {
        assert!(coverage((859.0, 1081.0), (859.0, 1081.0)).is_usable());
    }

    #[test]
    fn capture_a_couple_of_points_short_is_still_usable() {
        // Captures land a pixel or two off from rounding, and a window overlapped at the very edge
        // is still perfectly drawable. Rejecting these would discard almost every real capture.
        assert!(coverage((857.0, 1079.0), (859.0, 1081.0)).is_usable());
    }

    /// The measured case. A 3008x1692 render of the external display reached the built-in display's
    /// overlay, which sized the backdrop layer to the picture, so the wallpaper was drawn at its own size
    /// and only its corner was visible: the wallpaper appeared to zoom in for the length of an animation.
    #[test]
    fn a_picture_of_another_display_does_not_span_this_one() {
        assert!(!spans_display((3008.0, 1692.0), (1728.0, 1117.0)));
        assert!(!spans_display((1728.0, 1117.0), (3008.0, 1692.0)));
    }

    #[test]
    fn a_picture_of_this_display_spans_it_including_a_point_of_rounding() {
        assert!(spans_display((1728.0, 1117.0), (1728.0, 1117.0)));
        assert!(spans_display((1729.0, 1116.0), (1728.0, 1117.0)));
        assert!(!spans_display((1725.0, 1117.0), (1728.0, 1117.0)), "3pt short is not the display");
    }

    #[test]
    fn a_window_with_no_picture_needs_one() {
        assert!(needs_capture(None, (859.0, 1081.0)));
    }

    /// The measured case: a strip re-fit widened a window from 859pt to 1147pt and its picture was never
    /// refreshed, so it was dropped from every animation as the wrong shape and visibly vanished.
    #[test]
    fn a_resized_window_needs_a_new_picture_even_though_the_old_one_is_usable() {
        let old = coverage((859.0, 1081.0), (859.0, 1081.0));
        assert!(old.is_usable(), "the old picture is perfectly good for the old size");
        assert!(needs_capture(Some(old), (1147.0, 1081.0)));
    }

    #[test]
    fn a_picture_that_still_fits_needs_nothing() {
        let current = coverage((1147.0, 1081.0), (1147.0, 1081.0));
        assert!(!needs_capture(Some(current), (1147.0, 1081.0)));
        // Rounding is not a resize, the same tolerance the rest of the overlay uses.
        assert!(!needs_capture(Some(current), (1146.0, 1081.0)));
    }

    #[test]
    fn scrolled_off_strip_sliver_is_rejected() {
        // The measured shape of the problem: a window scrolled to a 40pt sliver captures 40pt wide.
        // Stretching that across 859pt is a smear, so it must not be drawn.
        assert!(!coverage((40.0, 1081.0), (859.0, 1081.0)).is_usable());
    }

    #[test]
    fn hidden_workspace_capture_is_rejected() {
        // Measured: a window on a workspace that is not showing captures as 1x28.
        assert!(!coverage((1.0, 28.0), (1147.0, 1081.0)).is_usable());
    }

    #[test]
    fn a_capture_clipped_only_vertically_is_rejected() {
        // Full width but short: a window overlapped along the bottom. Drawing it would stretch the
        // visible part downward, which looks like the window content shifted.
        assert!(!coverage((859.0, 300.0), (859.0, 1081.0)).is_usable());
    }

    #[test]
    fn zero_sized_window_is_rejected_rather_than_dividing_by_zero() {
        assert!(!coverage((0.0, 0.0), (0.0, 0.0)).is_usable());
    }

    #[test]
    fn a_usable_capture_is_never_downgraded_to_a_sliver() {
        let good = coverage((859.0, 1081.0), (859.0, 1081.0));
        let sliver = coverage((40.0, 1081.0), (859.0, 1081.0));
        assert!(!should_replace(Some(good), sliver));
    }

    #[test]
    fn a_sliver_is_accepted_when_nothing_is_cached() {
        let sliver = coverage((40.0, 1081.0), (859.0, 1081.0));
        assert!(should_replace(None, sliver));
    }

    #[test]
    fn a_sliver_is_upgraded_by_a_full_capture() {
        let sliver = coverage((40.0, 1081.0), (859.0, 1081.0));
        let good = coverage((859.0, 1081.0), (859.0, 1081.0));
        assert!(should_replace(Some(sliver), good));
    }

    #[test]
    fn a_fresh_full_capture_replaces_an_older_one() {
        // Refreshing good pixels with newer good pixels is the normal case and must not be blocked.
        let good = coverage((859.0, 1081.0), (859.0, 1081.0));
        assert!(should_replace(Some(good), good));
    }

    #[test]
    fn a_sliver_replaces_another_sliver() {
        // Neither is drawable, so there is nothing to protect, and the newer one is at least current.
        let a = coverage((40.0, 1081.0), (859.0, 1081.0));
        let b = coverage((2.0, 1081.0), (859.0, 1081.0));
        assert!(should_replace(Some(a), b));
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

    /// The measured failure: the composite came back with the icons and widgets but no wallpaper, and
    /// drew as a black screen for the whole animation.
    #[test]
    fn a_desktop_capture_missing_its_wallpaper_is_rejected() {
        assert!(!is_backdrop_worth_drawing(true, false, (1728.0, 1117.0), (1728.0, 1117.0)));
    }

    #[test]
    fn a_desktop_capture_with_its_wallpaper_is_drawn() {
        assert!(is_backdrop_worth_drawing(true, true, (1728.0, 1117.0), (1728.0, 1117.0)));
    }

    #[test]
    fn a_wallpaperless_capture_is_still_drawn_when_there_is_nothing_to_keep() {
        // Rejecting it would leave the bare black window, which is worse than a desktop with no photo.
        assert!(is_backdrop_worth_drawing(false, false, (1728.0, 1117.0), (1728.0, 1117.0)));
    }

    #[test]
    fn a_desktop_capture_shorter_than_the_display_is_rejected() {
        // Drawn from the top-left at its own size, so the rest of the screen stays black and the
        // captured bar strip lands partway up the display.
        assert!(!is_backdrop_worth_drawing(true, true, (1728.0, 1085.0), (1728.0, 1117.0)));
        assert!(!is_backdrop_worth_drawing(false, true, (1728.0, 1085.0), (1728.0, 1117.0)));
    }

    #[test]
    fn a_desktop_capture_spanning_more_than_the_display_is_rejected() {
        // What a composite of two displays' desktops measures, which cannot be drawn as one backdrop.
        assert!(!is_backdrop_worth_drawing(true, true, (3456.0, 1117.0), (1728.0, 1117.0)));
    }

    #[test]
    fn a_desktop_capture_a_rounding_error_short_is_still_drawn() {
        assert!(is_backdrop_worth_drawing(true, true, (1727.5, 1116.5), (1728.0, 1117.0)));
    }

    /// The measured failure: a full-width capture drawn into a half-width frame, squashed to fill.
    #[test]
    fn a_picture_of_the_wrong_shape_does_not_fit_its_frame() {
        assert!(!fits_frame((1720.0, 1081.0), (859.0, 1081.0)));
        assert!(!fits_frame((1720.0, 1081.0), (1499.0, 1656.0)));
    }

    #[test]
    fn a_picture_of_the_right_shape_fits() {
        assert!(fits_frame((859.0, 1081.0), (859.0, 1081.0)));
    }

    #[test]
    fn a_picture_a_rounding_error_off_still_fits() {
        // Captures land a pixel or two short, which is half a point at 2x, and rejecting those would
        // leave almost every window undrawn.
        assert!(fits_frame((858.5, 1080.5), (859.0, 1081.0)));
    }

    #[test]
    fn a_picture_off_by_a_few_points_does_not_fit() {
        // Small enough to look like a near miss, large enough to shift the contents visibly.
        assert!(!fits_frame((820.0, 1081.0), (859.0, 1081.0)));
    }

    #[test]
    fn a_zero_sized_frame_never_fits_rather_than_dividing_by_zero() {
        assert!(!fits_frame((859.0, 1081.0), (0.0, 0.0)));
    }

    /// A capture can cover its own window exactly and still be the wrong shape for where it is drawn.
    /// This is what `is_usable` cannot express, and why both checks exist.
    #[test]
    fn a_usable_capture_can_still_not_fit_the_frame_it_is_drawn_into() {
        let full_width = coverage((1720.0, 1081.0), (1720.0, 1081.0));
        assert!(full_width.is_usable(), "it covers the window it was taken from");
        assert!(!fits_frame(full_width.covered, (859.0, 1081.0)), "but not the frame it goes into");
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
