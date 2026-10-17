//! Pure geometry for the stack of workspace strips a vertical switch scrolls through, and the
//! strips a zoomed-out flight shows beside the one it moves.
//!
//! Every workspace is a horizontal strip. The strips are stacked in index order, one usable height
//! apart so each workspace's tiling area sits directly on the one above it, and a switch moves the
//! viewport rather than the strips: each strip keeps its own horizontal position and is pinned
//! vertically to its row. A look that zooms out draws the strips above and below a gap further
//! apart (`stack_gap`).

use rini_geometry::space::{
    Point, Respace, ScreenPoint, ScreenRect, SurfacePoint, SurfaceRect, Vector,
};

/// Longest a switch may be stretched for distance, as a multiple of the configured duration.
const MAX_DURATION_STRETCH: f64 = 2.5;

/// Where a window sits on the strip surface.
///
/// `row` is the workspace's position in the stack counted from the first one involved, not its absolute
/// index, so a switch only ever builds the rows between the two ends; a strip drawn beside the one a
/// flight moves is a row above it, `-1`, or below it, `1`.
pub fn strip_frame(
    frame: ScreenRect,
    display_origin: ScreenPoint,
    row: isize,
    row_pitch: f64,
) -> SurfaceRect {
    SurfaceRect::new(
        Point::new(
            frame.origin.x - display_origin.x,
            (frame.origin.y - display_origin.y) + row as f64 * row_pitch,
        ),
        frame.size.respace(),
    )
}

/// How far apart a look that zooms out draws the strips beside the one a flight moves, beyond the
/// usable height: `gap`, or the band the display leaves above or below the strip where that is
/// more, so nothing of a strip beside shows on the overlay at rest. `screen` is the usable frame,
/// `display` the display's bounds, and `top_band` what the overlay leaves clear at its top.
pub fn stack_gap(gap: f64, screen: ScreenRect, display: ScreenRect, top_band: f64) -> f64 {
    let above = screen.min().y - display.min().y - top_band;
    let below = display.max().y - screen.max().y;
    gap.max(above).max(below)
}

/// The strips beside the one at `index` of a stack of `count`: the one above it, row `-1`, and the
/// one below, row `1`, where there is one. The stack never wraps.
pub fn beside(index: usize, count: usize) -> Vec<(usize, isize)> {
    let above = index.checked_sub(1).map(|above| (above, -1));
    let below = (index + 1 < count).then_some((index + 1, 1));
    above.into_iter().chain(below).collect()
}

/// Whether any of `frame` shows on a display `viewport` big, with the strip zoomed out to `scale`
/// about the display's centre and pushed up to `reach` on screen either way, as a bounce carries it.
pub fn in_zoomed_view(frame: SurfaceRect, viewport: SurfaceRect, scale: f64, reach: f64) -> bool {
    if !scale.is_finite() || scale <= 0.0 {
        return false;
    }
    let half = Vector::new(
        viewport.size.width + 2.0 * reach,
        viewport.size.height + 2.0 * reach,
    ) / (2.0 * scale);
    let centre = viewport.center();
    let seen = SurfaceRect::new(centre - half, (half * 2.0).to_size());
    frame.intersects(&seen)
}

/// Whether a window will be on screen when a switch finishes.
///
/// A switch travels only in y, so the destination row has to be laid out at a scroll that already shows the
/// window being switched to. Otherwise the strip pans sideways afterwards to reveal it, which is the second
/// movement a switch exists to avoid. The viewport ends at strip-surface x = 0, so the band is the display's width.
pub fn lands_in_view(frame: SurfaceRect, viewport_width: f64) -> bool {
    frame.origin.x < viewport_width && frame.origin.x + frame.size.width > 0.0
}

/// The viewport offsets a switch travels between, and how far to stretch its duration.
#[derive(Debug, PartialEq)]
pub struct StripTravel {
    pub from: SurfacePoint,
    pub to: SurfacePoint,
    /// Multiplier on the configured animation duration.
    pub duration_stretch: f64,
}

/// Rows spanned by a switch, as an offset from the first workspace involved.
pub fn row_of(index: usize, from_index: usize, to_index: usize) -> usize {
    index - from_index.min(to_index)
}

/// How far the viewport travels for a switch, and how long to take over it.
///
/// The offset for a workspace is its row times the pitch, so the distance is exactly the number of
/// workspaces crossed. Duration grows with distance but sublinearly and capped, so a four-workspace
/// jump reads as further than a one-workspace step without becoming tedious.
pub fn travel(from_index: usize, to_index: usize, row_pitch: f64) -> StripTravel {
    let low = from_index.min(to_index);
    let rows = (to_index as f64 - from_index as f64).abs().max(1.0);
    StripTravel {
        from: Point::new(0.0, (from_index - low) as f64 * row_pitch),
        to: Point::new(0.0, (to_index - low) as f64 * row_pitch),
        duration_stretch: rows.sqrt().min(MAX_DURATION_STRETCH),
    }
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Rect, Size};

    use super::*;

    // The usable height on the built-in: the full 1117 display less the 32pt band.
    const PITCH: f64 = 1085.0;

    fn rect<U>(x: f64, y: f64, w: f64, h: f64) -> Rect<U> {
        Rect::new(Point::new(x, y), Size::new(w, h))
    }

    fn origin() -> ScreenPoint { ScreenPoint::origin() }

    /// The switch is vertical, so the destination row has to arrive already scrolled to the window being
    /// switched to. A target outside this band means a sideways pan has to follow, which is the artefact.
    #[test]
    fn a_target_the_destination_row_already_shows_lands_in_view() {
        assert!(lands_in_view(rect(4.0, 32.0, 859.0, 1081.0), 1728.0));
        assert!(lands_in_view(rect(865.0, 32.0, 859.0, 1081.0), 1728.0));
        // Half on screen at either edge still counts: part of it is visible when the slide lands.
        assert!(lands_in_view(rect(-400.0, 32.0, 859.0, 1081.0), 1728.0));
        assert!(lands_in_view(rect(1700.0, 32.0, 859.0, 1081.0), 1728.0));
    }

    #[test]
    fn a_target_the_destination_row_has_scrolled_past_does_not() {
        // The measured case: the target sat 9763pt along its own strip while the row was drawn at 0.
        assert!(!lands_in_view(rect(9763.0, 32.0, 859.0, 1081.0), 1728.0));
        assert!(!lands_in_view(rect(-1718.0, 32.0, 859.0, 1081.0), 1728.0));
        // Exactly abutting either edge is not visible.
        assert!(!lands_in_view(rect(1728.0, 32.0, 859.0, 1081.0), 1728.0));
        assert!(!lands_in_view(rect(-859.0, 32.0, 859.0, 1081.0), 1728.0));
    }

    #[test]
    fn strips_are_stacked_one_usable_height_apart() {
        let window = rect(4.0, 36.0, 859.0, 1077.0);
        assert_eq!(strip_frame(window, origin(), 0, PITCH).origin.y, 36.0);
        assert_eq!(strip_frame(window, origin(), 1, PITCH).origin.y, 36.0 + PITCH);
        assert_eq!(
            strip_frame(window, origin(), 3, PITCH).origin.y,
            36.0 + 3.0 * PITCH
        );
    }

    /// With the pitch a USABLE height, one workspace's tiling area sits directly on the next: the
    /// area filling the usable frame [32, 1117] in one row ends exactly where the next row's begins.
    /// The only space left between two workspaces' windows is their own outer margins, as between
    /// two windows side by side, not the menu-bar band a full-display pitch would leave.
    #[test]
    fn consecutive_workspaces_abut() {
        let usable = rect(4.0, 32.0, 859.0, 1085.0);
        let first = strip_frame(usable, origin(), 0, PITCH);
        let second = strip_frame(usable, origin(), 1, PITCH);
        assert_eq!(second.origin.y - (first.origin.y + first.size.height), 0.0);

        // A real window carries a 4pt top and bottom margin inside its usable frame, so the visible
        // gap between the two workspaces' windows is those margins alone: 8pt, like a tiled pair.
        let window = rect(4.0, 36.0, 859.0, 1077.0);
        let first = strip_frame(window, origin(), 0, PITCH);
        let second = strip_frame(window, origin(), 1, PITCH);
        assert_eq!(second.origin.y - (first.origin.y + first.size.height), 8.0);
    }

    /// Each strip scrolls horizontally on its own and is pinned vertically to its row. Stacking must
    /// therefore never touch x, or strips would drag each other sideways.
    #[test]
    fn stacking_never_changes_horizontal_position() {
        for x in [-4301.0, -857.0, 4.0, 865.0, 5170.0] {
            let window = rect(x, 32.0, 859.0, 1081.0);
            for row in 0..4 {
                assert_eq!(strip_frame(window, origin(), row, PITCH).origin.x, x);
            }
        }
    }

    #[test]
    fn stacking_never_changes_size() {
        let window = rect(4.0, 32.0, 1720.0, 1081.0);
        let placed = strip_frame(window, origin(), 2, PITCH);
        assert_eq!(placed.size.width, 1720.0);
        assert_eq!(placed.size.height, 1081.0);
    }

    /// A window half off the side of the strip keeps its position and size, so it slides in and out at
    /// the edge instead of being clipped to the display or snapped inward.
    #[test]
    fn a_window_hanging_off_the_edge_is_placed_unchanged() {
        let hanging = rect(1400.0, 32.0, 859.0, 1081.0);
        let placed = strip_frame(hanging, origin(), 1, PITCH);
        assert_eq!(placed.origin.x, 1400.0);
        assert_eq!(placed.size.width, 859.0);
        assert!(
            placed.origin.x + placed.size.width > 1728.0,
            "still hangs off the right"
        );
    }

    #[test]
    fn a_display_that_is_not_at_the_origin_is_made_relative() {
        let window = rect(-670.0, -1660.0, 859.0, 1081.0);
        let placed = strip_frame(window, ScreenPoint::new(-670.0, -1692.0), 0, PITCH);
        assert_eq!(placed.origin.x, 0.0);
        assert_eq!(placed.origin.y, 32.0);
    }

    /// Under a look that zooms out, the strips beside sit `STACK_GAP` further apart than the
    /// usable height: on the laptop, 1109pt, the row above at -1109 and the row below at 1109, so
    /// between a window of one and of the next lie their margins and the gap, 32pt.
    #[test]
    fn rows_beside_a_strip_sit_the_gap_apart() {
        use crate::animation::domain::choreography::zoom::STACK_GAP;
        let (display, screen) = (rect(0.0, 0.0, 1728.0, 1117.0), rect(0.0, 32.0, 1728.0, PITCH));
        let gap = stack_gap(STACK_GAP, screen, display, 32.0);
        assert_eq!(gap, 24.0);
        let pitch = PITCH + gap;
        assert_eq!(pitch, 1109.0);
        let window = rect(4.0, 36.0, 859.0, 1077.0);
        let (above, here, below) = (
            strip_frame(window, origin(), -1, pitch),
            strip_frame(window, origin(), 0, pitch),
            strip_frame(window, origin(), 1, pitch),
        );
        assert_eq!((above.origin.y, below.origin.y), (36.0 - 1109.0, 36.0 + 1109.0));
        assert_eq!(here.origin.y - above.max().y, 32.0);
        assert_eq!(below.origin.y - here.max().y, 32.0);
        assert_eq!(
            (above.origin.x, below.origin.x),
            (4.0, 4.0),
            "each at its own scroll"
        );
    }

    /// With `bar off` the overlay covers the menu bar's band, and with a Dock along the bottom the
    /// band under the usable frame: the gap grows to whichever is taller, so nothing of a strip
    /// beside shows at rest.
    #[test]
    fn the_gap_grows_to_the_band_left_above_or_below_the_strip() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let menu_bar = rect(0.0, 37.0, 1728.0, 1080.0);
        assert_eq!(stack_gap(24.0, menu_bar, display, 0.0), 37.0);
        assert_eq!(
            stack_gap(24.0, menu_bar, display, 37.0),
            24.0,
            "under rini's own bar"
        );
        let docked = rect(0.0, 32.0, 1728.0, 1015.0);
        assert_eq!(stack_gap(24.0, docked, display, 32.0), 70.0);
        let elsewhere = rect(-1728.0, -1085.0, 1728.0, 1085.0);
        let bounds = rect(-1728.0, -1117.0, 1728.0, 1117.0);
        assert_eq!(stack_gap(24.0, elsewhere, bounds, 32.0), 24.0, "made relative");
    }

    /// At rest the strip above ends behind the bar's band and the strip below starts past the
    /// display's bottom, with a menu bar or a Dock leaving a band of their own too.
    #[test]
    fn nothing_of_a_row_shows_at_rest() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        for (screen, top_band) in [
            (rect(0.0, 32.0, 1728.0, 1085.0), 32.0),
            (rect(0.0, 37.0, 1728.0, 1080.0), 0.0),
            (rect(0.0, 32.0, 1728.0, 1015.0), 32.0),
        ] {
            let pitch = screen.size.height + stack_gap(24.0, screen, display, top_band);
            let window = rect(4.0, screen.origin.y, 859.0, screen.size.height);
            let above = strip_frame(window, origin(), -1, pitch);
            let below = strip_frame(window, origin(), 1, pitch);
            assert!(
                above.max().y <= top_band,
                "{screen:?}: above ends at {}",
                above.max().y
            );
            assert!(
                below.min().y >= display.max().y,
                "{screen:?}: below at {}",
                below.min().y
            );
        }
    }

    /// The strips beside the one at an index are the one above and the one below it, none past
    /// either end of the stack.
    #[test]
    fn the_top_and_bottom_of_the_stack_have_no_row_beyond() {
        assert_eq!(beside(0, 4), [(1, 1)]);
        assert_eq!(beside(2, 4), [(1, -1), (3, 1)]);
        assert_eq!(beside(3, 4), [(2, -1)]);
        assert_eq!(beside(0, 1), []);
    }

    /// What the deepest zoom shows: the display scaled about its centre by 1/0.7, x in [-370, 2098]
    /// on the laptop, y in [-239, 1356].
    #[test]
    fn in_zoomed_view_keeps_what_the_deepest_zoom_shows() {
        use crate::animation::domain::choreography::zoom::ZOOM_DEEPEST;
        let viewport = rect(0.0, 0.0, 1728.0, 1117.0);
        let at =
            |x: f64, y: f64| in_zoomed_view(rect(x, y, 859.0, 1077.0), viewport, ZOOM_DEEPEST, 0.0);
        assert!(at(-1228.0, 36.0), "its right edge just in, at -369");
        assert!(!at(-1230.0, 36.0), "its right edge at -371");
        assert!(at(2097.0, 36.0));
        assert!(!at(2099.0, 36.0));
        assert!(at(4.0, 36.0 - 1109.0), "the row above, its bottom at 4");
        assert!(at(4.0, 36.0 + 1109.0), "the row below, its top at 1145");
        assert!(!at(4.0, 36.0 - 2.0 * 1109.0), "two rows up");
        assert!(!at(4.0, 36.0 + 2.0 * 1109.0), "two rows down");
        assert!(!in_zoomed_view(
            rect(4.0, 36.0, 859.0, 1077.0),
            viewport,
            0.0,
            0.0
        ));
    }

    /// A bounce pushes the deepest zoom 72pt on screen, 103pt on the surface, so the band reaches
    /// x in [-473, 2201].
    #[test]
    fn in_zoomed_view_keeps_what_a_push_of_the_deepest_zoom_brings_in() {
        use crate::animation::domain::choreography::zoom::ZOOM_DEEPEST;
        use crate::animation::domain::motion::plan::EDGE_BOUNCE_OVERSHOOT;
        let viewport = rect(0.0, 0.0, 1728.0, 1117.0);
        let at = |x: f64, reach: f64| {
            in_zoomed_view(
                rect(x, 36.0 - 1109.0, 859.0, 1077.0),
                viewport,
                ZOOM_DEEPEST,
                reach,
            )
        };
        assert!(!at(2150.0, 0.0), "a row window past the band at rest");
        assert!(
            at(2150.0, EDGE_BOUNCE_OVERSHOOT),
            "brought in by a push toward the left wall"
        );
        assert!(
            at(-1300.0, EDGE_BOUNCE_OVERSHOOT),
            "brought in by a push toward the right wall"
        );
        assert!(!at(2202.0, EDGE_BOUNCE_OVERSHOOT));
        assert!(!at(-1333.0, EDGE_BOUNCE_OVERSHOOT));
    }

    #[test]
    fn an_adjacent_switch_travels_exactly_one_display_height() {
        let down = travel(1, 2, PITCH);
        assert_eq!(down.from.y, 0.0);
        assert_eq!(down.to.y, PITCH);
        assert_eq!((down.to.y - down.from.y).abs(), PITCH);
    }

    /// Moving DOWN the stack increases the offset. A tile at strip-surface coordinate c is drawn at c minus the offset, so a
    /// rising offset slides the strips UP and brings the next one in from below, which is the direction
    /// the stack implies.
    #[test]
    fn moving_down_the_stack_increases_the_offset() {
        assert!(travel(0, 1, PITCH).to.y > travel(0, 1, PITCH).from.y);
        assert!(travel(1, 3, PITCH).to.y > travel(1, 3, PITCH).from.y);
    }

    #[test]
    fn moving_up_the_stack_decreases_the_offset() {
        assert!(travel(3, 1, PITCH).to.y < travel(3, 1, PITCH).from.y);
        assert_eq!(travel(1, 0, PITCH).to.y, 0.0);
        assert_eq!(travel(1, 0, PITCH).from.y, PITCH);
    }

    /// A long jump scrolls past every strip in between rather than cutting to the destination, which is
    /// the whole reason the strip surface holds all of them. Distance must therefore scale with the gap.
    #[test]
    fn a_longer_jump_travels_proportionally_further() {
        assert_eq!(
            (travel(0, 1, PITCH).to.y - travel(0, 1, PITCH).from.y).abs(),
            PITCH
        );
        assert_eq!(
            (travel(0, 2, PITCH).to.y - travel(0, 2, PITCH).from.y).abs(),
            2.0 * PITCH
        );
        assert_eq!(
            (travel(0, 3, PITCH).to.y - travel(0, 3, PITCH).from.y).abs(),
            3.0 * PITCH
        );
    }

    #[test]
    fn travel_never_moves_horizontally() {
        for (from, to) in [(0, 1), (3, 0), (1, 2)] {
            let t = travel(from, to, PITCH);
            assert_eq!(t.from.x, 0.0);
            assert_eq!(t.to.x, 0.0);
        }
    }

    #[test]
    fn duration_grows_with_distance_but_is_capped() {
        assert_eq!(travel(0, 1, PITCH).duration_stretch, 1.0);
        assert!((travel(0, 2, PITCH).duration_stretch - 2.0f64.sqrt()).abs() < 1e-9);
        assert!((travel(0, 4, PITCH).duration_stretch - 2.0).abs() < 1e-9);
        // Capped, or a jump across many workspaces becomes tedious.
        assert_eq!(travel(0, 31, PITCH).duration_stretch, MAX_DURATION_STRETCH);
    }

    #[test]
    fn duration_does_not_depend_on_direction() {
        assert_eq!(
            travel(0, 3, PITCH).duration_stretch,
            travel(3, 0, PITCH).duration_stretch
        );
    }

    #[test]
    fn rows_are_counted_from_the_first_workspace_involved() {
        // Only the rows between the two ends are built, so a switch from 2 to 3 builds two rows rather
        // than four, whichever direction it runs in.
        assert_eq!(row_of(2, 2, 3), 0);
        assert_eq!(row_of(3, 2, 3), 1);
        assert_eq!(row_of(2, 3, 2), 0);
        assert_eq!(row_of(3, 3, 2), 1);
    }
}
