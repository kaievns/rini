//! The order windows were last focused in, most recent first.
//!
//! A switcher offers windows in the order you last used them, the way macOS's own cmd-tab does. That
//! order is not derivable from anything rini already keeps. The layout knows where a window SITS, the
//! catalogue knows it exists, and the two focus memories that exist are single-slot and untimestamped:
//! `MainWindowTracker::last_focused_by_app` holds one window per application and
//! `VirtualWorkspace::last_focused` one per space. Neither can answer "which window did I use before
//! this one", which is the only question a switcher asks.
//!
//! Nor is the z-order an answer on this codebase. Raising the focused window lifts the whole visible
//! strip with it (`strip_group_to_lift`), so front-to-back says which COLUMN was touched last, not
//! which window, and it covers on-screen windows only — a switcher's whole point is the ones that are
//! not.
//!
//! So this is its own record: a list, front = most recently focused, no timestamps. Order is all that
//! is ever asked of it, and a clock would add a second thing to keep true.

use rini_core::ids::{WindowId, pid_t};

/// Windows in the order they were last focused, most recent first.
///
/// Unbounded on purpose. One `WindowId` is 8 bytes and the list is pruned whenever a window dies, so
/// it is bounded in practice by the number of live windows — a cap would only ever throw away the tail
/// of the list the switcher exists to show.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FocusOrder {
    recent: Vec<WindowId>,
}

impl FocusOrder {
    /// Record that `window` has the focus now, moving it to the front.
    ///
    /// Idempotent: focusing the window that is already at the front changes nothing, which matters
    /// because one raise produces a focus report per window it touches and the strip raise touches
    /// every visible one.
    pub fn touch(&mut self, window: WindowId) {
        if self.recent.first() == Some(&window) {
            return;
        }
        self.recent.retain(|candidate| *candidate != window);
        self.recent.insert(0, window);
    }

    /// Drop a window that no longer exists.
    pub fn forget(&mut self, window: WindowId) {
        self.recent.retain(|candidate| *candidate != window);
    }

    /// Drop every window belonging to an application that has quit.
    pub fn forget_app(&mut self, pid: pid_t) {
        self.recent.retain(|candidate| candidate.pid != pid);
    }

    /// Carry a window's place across an identity change, keeping its position in the order.
    ///
    /// An application relaunching into a new `WindowId` for the same window is the same window to the
    /// user, and dropping it to the back of the list would put it behind windows they have not touched
    /// since.
    pub fn rekey(&mut self, from: WindowId, to: WindowId) {
        // Remove any existing entry for the new id first, or a rekey onto an id already in the list
        // leaves the window twice.
        self.recent.retain(|candidate| *candidate != to);
        if let Some(slot) = self.recent.iter_mut().find(|candidate| **candidate == from) {
            *slot = to;
        }
    }

    /// Most recently focused first.
    pub fn iter(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.recent.iter().copied()
    }

    /// Where `window` sits in the order, or `None` if it has never been focused.
    pub fn position(&self, window: WindowId) -> Option<usize> {
        self.recent.iter().position(|candidate| *candidate == window)
    }

    pub fn len(&self) -> usize {
        self.recent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.recent.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(pid: pid_t, idx: u32) -> WindowId {
        WindowId::new(pid, idx)
    }

    #[test]
    fn the_most_recently_focused_window_is_first() {
        let mut order = FocusOrder::default();
        order.touch(win(1, 1));
        order.touch(win(1, 2));
        order.touch(win(1, 3));

        assert_eq!(
            order.iter().collect::<Vec<_>>(),
            vec![win(1, 3), win(1, 2), win(1, 1)]
        );
    }

    /// Re-focusing a window already in the list MOVES it rather than adding it again.
    #[test]
    fn focusing_a_window_again_moves_it_to_the_front() {
        let mut order = FocusOrder::default();
        order.touch(win(1, 1));
        order.touch(win(1, 2));
        order.touch(win(1, 1));

        assert_eq!(order.iter().collect::<Vec<_>>(), vec![win(1, 1), win(1, 2)]);
        assert_eq!(order.len(), 2, "no duplicate");
    }

    /// One raise produces a focus report per window it touches, and raising the focused window lifts
    /// the whole visible strip. Re-touching the front window has to be free, or every raise reshuffles
    /// the order it was trying to preserve.
    #[test]
    fn re_touching_the_front_window_changes_nothing() {
        let mut order = FocusOrder::default();
        order.touch(win(1, 1));
        order.touch(win(1, 2));
        let before = order.clone();

        order.touch(win(1, 2));

        assert_eq!(order, before);
    }

    #[test]
    fn a_window_that_dies_leaves_the_order() {
        let mut order = FocusOrder::default();
        order.touch(win(1, 1));
        order.touch(win(1, 2));

        order.forget(win(1, 1));

        assert_eq!(order.iter().collect::<Vec<_>>(), vec![win(1, 2)]);
    }

    #[test]
    fn an_application_that_quits_takes_all_its_windows() {
        let mut order = FocusOrder::default();
        order.touch(win(1, 1));
        order.touch(win(2, 1));
        order.touch(win(1, 2));

        order.forget_app(1);

        assert_eq!(order.iter().collect::<Vec<_>>(), vec![win(2, 1)]);
    }

    /// A relaunched window is the same window to the user, so it keeps its place rather than dropping
    /// behind windows they have not touched since.
    #[test]
    fn a_rekeyed_window_keeps_its_position() {
        let mut order = FocusOrder::default();
        order.touch(win(1, 1));
        order.touch(win(1, 2));
        order.touch(win(1, 3));

        order.rekey(win(1, 2), win(1, 9));

        assert_eq!(
            order.iter().collect::<Vec<_>>(),
            vec![win(1, 3), win(1, 9), win(1, 1)]
        );
    }

    /// Rekeying onto an id that is already listed must not leave the window in twice.
    #[test]
    fn a_rekey_onto_a_listed_id_does_not_duplicate_it() {
        let mut order = FocusOrder::default();
        order.touch(win(1, 1));
        order.touch(win(1, 2));

        order.rekey(win(1, 1), win(1, 2));

        assert_eq!(order.iter().collect::<Vec<_>>(), vec![win(1, 2)]);
    }

    #[test]
    fn rekeying_a_window_that_was_never_focused_adds_nothing() {
        let mut order = FocusOrder::default();
        order.touch(win(1, 1));

        order.rekey(win(9, 9), win(9, 10));

        assert_eq!(order.iter().collect::<Vec<_>>(), vec![win(1, 1)]);
    }

    #[test]
    fn position_says_where_a_window_sits_and_nothing_for_one_never_focused() {
        let mut order = FocusOrder::default();
        order.touch(win(1, 1));
        order.touch(win(1, 2));

        assert_eq!(order.position(win(1, 2)), Some(0));
        assert_eq!(order.position(win(1, 1)), Some(1));
        assert_eq!(order.position(win(7, 7)), None);
    }

    #[test]
    fn a_fresh_order_is_empty() {
        let order = FocusOrder::default();
        assert!(order.is_empty());
        assert_eq!(order.iter().next(), None);
    }
}
