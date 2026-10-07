//! Which of the window server's windows a listing of what is on a display names.

use objc2_core_foundation::CGRect;
use rini_core::ids::pid_t;

/// Below this in either axis a window is too small to be worth drawing.
const MIN_SIDE: f64 = 100.0;

/// Whether a window of `layer`, owned by `owner`, at `bounds`, is listed as on `display`: a layer-0
/// window of another process, 100pt or more each way, overlapping it. Other layers hold the bar, the
/// Dock and notifications; rini's own windows (`own`) are the decorations it draws, never pictured. A
/// window parked at the corner is listed: it keeps its full size, and the sliver it shows is on
/// screen.
pub fn listed(layer: Option<i64>, owner: Option<pid_t>, own: pid_t, bounds: CGRect, display: CGRect) -> bool {
    layer == Some(0)
        && owner != Some(own)
        && bounds.size.width >= MIN_SIDE
        && bounds.size.height >= MIN_SIDE
        && overlaps(bounds, display)
}

/// Whether two rects share an area, edges that merely touch excluded.
pub fn overlaps(a: CGRect, b: CGRect) -> bool {
    a.origin.x < b.origin.x + b.size.width
        && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height
        && b.origin.y < a.origin.y + a.size.height
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    #[test]
    fn rinis_own_windows_are_never_listed() {
        let display = rect(0.0, 32.0, 1728.0, 1085.0);
        let window = rect(865.0, 32.0, 859.0, 1081.0);
        assert!(listed(Some(0), Some(500), 42, window, display));
        assert!(!listed(Some(0), Some(42), 42, window, display), "a rest window rini draws");
        assert!(listed(Some(0), None, 42, window, display), "an owner unreported is another's");
        assert!(!listed(Some(18), Some(500), 42, window, display), "off layer 0");
        assert!(!listed(Some(0), Some(500), 42, rect(865.0, 32.0, 99.0, 1081.0), display), "too narrow");
        assert!(!listed(Some(0), Some(500), 42, rect(1728.0, 32.0, 859.0, 1081.0), display), "the next display's");
    }

    #[test]
    fn overlapping_rects_overlap() {
        assert!(overlaps(
            rect(0.0, 0.0, 100.0, 100.0),
            rect(50.0, 50.0, 100.0, 100.0)
        ));
    }

    #[test]
    fn a_window_fully_inside_a_display_overlaps_it() {
        let display = rect(0.0, 32.0, 1728.0, 1085.0);
        assert!(overlaps(rect(865.0, 32.0, 859.0, 1081.0), display));
    }

    #[test]
    fn a_window_scrolled_entirely_off_the_left_does_not_overlap() {
        // Real measured geometry: off-strip columns sit at x = -1680 with width 1720, so their right
        // edge lands at 40 and they DO still touch the display. A window fully clear of it must not.
        let display = rect(0.0, 32.0, 1728.0, 1085.0);
        assert!(!overlaps(rect(-2000.0, 32.0, 859.0, 1081.0), display));
    }

    #[test]
    fn a_window_overlapping_by_its_parked_sliver_still_counts_as_overlapping() {
        // The parked case, which the caller filters on size rather than on overlap.
        let display = rect(0.0, 32.0, 1728.0, 1085.0);
        assert!(overlaps(rect(-1680.0, 32.0, 1720.0, 1081.0), display));
    }

    #[test]
    fn a_window_on_the_next_display_does_not_overlap_this_one() {
        let display = rect(0.0, 32.0, 1728.0, 1085.0);
        assert!(!overlaps(rect(1728.0, 32.0, 859.0, 1081.0), display));
    }

    #[test]
    fn merely_touching_edges_does_not_count_as_overlap() {
        // Exclusive comparison, so a window whose right edge is exactly the display's left edge
        // contributes no visible pixels and is excluded.
        let display = rect(0.0, 0.0, 100.0, 100.0);
        assert!(!overlaps(rect(-50.0, 0.0, 50.0, 100.0), display));
    }
}
