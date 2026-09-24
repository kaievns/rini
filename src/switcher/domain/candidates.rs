//! Which windows the switcher offers, and in what order.
//!
//! The strip answers "what is beside this window on this display". The switcher answers a different
//! question — "every window I have, wherever it is" — so it is built from the workspace assignments
//! rather than from any one layout tree, and it groups nothing: one row per window, never per
//! application, because a row that stands for four Slack windows cannot take you to the third one.
//!
//! Order is focus order first, then a stable tail. Focus order is what makes the switcher useful: the
//! window you want next is nearly always the one you were in before this one. But a window rini has
//! never seen focused has no place in that order, and the enumeration it arrives in is an `FxHashMap`
//! iteration — unspecified, and different run to run. Sorting the tail explicitly is what stops the
//! list reshuffling between two presses of the same key.

use rini_core::ids::{SpaceId, WindowId};

use crate::windows::domain::focus_order::FocusOrder;

/// One window the switcher can offer.
///
/// Carries no picture. A thumbnail is a platform concern and a domain file may not name one — the
/// architecture test bans `platform::` under `domain/` — so the surface pairs a row with its snapshot
/// by `WindowId` at draw time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub window: WindowId,
    pub space: SpaceId,
    /// Sort key for the stable tail, from the workspace's canonical position rather than its id.
    pub workspace_index: usize,
    pub title: String,
    pub app_name: String,
    /// Minimised windows are offered. macOS's own switcher shows them, and leaving them out means a
    /// window you minimised becomes unreachable by the one key whose job is reaching windows.
    pub is_minimized: bool,
}

/// How much of the machine a switch covers.
///
/// The global switcher is the one asked for first; the per-workspace variant is the same machinery
/// with a narrower candidate set, which is why this is a parameter rather than a second code path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every window on every workspace and every display.
    Everything,
    /// Only windows on this space — one display's current workspace.
    ThisSpace(SpaceId),
}

impl Scope {
    fn admits(self, candidate: &Candidate) -> bool {
        match self {
            Self::Everything => true,
            Self::ThisSpace(space) => candidate.space == space,
        }
    }
}

/// The switcher's list: focus order first, then everything never focused, stably ordered.
///
/// `candidates` may arrive in any order. The result is deterministic for the same inputs, which is
/// what lets two presses of the same key walk the list rather than jumping around it.
pub fn switch_list(
    candidates: impl IntoIterator<Item = Candidate>,
    order: &FocusOrder,
    scope: Scope,
) -> Vec<Candidate> {
    let mut admitted: Vec<Candidate> =
        candidates.into_iter().filter(|candidate| scope.admits(candidate)).collect();

    // Stable tail first, so windows with no focus record keep a predictable order among themselves.
    admitted.sort_by(|a, b| {
        (a.space.get(), a.workspace_index, a.window).cmp(&(
            b.space.get(),
            b.workspace_index,
            b.window,
        ))
    });

    // Then lift the ones with a focus record to the front, in that order. A stable partition rather
    // than a comparator, because "never focused" has no position to compare against.
    let mut known: Vec<Candidate> = Vec::with_capacity(admitted.len());
    let mut unknown: Vec<Candidate> = Vec::with_capacity(admitted.len());
    for candidate in admitted {
        if order.position(candidate.window).is_some() {
            known.push(candidate);
        } else {
            unknown.push(candidate);
        }
    }
    known.sort_by_key(|candidate| order.position(candidate.window).unwrap_or(usize::MAX));
    known.extend(unknown);
    known
}

/// Where the selection starts when a switch opens.
///
/// The SECOND entry, not the first. The first is the window you are already in, so starting there
/// would make a quick tap do nothing — and "tap to get back to the last window" is the behaviour the
/// key is for. A list of one has nowhere to go and answers `None`.
pub fn opening_selection(list: &[Candidate]) -> Option<usize> {
    match list.len() {
        0 | 1 => None,
        _ => Some(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(pid: rini_core::ids::pid_t, idx: u32) -> WindowId {
        WindowId::new(pid, idx)
    }

    fn candidate(pid: rini_core::ids::pid_t, idx: u32, space: u64, workspace: usize) -> Candidate {
        Candidate {
            window: win(pid, idx),
            space: SpaceId::new(space),
            workspace_index: workspace,
            title: format!("window {idx}"),
            app_name: format!("app {pid}"),
            is_minimized: false,
        }
    }

    #[test]
    fn focus_order_comes_first() {
        let list = vec![
            candidate(1, 1, 1, 0),
            candidate(1, 2, 1, 0),
            candidate(1, 3, 1, 0),
        ];
        let mut order = FocusOrder::default();
        order.touch(win(1, 1));
        order.touch(win(1, 3));

        let switched = switch_list(list, &order, Scope::Everything);

        assert_eq!(
            switched.iter().map(|c| c.window).collect::<Vec<_>>(),
            vec![win(1, 3), win(1, 1), win(1, 2)],
            "most recent, then the rest of the focus order, then the never-focused"
        );
    }

    /// The enumeration is an FxHashMap walk, so the same windows arrive in a different order run to
    /// run. Everything with no focus record has to be sorted explicitly or the list reshuffles between
    /// two presses of the same key.
    #[test]
    fn windows_never_focused_are_ordered_stably_whatever_order_they_arrive_in() {
        let forwards = vec![
            candidate(1, 1, 1, 0),
            candidate(2, 5, 1, 1),
            candidate(1, 9, 2, 0),
        ];
        let backwards: Vec<Candidate> = forwards.iter().rev().cloned().collect();
        let order = FocusOrder::default();

        let a = switch_list(forwards, &order, Scope::Everything);
        let b = switch_list(backwards, &order, Scope::Everything);

        assert_eq!(a, b);
        assert_eq!(
            a.iter().map(|c| c.window).collect::<Vec<_>>(),
            vec![win(1, 1), win(2, 5), win(1, 9)],
            "space, then workspace, then window id"
        );
    }

    /// One row per window. Four windows of one application are four rows, because a row standing for
    /// an application cannot take you to its third window.
    #[test]
    fn windows_of_one_application_are_not_bundled() {
        let list = vec![
            candidate(7, 1, 1, 0),
            candidate(7, 2, 1, 0),
            candidate(7, 3, 1, 0),
        ];
        let switched = switch_list(list, &FocusOrder::default(), Scope::Everything);
        assert_eq!(switched.len(), 3);
    }

    #[test]
    fn everything_spans_every_space_and_workspace() {
        let list = vec![
            candidate(1, 1, 1, 0),
            candidate(1, 2, 1, 3),
            candidate(1, 3, 99, 0),
        ];
        let switched = switch_list(list, &FocusOrder::default(), Scope::Everything);
        assert_eq!(switched.len(), 3);
    }

    #[test]
    fn this_space_admits_only_its_own_windows() {
        let list = vec![
            candidate(1, 1, 1, 0),
            candidate(1, 2, 1, 3),
            candidate(1, 3, 99, 0),
        ];
        let switched = switch_list(list, &FocusOrder::default(), Scope::ThisSpace(SpaceId::new(1)));
        assert_eq!(
            switched.iter().map(|c| c.window).collect::<Vec<_>>(),
            vec![win(1, 1), win(1, 2)]
        );
    }

    /// A minimised window is still a window you want to reach. Native cmd-tab offers them.
    #[test]
    fn minimised_windows_are_offered() {
        let mut minimised = candidate(1, 1, 1, 0);
        minimised.is_minimized = true;
        let switched = switch_list(vec![minimised], &FocusOrder::default(), Scope::Everything);
        assert_eq!(switched.len(), 1);
    }

    /// A focus record for a window that is no longer a candidate must not resurrect it.
    #[test]
    fn a_focus_record_for_a_gone_window_adds_nothing() {
        let mut order = FocusOrder::default();
        order.touch(win(9, 9));
        order.touch(win(1, 1));

        let switched = switch_list(vec![candidate(1, 1, 1, 0)], &order, Scope::Everything);

        assert_eq!(
            switched.iter().map(|c| c.window).collect::<Vec<_>>(),
            vec![win(1, 1)]
        );
    }

    /// The whole point of a tap: it takes you to the window you were in before this one.
    #[test]
    fn a_switch_opens_on_the_second_entry() {
        let list = vec![candidate(1, 1, 1, 0), candidate(1, 2, 1, 0)];
        assert_eq!(opening_selection(&list), Some(1));
    }

    #[test]
    fn a_list_with_nowhere_to_go_opens_on_nothing() {
        assert_eq!(opening_selection(&[]), None);
        assert_eq!(opening_selection(&[candidate(1, 1, 1, 0)]), None);
    }
}
