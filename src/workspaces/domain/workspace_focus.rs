//! Which window takes focus, as a precedence rather than a chain of `if focus_window.is_none()`.
//!
//! Two rules, both of which used to be written out inside `LayoutEngine` against live stores: what
//! to focus when a workspace becomes the active one, and where focus lands when the user cycles
//! through the windows in it. The reads stay in the engine; the orders are here.

use rini_core::ids::WindowId;

/// Every window that could take focus when a workspace becomes active.
///
/// All six are already filtered to the workspace in question by the caller — a candidate that is not
/// in the workspace is not a candidate, and checking that needs the stores. `None` means that source
/// had nobody to offer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FocusCandidates {
    /// The window the caller asked for.
    pub requested: Option<WindowId>,
    /// The window that had focus last time this workspace was active.
    pub last_focused: Option<WindowId>,
    /// The layout's own selected window.
    pub selected: Option<WindowId>,
    /// The first window the layout is showing.
    pub first_visible: Option<WindowId>,
    /// The floating window that had focus last.
    pub last_floating: Option<WindowId>,
    /// The first floating window in the workspace.
    pub first_floating: Option<WindowId>,
}

/// Who gets focus, in order of how much the choice was actually a choice.
///
/// An explicit request wins because someone asked. Then what the user was last looking at, which is
/// the answer that makes switching away and back feel like returning rather than arriving.
///
/// Tiled candidates outrank floating ones, which is the decision worth naming here: a floating window
/// sits on top of the strip, so focusing one on every workspace switch would bury the columns the
/// user switched to see. A floating window is the answer only when the strip has nobody to offer.
pub fn preferred(c: &FocusCandidates) -> Option<WindowId> {
    c.requested
        .or(c.last_focused)
        .or(c.selected)
        .or(c.first_visible)
        .or(c.last_floating)
        .or(c.first_floating)
}

/// Which way a cycle step moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cycle {
    Forward,
    Backward,
}

/// The index a cycle step lands on, wrapping at both ends.
///
/// `from + len - 1` rather than `from - 1`, because the indices are unsigned and stepping backward
/// from 0 would underflow rather than wrap. A list of one is its own next and previous.
pub fn cycle_step(from: usize, len: usize, direction: Cycle) -> Option<usize> {
    if len == 0 || from >= len {
        return None;
    }
    Some(match direction {
        Cycle::Forward => (from + 1) % len,
        Cycle::Backward => (from + len - 1) % len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    #[test]
    fn an_explicit_request_wins() {
        let c = FocusCandidates {
            requested: Some(window(1)),
            last_focused: Some(window(2)),
            selected: Some(window(3)),
            ..FocusCandidates::default()
        };
        assert_eq!(preferred(&c), Some(window(1)));
    }

    /// Switching away and back should feel like returning, so the window that had focus last beats
    /// whatever the layout would pick on its own.
    #[test]
    fn the_last_focused_window_beats_the_layout_s_own_choice() {
        let c = FocusCandidates {
            last_focused: Some(window(2)),
            selected: Some(window(3)),
            first_visible: Some(window(4)),
            ..FocusCandidates::default()
        };
        assert_eq!(preferred(&c), Some(window(2)));
    }

    #[test]
    fn the_layout_s_selection_beats_its_first_visible_window() {
        let c = FocusCandidates {
            selected: Some(window(3)),
            first_visible: Some(window(4)),
            ..FocusCandidates::default()
        };
        assert_eq!(preferred(&c), Some(window(3)));
    }

    /// The decision this exists to state. A floating window sits on top of the strip, so preferring
    /// one would bury the columns the user switched to the workspace to see.
    #[test]
    fn every_tiled_candidate_outranks_every_floating_one() {
        for tiled in ["selected", "first_visible"] {
            let mut c = FocusCandidates {
                last_floating: Some(window(8)),
                first_floating: Some(window(9)),
                ..FocusCandidates::default()
            };
            match tiled {
                "selected" => c.selected = Some(window(3)),
                _ => c.first_visible = Some(window(4)),
            }
            assert_ne!(preferred(&c), Some(window(8)), "{tiled} must win");
            assert_ne!(preferred(&c), Some(window(9)), "{tiled} must win");
        }
    }

    #[test]
    fn a_floating_window_is_the_answer_when_the_strip_has_nobody() {
        let c = FocusCandidates {
            last_floating: Some(window(8)),
            first_floating: Some(window(9)),
            ..FocusCandidates::default()
        };
        assert_eq!(
            preferred(&c),
            Some(window(8)),
            "the one that had focus, not the first"
        );
    }

    #[test]
    fn nobody_anywhere_is_nobody() {
        assert_eq!(preferred(&FocusCandidates::default()), None);
    }

    #[test]
    fn a_forward_step_wraps_at_the_end() {
        assert_eq!(cycle_step(0, 3, Cycle::Forward), Some(1));
        assert_eq!(cycle_step(2, 3, Cycle::Forward), Some(0));
    }

    /// The indices are unsigned, so stepping back from 0 underflows unless the length is added
    /// first. This is the case that arithmetic is written the way it is for.
    #[test]
    fn a_backward_step_wraps_at_the_beginning() {
        assert_eq!(cycle_step(0, 3, Cycle::Backward), Some(2));
        assert_eq!(cycle_step(2, 3, Cycle::Backward), Some(1));
    }

    #[test]
    fn a_single_window_is_its_own_next_and_previous() {
        assert_eq!(cycle_step(0, 1, Cycle::Forward), Some(0));
        assert_eq!(cycle_step(0, 1, Cycle::Backward), Some(0));
    }

    #[test]
    fn there_is_nowhere_to_step_in_an_empty_list() {
        assert_eq!(cycle_step(0, 0, Cycle::Forward), None);
    }

    /// An index the list does not have is a caller bug, and answering it would focus whichever
    /// window happened to be at the wrapped position.
    #[test]
    fn an_index_past_the_end_has_no_answer() {
        assert_eq!(cycle_step(3, 3, Cycle::Forward), None);
        assert_eq!(cycle_step(9, 3, Cycle::Backward), None);
    }
}
