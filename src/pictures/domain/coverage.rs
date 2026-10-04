//! When a picture covers its window and fits the frame it is drawn into. Sizes only, so every rule
//! here is testable without an image. Measurements behind the tolerances are in
//! `src/pictures/docs/capture-research.md`; the stretch tolerance is `rini_geometry::stretch`.

use std::time::Duration;

use rini_geometry::space::{PointSpace, Respace, ScreenSize, Size};
use rini_geometry::stretch::{MAX_STRETCH, within_stretch};

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
    /// The size the captured pixels cover, which is NOT always the window's own size: a
    /// SkyLight capture of a partly visible window covers only the visible part.
    pub covered: ScreenSize,
    /// The window's full size at capture time, for comparison against `covered`.
    pub window: ScreenSize,
}

impl Coverage {
    /// Does this capture cover enough of the window to be worth drawing?
    ///
    /// A clipped capture is not merely lower quality, it is wrong: stretching a 40pt sliver across
    /// 859pt produces a smear that reads as a rendering bug.
    pub fn is_usable(&self) -> bool {
        if self.window.width <= 0.0 || self.window.height <= 0.0 {
            return false;
        }
        let wide = self.covered.width / self.window.width;
        let tall = self.covered.height / self.window.height;
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

/// Can this picture be drawn at `frame` without visibly distorting it?
///
/// Distinct from [`Coverage::is_usable`], which compares a capture against the window it was taken
/// FROM. This compares it against the frame it is drawn INTO, which a cached picture routinely no
/// longer matches. See "A capture can be usable and still be the wrong shape" in
/// `src/pictures/docs/capture-research.md`.
pub fn fits_frame<U: PointSpace>(covered: ScreenSize, frame: Size<U>) -> bool {
    within_stretch(covered, frame.respace())
}

/// Whether `size` needs pixels this picture does not have: bigger than the capture in either
/// axis, beyond the stretch tolerance.
///
/// The grow-vs-shrink asymmetry of the crop-drawn resize: a shrink crops the picture it has,
/// which is truthful; a grow needs content that does not exist until the app renders at the new
/// size, so it holds for a fresh capture (see "Resizes through the overlay" in
/// `src/animation/docs/animation-smoothness.md`).
pub fn outgrows<U: PointSpace>(covered: ScreenSize, size: Size<U>) -> bool {
    size.width > covered.width * MAX_STRETCH || size.height > covered.height * MAX_STRETCH
}

/// Whether a window needs a fresh capture before it can be drawn at `size`.
///
/// Having a drawable picture is not enough: it also has to match the size the window is now. A window
/// resized from 859pt to 1147pt keeps a perfectly usable 859pt picture, and skipping it on that basis left
/// it permanently the wrong shape, so every animation either stretched it or dropped it.
pub fn needs_capture<U: PointSpace>(cached: Option<Coverage>, size: Size<U>) -> bool {
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
/// keep. See "The wallpaper is not reliably a window" in `src/pictures/docs/capture-research.md`.
pub fn is_backdrop_worth_drawing(
    have_one_already: bool,
    has_wallpaper: bool,
    covered: ScreenSize,
    display: ScreenSize,
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
pub fn spans_display(covered: ScreenSize, display: ScreenSize) -> bool {
    (covered.width - display.width).abs() <= BACKDROP_SIZE_TOLERANCE
        && (covered.height - display.height).abs() <= BACKDROP_SIZE_TOLERANCE
}

/// How old a fitting picture may grow before a warm re-captures it anyway.
///
/// One flight of staleness at most under continuous use, without re-capturing everything on
/// every keystroke: the service already dedups in-flight requests, so the steady-state cost is
/// one capture per animated window per interval.
const PICTURE_STALE_AFTER: Duration = Duration::from_secs(2);

/// Whether a cached picture's age alone justifies a fresh capture.
pub fn picture_is_stale(age: Duration) -> bool {
    age > PICTURE_STALE_AFTER
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: f64, height: f64) -> ScreenSize { ScreenSize::new(width, height) }

    fn coverage(covered: (f64, f64), window: (f64, f64)) -> Coverage {
        Coverage {
            covered: size(covered.0, covered.1),
            window: size(window.0, window.1),
        }
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
        assert!(!spans_display(size(3008.0, 1692.0), size(1728.0, 1117.0)));
        assert!(!spans_display(size(1728.0, 1117.0), size(3008.0, 1692.0)));
    }

    #[test]
    fn a_picture_of_this_display_spans_it_including_a_point_of_rounding() {
        assert!(spans_display(size(1728.0, 1117.0), size(1728.0, 1117.0)));
        assert!(spans_display(size(1729.0, 1116.0), size(1728.0, 1117.0)));
        assert!(
            !spans_display(size(1725.0, 1117.0), size(1728.0, 1117.0)),
            "3pt short is not the display"
        );
    }

    /// Fresh pictures are left alone — re-capturing every animated window per keystroke is churn —
    /// while anything older than the threshold re-warms even though it still fits.
    #[test]
    fn a_picture_goes_stale_by_age_alone() {
        assert!(!picture_is_stale(Duration::from_millis(500)));
        assert!(picture_is_stale(Duration::from_secs(3)));
    }

    /// A grow needs pixels the picture does not have; a shrink or a match never does. This is
    /// what decides whether a resize animates immediately (crop) or holds for a fresh capture
    /// (reveal).
    #[test]
    fn a_grow_outgrows_its_picture_and_a_shrink_does_not() {
        let picture = size(859.0, 1081.0);
        assert!(outgrows(picture, size(1147.0, 1081.0)), "wider");
        assert!(outgrows(picture, size(859.0, 1300.0)), "taller");
        assert!(!outgrows(picture, size(859.0, 1081.0)), "same");
        assert!(!outgrows(picture, size(572.0, 1081.0)), "narrower");
        // Rounding is not a grow, same tolerance as everything else here.
        assert!(!outgrows(picture, size(861.0, 1081.0)));
    }

    #[test]
    fn a_window_with_no_picture_needs_one() {
        assert!(needs_capture(None, size(859.0, 1081.0)));
    }

    /// The measured case: a strip re-fit widened a window from 859pt to 1147pt and its picture was never
    /// refreshed, so it was dropped from every animation as the wrong shape and visibly vanished.
    #[test]
    fn a_resized_window_needs_a_new_picture_even_though_the_old_one_is_usable() {
        let old = coverage((859.0, 1081.0), (859.0, 1081.0));
        assert!(
            old.is_usable(),
            "the old picture is perfectly good for the old size"
        );
        assert!(needs_capture(Some(old), size(1147.0, 1081.0)));
    }

    #[test]
    fn a_picture_that_still_fits_needs_nothing() {
        let current = coverage((1147.0, 1081.0), (1147.0, 1081.0));
        assert!(!needs_capture(Some(current), size(1147.0, 1081.0)));
        // Rounding is not a resize, the same tolerance the rest of the overlay uses.
        assert!(!needs_capture(Some(current), size(1146.0, 1081.0)));
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

    /// The measured failure: the composite came back with the icons and widgets but no wallpaper, and
    /// drew as a black screen for the whole animation.
    #[test]
    fn a_desktop_capture_missing_its_wallpaper_is_rejected() {
        assert!(!is_backdrop_worth_drawing(
            true,
            false,
            size(1728.0, 1117.0),
            size(1728.0, 1117.0)
        ));
    }

    #[test]
    fn a_desktop_capture_with_its_wallpaper_is_drawn() {
        assert!(is_backdrop_worth_drawing(
            true,
            true,
            size(1728.0, 1117.0),
            size(1728.0, 1117.0)
        ));
    }

    #[test]
    fn a_wallpaperless_capture_is_still_drawn_when_there_is_nothing_to_keep() {
        // Rejecting it would leave the bare black window, which is worse than a desktop with no photo.
        assert!(is_backdrop_worth_drawing(
            false,
            false,
            size(1728.0, 1117.0),
            size(1728.0, 1117.0)
        ));
    }

    #[test]
    fn a_desktop_capture_shorter_than_the_display_is_rejected() {
        // Drawn from the top-left at its own size, so the rest of the screen stays black and the
        // captured bar strip lands partway up the display.
        assert!(!is_backdrop_worth_drawing(
            true,
            true,
            size(1728.0, 1085.0),
            size(1728.0, 1117.0)
        ));
        assert!(!is_backdrop_worth_drawing(
            false,
            true,
            size(1728.0, 1085.0),
            size(1728.0, 1117.0)
        ));
    }

    #[test]
    fn a_desktop_capture_spanning_more_than_the_display_is_rejected() {
        // What a composite of two displays' desktops measures, which cannot be drawn as one backdrop.
        assert!(!is_backdrop_worth_drawing(
            true,
            true,
            size(3456.0, 1117.0),
            size(1728.0, 1117.0)
        ));
    }

    #[test]
    fn a_desktop_capture_a_rounding_error_short_is_still_drawn() {
        assert!(is_backdrop_worth_drawing(
            true,
            true,
            size(1727.5, 1116.5),
            size(1728.0, 1117.0)
        ));
    }

    /// The measured failure: a full-width capture drawn into a half-width frame, squashed to fill.
    #[test]
    fn a_picture_of_the_wrong_shape_does_not_fit_its_frame() {
        assert!(!fits_frame(size(1720.0, 1081.0), size(859.0, 1081.0)));
        assert!(!fits_frame(size(1720.0, 1081.0), size(1499.0, 1656.0)));
    }

    #[test]
    fn a_picture_of_the_right_shape_fits() {
        assert!(fits_frame(size(859.0, 1081.0), size(859.0, 1081.0)));
    }

    #[test]
    fn a_picture_a_rounding_error_off_still_fits() {
        // Captures land a pixel or two short, which is half a point at 2x, and rejecting those would
        // leave almost every window undrawn.
        assert!(fits_frame(size(858.5, 1080.5), size(859.0, 1081.0)));
    }

    #[test]
    fn a_picture_off_by_a_few_points_does_not_fit() {
        // Small enough to look like a near miss, large enough to shift the contents visibly.
        assert!(!fits_frame(size(820.0, 1081.0), size(859.0, 1081.0)));
    }

    #[test]
    fn a_zero_sized_frame_never_fits_rather_than_dividing_by_zero() {
        assert!(!fits_frame(size(859.0, 1081.0), size(0.0, 0.0)));
    }

    /// A capture can cover its own window exactly and still be the wrong shape for where it is drawn.
    /// This is what `is_usable` cannot express, and why both checks exist.
    #[test]
    fn a_usable_capture_can_still_not_fit_the_frame_it_is_drawn_into() {
        let full_width = coverage((1720.0, 1081.0), (1720.0, 1081.0));
        assert!(full_width.is_usable(), "it covers the window it was taken from");
        assert!(
            !fits_frame(full_width.covered, size(859.0, 1081.0)),
            "but not the frame it goes into"
        );
    }
}
