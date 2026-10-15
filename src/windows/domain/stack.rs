//! A window's place in the window server's stack, and what rini decides over places: whether a
//! window is covered by a peer above it, and how a window of rini's own takes another's place.

use std::ffi::c_int;

use objc2_core_foundation::CGRect;
use rini_geometry::CGRectExt;

/// A window level, as AppKit's `NSWindowLevel` counts them.
pub type WindowLevel = isize;

/// `NSNormalWindowLevel`, the level of an ordinary app window.
pub const NORMAL_WINDOW_LEVEL: WindowLevel = 0;

/// A window's place in the window server's stack: its frame, level and sub-level, each none where
/// the window server gave no answer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StackPlace {
    pub frame: CGRect,
    pub level: Option<WindowLevel>,
    pub sub_level: Option<c_int>,
}

/// Whether one of the windows stacked above `candidate` sits wholly inside its frame at the same
/// level and sub-level. Raising the candidate would put it over that window, which the user has
/// deliberately on top; `above` is the stack from the top down to the candidate, exclusive.
pub fn covered_by_peer_above(
    candidate: StackPlace,
    above: impl IntoIterator<Item = StackPlace>,
) -> bool {
    above.into_iter().any(|peer| {
        candidate.frame.contains_rect(peer.frame)
            && candidate.level.zip(peer.level).is_some_and(|(c, p)| c == p)
            && candidate.sub_level.zip(peer.sub_level).is_some_and(|(c, p)| c == p)
    })
}

/// One window server write that puts a window of rini's own in another window's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackWrite {
    Level(c_int),
    SubLevel(c_int),
}

/// The writes that put a window of rini's own at another window's `level` and `sub_level`, in
/// order: the level, the normal one when none was read, then the sub-level only when one was read.
pub fn stack_place_writes(level: Option<WindowLevel>, sub_level: Option<c_int>) -> Vec<StackWrite> {
    let level = StackWrite::Level(level.unwrap_or(NORMAL_WINDOW_LEVEL) as c_int);
    std::iter::once(level).chain(sub_level.map(StackWrite::SubLevel)).collect()
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    fn place(x: f64, w: f64, level: Option<WindowLevel>, sub_level: Option<c_int>) -> StackPlace {
        StackPlace {
            frame: CGRect::new(CGPoint::new(x, 0.0), CGSize::new(w, 100.0)),
            level,
            sub_level,
        }
    }

    #[test]
    fn a_peer_wholly_inside_at_the_same_level_covers() {
        let candidate = place(0.0, 1000.0, Some(0), Some(0));
        assert!(covered_by_peer_above(candidate, [place(
            100.0,
            200.0,
            Some(0),
            Some(0)
        )]));
    }

    #[test]
    fn a_peer_at_another_level_or_sub_level_or_overhanging_does_not() {
        let candidate = place(0.0, 1000.0, Some(0), Some(0));
        assert!(!covered_by_peer_above(candidate, [place(
            100.0,
            200.0,
            Some(3),
            Some(0)
        )]));
        assert!(!covered_by_peer_above(candidate, [place(
            100.0,
            200.0,
            Some(0),
            Some(1)
        )]));
        assert!(
            !covered_by_peer_above(candidate, [place(900.0, 200.0, Some(0), Some(0))]),
            "overhangs the edge"
        );
        assert!(!covered_by_peer_above(candidate, []));
    }

    #[test]
    fn an_unanswered_sub_level_is_no_evidence_of_a_shared_one() {
        let candidate = place(0.0, 1000.0, Some(0), Some(0));
        assert!(!covered_by_peer_above(candidate, [place(
            100.0,
            200.0,
            Some(0),
            None
        )]));
        assert!(!covered_by_peer_above(place(0.0, 1000.0, Some(0), None), [
            place(100.0, 200.0, Some(0), Some(0))
        ]));
        assert!(!covered_by_peer_above(place(0.0, 1000.0, Some(0), None), [
            place(100.0, 200.0, Some(0), None)
        ]));
    }

    #[test]
    fn an_unknown_level_never_covers() {
        let candidate = place(0.0, 1000.0, None, Some(0));
        assert!(!covered_by_peer_above(candidate, [place(
            100.0,
            200.0,
            None,
            Some(0)
        )]));
    }

    #[test]
    fn an_unanswered_sub_level_is_not_written() {
        use StackWrite::{Level, SubLevel};
        assert_eq!(stack_place_writes(Some(3), Some(1)), [Level(3), SubLevel(1)]);
        assert_eq!(stack_place_writes(Some(3), Some(0)), [Level(3), SubLevel(0)]);
        assert_eq!(stack_place_writes(Some(3), None), [Level(3)]);
        assert_eq!(stack_place_writes(None, None), [Level(
            NORMAL_WINDOW_LEVEL as c_int
        )]);
    }
}
