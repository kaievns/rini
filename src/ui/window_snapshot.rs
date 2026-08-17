//! Bitmap snapshots of windows, for the capture-based animation overlay.
//!
//! The animation overlay draws pictures of windows rather than moving the real ones. Moving real
//! windows means one `AXPosition` write per window per frame, each a synchronous request into a
//! different process, and those processes answer at different speeds. That is the cross-app tear.
//! Drawing pictures replaces N unsynchronised writes with one composited frame that cannot tear.
//!
//! Two capture APIs exist and neither is sufficient alone, so this module uses both. The measured
//! constraints are recorded in `docs/capture-overlay-research.md`; the short version:
//!
//! - `SLSHWCaptureWindowList` is cheap, about 16ms per window, and needs only the Screen Recording
//!   grant. But it captures the FRAMEBUFFER, so it returns only the visible portion of a window. A
//!   window scrolled to a 40pt sliver comes back 40pt wide, and one on a hidden workspace comes back
//!   1x28. Passing several window ids returns a single flattened composite rather than one image per
//!   window, so it cannot be batched for per-window animation either.
//! - ScreenCaptureKit returns the window's own surface at full size whatever its visibility, which
//!   is the only way to get pixels for a window that is about to slide in. It costs about 40ms fixed
//!   plus 14.5ms per window, far too slow to run at switch time.
//!
//! Hence: SkyLight for what is on screen, captured fresh, and ScreenCaptureKit for everything else,
//! served from a cache refreshed in the background. Staleness only ever lands on windows that are
//! currently a sliver, where it cannot be seen.

use std::collections::HashMap;
use std::ffi::c_int;

use objc2_core_foundation::{CFArray, CFRetained};
use objc2_core_graphics::CGImage;
use objc2_io_surface::IOSurfaceRef;

use crate::actor::app::WindowId;
use crate::sys::skylight::{SLSHWCaptureWindowList, SLSMainConnectionID};
use crate::sys::window_server::WindowServerId;

/// Capture options cribbed verbatim from yabai (`window_manager.c:521`). The bits are undocumented,
/// so they are not named: 1 << 11 asks for nominal resolution and 1 << 8 for best. Measured to make
/// no difference to the visible-portion clipping, but kept identical to the one implementation known
/// to work in production.
const CAPTURE_OPTIONS: u32 = (1 << 11) | (1 << 8);

/// How much smaller than its real size a capture may be before it is not worth drawing.
///
/// A layer draws its contents stretched to fill, so a capture covering less than the whole window is
/// scaled up when drawn, and the picture no longer lines up with where the real window is. Measured
/// on a live switch: accepting 92% coverage put the animation 13.5pt off vertically, which read as
/// the whole animation being misaligned and jumpy.
///
/// So this is deliberately strict. 0.995 rather than exactly 1.0 only because captures land a pixel
/// or two short from rounding, which is half a point at 2x. Anything genuinely clipped is left to
/// ScreenCaptureKit, which returns the window's own surface at full size.
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
/// The rule that matters: never downgrade a usable capture to a clipped one. Without it, a
/// background refresh that happens to run while a window is scrolled off-strip overwrites good
/// pixels with a sliver, and that window animates as a smear from then on. A clipped capture is
/// still accepted when there is nothing better, since the caller can decline to draw it and a later
/// refresh can upgrade it.
pub fn should_replace(existing: Option<Coverage>, incoming: Coverage) -> bool {
    match existing {
        Some(existing) if existing.is_usable() && !incoming.is_usable() => false,
        _ => true,
    }
}

/// A window's pixels, whichever API produced them.
///
/// Kept as two cases rather than normalised to one, because converting between them costs exactly
/// what each API is good at avoiding. A `CGImage` from SkyLight is already a CPU-side bitmap, and an
/// `IOSurface` from ScreenCaptureKit already lives where the compositor wants it. Core Animation
/// accepts either as layer contents, so there is nothing to gain by picking one.
///
/// The difference matters for memory. A single 859x1081pt window at 2x is 1718x2162 pixels, which is
/// about 14.9MB as a CPU bitmap. Holding twenty of those would be roughly 280MB of resident memory
/// for a window manager, which is why the cache prefers surfaces.
#[derive(Clone)]
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
#[derive(Clone)]
pub struct WindowSnapshot {
    pub image: SnapshotImage,
    pub coverage: Coverage,
    pub source: SnapshotSource,
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
}

/// Captures one window from the framebuffer through SkyLight.
///
/// Deliberately one window per call. Passing a list returns a single flattened composite of all of
/// them, which cannot drive per-window animation. yabai passes a count of 1 for the same reason.
///
/// Returns `None` when the window server declines, which happens for a window with no backing
/// store and, notably, whenever the display is asleep. Callers must treat `None` as normal and fall
/// back to the cache rather than reporting an error.
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
    })
}

/// Anything the cache can hold and judge. Exists so the cache's replacement policy can be tested
/// against plain sizes, without constructing bitmaps for a rule that never looks at pixels.
pub trait HasCoverage {
    fn coverage(&self) -> Coverage;
}

impl HasCoverage for WindowSnapshot {
    fn coverage(&self) -> Coverage {
        self.coverage
    }
}

impl HasCoverage for Coverage {
    fn coverage(&self) -> Coverage {
        *self
    }
}

/// Captures several windows as ONE composited image.
///
/// Used for the desktop backdrop, where the wallpaper and the icon layer have to end up in a single
/// picture. This is the one case where SkyLight's batching behaviour is what is wanted: passing a list
/// returns a single flattened composite rather than one image per window, which is useless for
/// animating windows separately but exactly right for a backdrop.
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

impl<T: HasCoverage> SnapshotCache<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores a snapshot, subject to [`should_replace`].
    pub fn insert(&mut self, window: WindowId, snapshot: T) {
        let existing = self.entries.get(&window).map(|s| s.coverage());
        if !should_replace(existing, snapshot.coverage()) {
            return;
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
