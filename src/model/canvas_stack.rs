//! Pure geometry for the stack of workspace strips a vertical switch scrolls through.
//!
//! Every workspace is a horizontal strip. The strips are stacked in index order, one full display
//! height apart, and a switch moves the viewport rather than the strips: each strip keeps its own
//! horizontal position and is pinned vertically to its row.

use objc2_core_foundation::{CGPoint, CGRect};

/// Longest a switch may be stretched for distance, as a multiple of the configured duration.
const MAX_DURATION_STRETCH: f64 = 2.5;

/// Where a window sits on the canvas.
///
/// `row` is the workspace's position in the stack counted from the first one involved, not its absolute
/// index, so a switch only ever builds the rows between the two ends.
pub fn canvas_frame(frame: CGRect, display_origin: CGPoint, row: usize, row_pitch: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(
            frame.origin.x - display_origin.x,
            (frame.origin.y - display_origin.y) + row as f64 * row_pitch,
        ),
        frame.size,
    )
}

/// Whether a window will be on screen when a switch finishes.
///
/// A switch travels only in y, so the destination row has to be laid out at a scroll that already shows the
/// window being switched to. Otherwise the strip pans sideways afterwards to reveal it, which is the second
/// movement a switch exists to avoid. The viewport ends at canvas x = 0, so the band is the display's width.
pub fn lands_in_view(frame: CGRect, viewport_width: f64) -> bool {
    frame.origin.x < viewport_width && frame.origin.x + frame.size.width > 0.0
}

/// The viewport offsets a switch travels between, and how far to stretch its duration.
#[derive(Debug, PartialEq)]
pub struct CanvasTravel {
    pub from: CGPoint,
    pub to: CGPoint,
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
pub fn travel(from_index: usize, to_index: usize, row_pitch: f64) -> CanvasTravel {
    let low = from_index.min(to_index);
    let rows = (to_index as f64 - from_index as f64).abs().max(1.0);
    CanvasTravel {
        from: CGPoint::new(0.0, (from_index - low) as f64 * row_pitch),
        to: CGPoint::new(0.0, (to_index - low) as f64 * row_pitch),
        duration_stretch: rows.sqrt().min(MAX_DURATION_STRETCH),
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    const PITCH: f64 = 1117.0;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn origin() -> CGPoint {
        CGPoint::new(0.0, 0.0)
    }

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
    fn strips_are_stacked_one_display_height_apart() {
        let window = rect(4.0, 32.0, 859.0, 1081.0);
        assert_eq!(canvas_frame(window, origin(), 0, PITCH).origin.y, 32.0);
        assert_eq!(canvas_frame(window, origin(), 1, PITCH).origin.y, 32.0 + PITCH);
        assert_eq!(canvas_frame(window, origin(), 3, PITCH).origin.y, 32.0 + 3.0 * PITCH);
    }

    /// The pitch is the FULL display height, not the usable height, which is what leaves a gap the size
    /// of the menu bar between strips: each workspace's windows start below the bar inside their own row.
    #[test]
    fn the_row_gap_is_the_menu_bar_inset() {
        let window = rect(4.0, 32.0, 859.0, 1081.0);
        let first = canvas_frame(window, origin(), 0, PITCH);
        let second = canvas_frame(window, origin(), 1, PITCH);
        let gap = second.origin.y - (first.origin.y + first.size.height);
        assert_eq!(gap, PITCH - 1081.0);
        assert_eq!(gap, 36.0);
    }

    /// Each strip scrolls horizontally on its own and is pinned vertically to its row. Stacking must
    /// therefore never touch x, or strips would drag each other sideways.
    #[test]
    fn stacking_never_changes_horizontal_position() {
        for x in [-4301.0, -857.0, 4.0, 865.0, 5170.0] {
            let window = rect(x, 32.0, 859.0, 1081.0);
            for row in 0..4 {
                assert_eq!(canvas_frame(window, origin(), row, PITCH).origin.x, x);
            }
        }
    }

    #[test]
    fn stacking_never_changes_size() {
        let window = rect(4.0, 32.0, 1720.0, 1081.0);
        let placed = canvas_frame(window, origin(), 2, PITCH);
        assert_eq!(placed.size.width, 1720.0);
        assert_eq!(placed.size.height, 1081.0);
    }

    /// A window half off the side of the strip keeps its position and size, so it slides in and out at
    /// the edge instead of being clipped to the display or snapped inward.
    #[test]
    fn a_window_hanging_off_the_edge_is_placed_unchanged() {
        let hanging = rect(1400.0, 32.0, 859.0, 1081.0);
        let placed = canvas_frame(hanging, origin(), 1, PITCH);
        assert_eq!(placed.origin.x, 1400.0);
        assert_eq!(placed.size.width, 859.0);
        assert!(placed.origin.x + placed.size.width > 1728.0, "still hangs off the right");
    }

    #[test]
    fn a_display_that_is_not_at_the_origin_is_made_relative() {
        let window = rect(-670.0, -1660.0, 859.0, 1081.0);
        let placed = canvas_frame(window, CGPoint::new(-670.0, -1692.0), 0, PITCH);
        assert_eq!(placed.origin.x, 0.0);
        assert_eq!(placed.origin.y, 32.0);
    }

    #[test]
    fn an_adjacent_switch_travels_exactly_one_display_height() {
        let down = travel(1, 2, PITCH);
        assert_eq!(down.from.y, 0.0);
        assert_eq!(down.to.y, PITCH);
        assert_eq!((down.to.y - down.from.y).abs(), PITCH);
    }

    /// Moving DOWN the stack increases the offset. The canvas is positioned at the negated offset, so a
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
    /// the whole reason the canvas holds all of them. Distance must therefore scale with the gap.
    #[test]
    fn a_longer_jump_travels_proportionally_further() {
        assert_eq!((travel(0, 1, PITCH).to.y - travel(0, 1, PITCH).from.y).abs(), PITCH);
        assert_eq!((travel(0, 2, PITCH).to.y - travel(0, 2, PITCH).from.y).abs(), 2.0 * PITCH);
        assert_eq!((travel(0, 3, PITCH).to.y - travel(0, 3, PITCH).from.y).abs(), 3.0 * PITCH);
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
        assert_eq!(travel(0, 3, PITCH).duration_stretch, travel(3, 0, PITCH).duration_stretch);
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
