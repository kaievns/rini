//! Which edge of a window a mouse drag is moving, the width it asks of the column, and the heights
//! it trades with the window across a stacked edge.
//!
//! macOS reports a drag as a frame and nothing else, so the edge is read off the two frames: the
//! one the window started from and the one it has now. See "Mouse resize" in
//! `src/layout/docs/strip.md`.

use objc2_core_foundation::CGRect;

/// How far an edge may drift and still count as where it was: frames are rounded to whole points
/// when written, and an app can settle a fraction of one.
const STILL: f64 = 0.5;

/// A window's left or right edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// A window's top or bottom edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Top,
    Bottom,
}

/// One report of a mouse drag on a window's edges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drag {
    /// The window's frame before the drag's first report.
    pub from: CGRect,
    /// The top or bottom edge the drag moves: the one this report moved, else the one it moved last,
    /// so a report back where the drag started gives back what that edge traded.
    pub end: Option<End>,
}

impl Drag {
    /// The report at `now` of the drag from `from` whose reports so far last moved `last_end`.
    pub fn report(from: CGRect, now: CGRect, last_end: Option<End>) -> Self {
        Self {
            from,
            end: dragged_end(from, now).or(last_end),
        }
    }
}

/// The vertical edge a drag from `start` to `now` moved, or `None` when both moved or neither did.
pub fn dragged_side(start: CGRect, now: CGRect) -> Option<Side> {
    let left_moved = (now.origin.x - start.origin.x).abs() > STILL;
    let right_moved = (max_x(now) - max_x(start)).abs() > STILL;
    match (left_moved, right_moved) {
        (true, false) => Some(Side::Left),
        (false, true) => Some(Side::Right),
        _ => None,
    }
}

/// The horizontal edge a drag from `start` to `now` moved, or `None` when both moved or neither did.
pub fn dragged_end(start: CGRect, now: CGRect) -> Option<End> {
    let top_moved = (now.origin.y - start.origin.y).abs() > STILL;
    let bottom_moved = (max_y(now) - max_y(start)).abs() > STILL;
    match (top_moved, bottom_moved) {
        (true, false) => Some(End::Top),
        (false, true) => Some(End::Bottom),
        _ => None,
    }
}

/// Gives `heights_px[row]` its `new_height` by trading the change with the row across the dragged
/// `end` alone, which stays at least 1pt high, so the column's total stays what it was. An edge with
/// no row across it changes nothing.
pub fn trade_height(heights_px: &mut [f64], row: usize, end: End, new_height: f64) {
    let across = match end {
        End::Top => row.checked_sub(1),
        End::Bottom => Some(row + 1).filter(|&i| i < heights_px.len()),
    };
    let Some(across) = across else {
        return;
    };
    let wanted = new_height.max(1.0) - heights_px[row];
    let change = wanted.min((heights_px[across] - 1.0).max(0.0));
    heights_px[row] += change;
    heights_px[across] -= change;
}

/// The width a drag from `start_width` to `now_width` asks of its column. A centred column grows on
/// both sides, so it takes twice the dragged edge's travel and that edge ends under the cursor.
pub fn dragged_width(start_width: f64, now_width: f64, centred: bool) -> f64 {
    if centred {
        start_width + 2.0 * (now_width - start_width)
    } else {
        now_width
    }
}

fn max_x(frame: CGRect) -> f64 {
    frame.origin.x + frame.size.width
}

fn max_y(frame: CGRect) -> f64 {
    frame.origin.y + frame.size.height
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    fn frame(x: f64, width: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, 40.0), CGSize::new(width, 800.0))
    }

    #[test]
    fn the_side_that_moved_is_the_one_that_left_its_start() {
        let start = frame(600.0, 882.0);
        for (now, side, case) in [
            (frame(334.0, 1148.0), Some(Side::Left), "left edge out"),
            (frame(700.0, 782.0), Some(Side::Left), "left edge in"),
            (frame(600.0, 1000.0), Some(Side::Right), "right edge out"),
            (frame(500.0, 1100.0), None, "both moved"),
            (frame(500.0, 882.0), None, "a move, not a resize"),
            (frame(600.0, 882.0), None, "neither"),
            (frame(600.4, 881.7), None, "drift within half a point"),
            (frame(334.0, 1148.4), Some(Side::Left), "the kept edge drifting"),
        ] {
            assert_eq!(dragged_side(start, now), side, "{case}");
        }
    }

    fn frame_y(y: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(600.0, y), CGSize::new(882.0, height))
    }

    #[test]
    fn the_end_that_moved_is_the_one_that_left_its_start() {
        let start = frame_y(374.0, 368.0);
        for (now, end, case) in [
            (frame_y(274.0, 468.0), Some(End::Top), "top edge up"),
            (frame_y(424.0, 318.0), Some(End::Top), "top edge down"),
            (frame_y(374.0, 468.0), Some(End::Bottom), "bottom edge down"),
            (frame_y(274.0, 568.0), None, "both moved"),
            (frame_y(274.0, 368.0), None, "a move, not a resize"),
            (frame_y(374.0, 368.0), None, "neither"),
            (frame_y(374.4, 367.7), None, "drift within half a point"),
            (frame_y(274.0, 468.4), Some(End::Top), "the kept edge drifting"),
        ] {
            assert_eq!(dragged_end(start, now), end, "{case}");
        }
    }

    #[test]
    fn a_report_that_moves_neither_end_keeps_the_end_the_drag_moved_last() {
        let start = frame_y(374.0, 368.0);
        let out = Drag::report(start, frame_y(274.0, 468.0), None);
        assert_eq!(out.end, Some(End::Top), "moved");
        assert_eq!(Drag::report(start, start, out.end).end, Some(End::Top), "back at the start");
        assert_eq!(
            Drag::report(start, frame_y(374.0, 468.0), out.end).end,
            Some(End::Bottom),
            "the other end"
        );
        assert_eq!(Drag::report(start, start, None).end, None, "a new drag");
    }

    #[test]
    fn a_top_edge_takes_height_only_from_the_window_above() {
        let mut heights = [300.0, 400.0, 405.0];
        trade_height(&mut heights, 1, End::Top, 500.0);
        assert_eq!(heights, [200.0, 500.0, 405.0], "grown");

        trade_height(&mut heights, 1, End::Top, 350.0);
        assert_eq!(heights, [350.0, 350.0, 405.0], "shrunk");

        trade_height(&mut heights, 1, End::Top, 900.0);
        assert_eq!(heights, [1.0, 699.0, 405.0], "the window above stops at 1pt");
    }

    #[test]
    fn a_bottom_edge_only_from_the_window_below() {
        let mut heights = [300.0, 400.0, 405.0];
        trade_height(&mut heights, 1, End::Bottom, 450.0);
        assert_eq!(heights, [300.0, 450.0, 355.0], "grown");

        trade_height(&mut heights, 0, End::Bottom, 100.0);
        assert_eq!(heights, [100.0, 650.0, 355.0], "shrunk");
    }

    #[test]
    fn an_edge_with_nothing_across_it_changes_nothing() {
        let mut heights = [300.0, 400.0, 405.0];
        trade_height(&mut heights, 0, End::Top, 500.0);
        trade_height(&mut heights, 2, End::Bottom, 500.0);
        assert_eq!(heights, [300.0, 400.0, 405.0]);
    }

    #[test]
    fn a_centred_drag_asks_for_twice_the_travel() {
        assert_eq!(dragged_width(882.0, 932.0, true), 982.0, "out 50");
        assert_eq!(dragged_width(882.0, 832.0, true), 782.0, "in 50");
        assert_eq!(dragged_width(882.0, 932.0, false), 932.0, "not centred");
    }
}
