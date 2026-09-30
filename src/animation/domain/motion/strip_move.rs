//! A window moved along the strip: where the two columns that change places start, and the nudge
//! that shows a move the camera follows. See "The move flight" in
//! `src/animation/docs/animation-smoothness.md`.

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::WindowId;
use rini_ipc::protocol::Direction;

use super::easing::nudge_displacement;

/// Frames this close in x are one column.
const COLUMN_TOLERANCE: f64 = 0.5;

/// How far a moved window steps toward where it is going, as a share of the viewport's width.
pub const NUDGE_SHARE: f64 = 1.0 / 3.0;

/// A moved window whose own travel on screen is under this share of the viewport is nudged: the
/// camera followed it, so nothing on screen says it moved.
pub const NUDGE_BELOW: f64 = 0.25;

/// How much longer than a plain flight a flight carrying a nudge runs, so the nudge's peak speed
/// stays moderate.
pub const NUDGE_STRETCH: f64 = 1.3;

/// A moved window's out-and-back: `offset` at its furthest, riding the window's own container.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Nudge {
    pub window: WindowId,
    pub offset: CGPoint,
}

/// Where the moved window's column and the column it passed started, in the strip's coordinates.
///
/// `strip` is every tiled window at its new frame. Together the two columns fill the span they fill
/// now, in the order they had before, so the gap between them is the one they have now. Every window
/// of a column moves by the same amount. Empty for a vertical move, or one with no column to pass.
pub fn swap_starts(
    strip: &[(WindowId, CGRect)],
    moved: WindowId,
    direction: Direction,
) -> Vec<(WindowId, CGRect)> {
    let Some(&(_, at)) = strip.iter().find(|(window, _)| *window == moved) else {
        return Vec::new();
    };
    let moved_x = at.origin.x;
    let xs = strip.iter().map(|(_, frame)| frame.origin.x);
    let passed_x = match direction {
        Direction::Right => xs.filter(|x| *x < moved_x - COLUMN_TOLERANCE).reduce(f64::max),
        Direction::Left => xs.filter(|x| *x > moved_x + COLUMN_TOLERANCE).reduce(f64::min),
        Direction::Up | Direction::Down => None,
    };
    let Some(passed_x) = passed_x else {
        return Vec::new();
    };
    let in_column = |frame: &CGRect, x: f64| (frame.origin.x - x).abs() <= COLUMN_TOLERANCE;
    let Some(passed_width) = strip
        .iter()
        .find(|(_, frame)| in_column(frame, passed_x))
        .map(|(_, f)| f.size.width)
    else {
        return Vec::new();
    };
    let moved_width = at.size.width;
    let (moved_start, passed_start) = match direction {
        Direction::Right => (passed_x, moved_x + moved_width - passed_width),
        _ => (passed_x + passed_width - moved_width, moved_x),
    };
    strip
        .iter()
        .filter_map(|&(window, frame)| {
            let x = if in_column(&frame, moved_x) {
                moved_start
            } else if in_column(&frame, passed_x) {
                passed_start
            } else {
                return None;
            };
            Some((window, CGRect::new(CGPoint::new(x, frame.origin.y), frame.size)))
        })
        .collect()
}

/// The nudge a moved window gets, from its own travel on screen, or `None` when that travel
/// already shows the move.
pub fn nudge(travel: f64, viewport_width: f64, direction: Direction) -> Option<CGPoint> {
    if travel.abs() >= viewport_width * NUDGE_BELOW {
        return None;
    }
    let step = viewport_width * NUDGE_SHARE;
    match direction {
        Direction::Right => Some(CGPoint::new(step, 0.0)),
        Direction::Left => Some(CGPoint::new(-step, 0.0)),
        Direction::Up | Direction::Down => None,
    }
}

/// The nudge as evenly spaced keyframes about `step` seconds apart over `seconds`, from rest to rest.
pub fn nudge_samples(offset: CGPoint, seconds: f64, step: f64) -> Vec<CGPoint> {
    let steps = (seconds / step).ceil().max(1.0) as usize;
    (0..=steps)
        .map(|k| {
            let d = nudge_displacement(k as f64 / steps as f64);
            CGPoint::new(offset.x * d, offset.y * d)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;
    use crate::animation::domain::motion::easing::nudge_rate;

    const GAP: f64 = 3.0;
    const HEIGHT: f64 = 1081.0;

    fn wid(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    fn column(x: f64, width: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, 32.0), CGSize::new(width, HEIGHT))
    }

    fn start_of(starts: &[(WindowId, CGRect)], window: WindowId) -> Option<f64> {
        starts.iter().find(|(w, _)| *w == window).map(|(_, frame)| frame.origin.x)
    }

    /// A B W, just after W moved right past B: W started where B is now, B where W's right edge
    /// leaves room for it. A is where it was.
    #[test]
    fn a_move_right_starts_the_pair_in_each_others_slots() {
        let strip = [
            (wid(1), column(0.0, 861.0)),
            (wid(2), column(864.0, 861.0)),
            (wid(3), column(1728.0, 861.0)),
        ];
        let starts = swap_starts(&strip, wid(3), Direction::Right);
        assert_eq!(start_of(&starts, wid(3)), Some(864.0));
        assert_eq!(start_of(&starts, wid(2)), Some(1728.0));
        assert_eq!(
            start_of(&starts, wid(1)),
            None,
            "the rest of the strip is untouched"
        );
    }

    #[test]
    fn a_move_left_starts_the_pair_in_each_others_slots() {
        let strip = [
            (wid(1), column(0.0, 861.0)),
            (wid(2), column(864.0, 861.0)),
            (wid(3), column(1728.0, 861.0)),
        ];
        let starts = swap_starts(&strip, wid(1), Direction::Left);
        assert_eq!(start_of(&starts, wid(1)), Some(864.0));
        assert_eq!(start_of(&starts, wid(2)), Some(0.0));
        assert_eq!(start_of(&starts, wid(3)), None);
    }

    /// The formulas the move is specified by, W of width a and N of width b with gap g: moving right
    /// W started at N's new x and N at N's new x + a + g; moving left W started at W's new x + b + g
    /// and N at W's new x.
    #[test]
    fn unequal_widths_keep_the_gap_between_the_pair() {
        let (a, b) = (600.0, 1000.0);
        let right = [(wid(2), column(0.0, b)), (wid(1), column(b + GAP, a))];
        let starts = swap_starts(&right, wid(1), Direction::Right);
        assert_eq!(start_of(&starts, wid(1)), Some(0.0));
        assert_eq!(start_of(&starts, wid(2)), Some(a + GAP));

        let left = [(wid(1), column(0.0, a)), (wid(2), column(a + GAP, b))];
        let starts = swap_starts(&left, wid(1), Direction::Left);
        assert_eq!(start_of(&starts, wid(1)), Some(b + GAP));
        assert_eq!(start_of(&starts, wid(2)), Some(0.0));
    }

    /// Every window of the passed column moves by one amount and keeps its row.
    #[test]
    fn a_stacked_neighbour_moves_as_one_column() {
        let top = CGRect::new(CGPoint::new(0.0, 32.0), CGSize::new(861.0, 539.0));
        let bottom = CGRect::new(CGPoint::new(0.0, 574.0), CGSize::new(861.0, 539.0));
        let strip = [
            (wid(1), top),
            (wid(2), bottom),
            (wid(3), column(864.0, 861.0)),
            (wid(4), column(1728.0, 861.0)),
        ];
        let starts = swap_starts(&strip, wid(3), Direction::Right);
        assert_eq!(starts.len(), 3);
        for (window, row) in [(wid(1), top), (wid(2), bottom)] {
            let (_, start) = starts.iter().find(|(w, _)| *w == window).copied().expect("moved");
            assert_eq!(start.origin, CGPoint::new(864.0, row.origin.y));
            assert_eq!(start.size, row.size);
        }
        assert_eq!(start_of(&starts, wid(3)), Some(0.0));
        assert_eq!(start_of(&starts, wid(4)), None);
    }

    #[test]
    fn nothing_starts_elsewhere_for_a_vertical_move_or_with_no_column_to_pass() {
        let strip = [(wid(1), column(0.0, 861.0)), (wid(2), column(864.0, 861.0))];
        assert!(swap_starts(&strip, wid(1), Direction::Up).is_empty());
        assert!(
            swap_starts(&strip, wid(1), Direction::Right).is_empty(),
            "nothing left of it"
        );
        assert!(
            swap_starts(&strip, wid(9), Direction::Right).is_empty(),
            "not on the strip"
        );
    }

    /// A full-width column the camera follows travels nothing on screen: it steps a third toward where
    /// it went. Half a column of travel shows the move already.
    #[test]
    fn only_a_move_the_camera_follows_is_nudged() {
        assert_eq!(
            nudge(0.0, 1728.0, Direction::Right),
            Some(CGPoint::new(576.0, 0.0))
        );
        assert_eq!(
            nudge(-12.0, 1728.0, Direction::Left),
            Some(CGPoint::new(-576.0, 0.0))
        );
        assert_eq!(nudge(867.0, 1728.0, Direction::Right), None);
        assert_eq!(nudge(432.0, 1728.0, Direction::Right), None);
        assert_eq!(nudge(0.0, 1728.0, Direction::Up), None);
    }

    #[test]
    fn the_nudge_keyframes_run_rest_to_rest_through_the_offset() {
        let offset = CGPoint::new(576.0, 0.0);
        let samples = nudge_samples(offset, 0.455, 1.0 / 120.0);
        assert_eq!(samples.first(), Some(&CGPoint::new(0.0, 0.0)));
        assert!(samples.last().expect("some").x.abs() < 1e-9);
        let furthest = samples.iter().map(|p| p.x).fold(0.0, f64::max);
        assert!((furthest - 576.0).abs() < 1.0, "{furthest}");
        assert!(samples.iter().all(|p| p.y == 0.0 && p.x >= 0.0));
    }

    /// The numbers "The move flight" quotes: a third of the laptop's 1720pt and of the 3008pt
    /// external, over the stretched default flight, peak well under the 28,338pt/s already reported
    /// as a jerk and near the edge bounce's own launch.
    #[test]
    fn the_nudge_peaks_at_a_moderate_speed() {
        let seconds = 0.35 * NUDGE_STRETCH;
        let peak = |offset: f64| offset * nudge_rate(0.25) / seconds;
        assert!((peak(573.0) - 3956.0).abs() < 1.0, "{}", peak(573.0));
        assert!((peak(1003.0) - 6925.0).abs() < 1.0, "{}", peak(1003.0));
        for i in 0..=1000 {
            let t = i as f64 / 1000.0;
            assert!(1003.0 * nudge_rate(t).abs() / seconds <= peak(1003.0) + 1e-6);
        }
    }
}
