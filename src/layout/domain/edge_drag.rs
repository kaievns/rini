//! Which edge of a window a mouse drag is moving, and the width it asks of the column.
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

    #[test]
    fn a_centred_drag_asks_for_twice_the_travel() {
        assert_eq!(dragged_width(882.0, 932.0, true), 982.0, "out 50");
        assert_eq!(dragged_width(882.0, 832.0, true), 782.0, "in 50");
        assert_eq!(dragged_width(882.0, 932.0, false), 932.0, "not centred");
    }
}
