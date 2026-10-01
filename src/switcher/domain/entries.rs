//! What a switch steps between: one window, or, for the every-window switch, one application's stack.
//!
//! Built on the flat, already ordered `switch_list`, so recency is decided in one place. An
//! application sits where its most recent window sits, and an application with no focused window
//! falls into the stable tail by its first window there.
//!
//! An application is a process, the unit `Scope::App` filters on, so cmd-` from any window of a stack
//! offers exactly that stack's windows.

use rini_core::ids::WindowId;

use crate::switcher::domain::candidates::{Candidate, Scope};

/// How many of a stack's windows are drawn behind its top one.
pub const BEHIND: usize = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Never empty. `windows[0]` is drawn on top and is what choosing the entry focuses.
    windows: Vec<Candidate>,
    stacked: bool,
}

impl Entry {
    pub fn front(&self) -> &Candidate {
        &self.windows[0]
    }

    /// The windows drawn behind the top one, nearest first.
    pub fn behind(&self) -> &[Candidate] {
        &self.windows[1..self.windows.len().min(1 + BEHIND)]
    }

    /// Every window the entry stands for, drawn or not.
    pub fn count(&self) -> usize {
        self.windows.len()
    }

    pub fn holds(&self, window: WindowId) -> bool {
        self.windows.iter().any(|candidate| candidate.window == window)
    }

    /// The caption's second line: a stack's window count, or a lone window's title.
    pub fn detail(&self) -> String {
        if !self.stacked {
            return self.front().title.clone();
        }
        match self.count() {
            1 => "1 window".to_owned(),
            count => format!("{count} windows"),
        }
    }
}

/// `list` as the entries a switch over `scope` steps between, in `list`'s order.
pub fn entries(list: Vec<Candidate>, scope: Scope) -> Vec<Entry> {
    if !scope.stacks() {
        return list
            .into_iter()
            .map(|candidate| Entry {
                windows: vec![candidate],
                stacked: false,
            })
            .collect();
    }
    let mut stacks: Vec<Vec<Candidate>> = Vec::new();
    for candidate in list {
        match stacks.iter_mut().find(|stack| stack[0].window.pid == candidate.window.pid) {
            Some(stack) => stack.push(candidate),
            None => stacks.push(vec![candidate]),
        }
    }
    stacks.into_iter().map(|windows| Entry { windows, stacked: true }).collect()
}

/// What one of a row's `BEHIND` card slots shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardBehind {
    Shows(WindowId),
    /// Past the row's depth. The panel reuses its layers between switches, so a slot not hidden here
    /// keeps showing the last switch's window behind a lone one.
    Hidden,
}

/// Every card slot behind a row's top card, nearest first, for the windows it draws behind.
pub fn cards_behind(behind: &[WindowId]) -> [CardBehind; BEHIND] {
    std::array::from_fn(|depth| match behind.get(depth) {
        Some(window) => CardBehind::Shows(*window),
        None => CardBehind::Hidden,
    })
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;
    use rini_core::ids::{SpaceId, pid_t};

    use super::*;
    use crate::switcher::domain::candidates::{opening_selection, switch_list};
    use crate::switcher::domain::selection::Selection;
    use crate::windows::domain::focus_order::FocusOrder;
    use crate::workspaces::VirtualWorkspaceId;

    fn win(pid: pid_t, idx: u32) -> WindowId {
        WindowId::new(pid, idx)
    }

    fn workspace(n: usize) -> VirtualWorkspaceId {
        slotmap::KeyData::from_ffi((1u64 << 32) | (n as u64 + 1)).into()
    }

    fn candidate(pid: pid_t, idx: u32, space: u64, workspace_at: usize) -> Candidate {
        Candidate {
            window: win(pid, idx),
            space: SpaceId::new(space),
            workspace: workspace(workspace_at),
            workspace_index: workspace_at,
            size: CGSize::new(800.0, 600.0),
            title: format!("window {idx}"),
            app_name: format!("app {pid}"),
        }
    }

    /// Recency as `order` lists it, most recent first.
    fn focused(windows: &[WindowId]) -> FocusOrder {
        let mut order = FocusOrder::default();
        for window in windows.iter().rev() {
            order.touch(*window);
        }
        order
    }

    fn stacked(list: Vec<Candidate>, order: &FocusOrder) -> Vec<Entry> {
        entries(switch_list(list, order, Scope::Everything), Scope::Everything)
    }

    fn members(entry: &Entry) -> Vec<WindowId> {
        entry.windows.iter().map(|candidate| candidate.window).collect()
    }

    #[test]
    fn every_window_groups_into_one_stack_per_application() {
        let list = vec![
            candidate(1, 1, 1, 0),
            candidate(2, 1, 1, 0),
            candidate(1, 2, 1, 1),
            candidate(1, 3, 99, 0),
        ];
        let stacks = stacked(list, &FocusOrder::default());
        assert_eq!(stacks.len(), 2);
        assert_eq!(members(&stacks[0]), vec![win(1, 1), win(1, 2), win(1, 3)]);
        assert_eq!(members(&stacks[1]), vec![win(2, 1)]);
    }

    #[test]
    fn applications_are_ordered_by_their_most_recent_window() {
        let list = vec![
            candidate(1, 1, 1, 0),
            candidate(1, 2, 1, 0),
            candidate(2, 1, 1, 0),
        ];
        let order = focused(&[win(1, 1), win(2, 1), win(1, 2)]);
        let stacks = stacked(list, &order);
        assert_eq!(members(&stacks[0]), vec![win(1, 1), win(1, 2)]);
        assert_eq!(members(&stacks[1]), vec![win(2, 1)]);

        let order = focused(&[win(2, 1), win(1, 2), win(1, 1)]);
        let list = vec![
            candidate(1, 1, 1, 0),
            candidate(1, 2, 1, 0),
            candidate(2, 1, 1, 0),
        ];
        let stacks = stacked(list, &order);
        assert_eq!(members(&stacks[0]), vec![win(2, 1)]);
        assert_eq!(members(&stacks[1]), vec![win(1, 2), win(1, 1)]);
    }

    #[test]
    fn a_stack_puts_its_most_recent_window_on_top() {
        let list = vec![
            candidate(1, 1, 1, 0),
            candidate(1, 2, 1, 0),
            candidate(1, 3, 1, 0),
        ];
        let stacks = stacked(list, &focused(&[win(1, 3), win(1, 1), win(1, 2)]));
        assert_eq!(stacks[0].front().window, win(1, 3));
        assert_eq!(members(&stacks[0]), vec![win(1, 3), win(1, 1), win(1, 2)]);
    }

    #[test]
    fn a_stack_draws_at_most_two_windows_behind_the_top_one() {
        let list = (1..=5).map(|idx| candidate(1, idx, 1, 0)).collect();
        let stacks = stacked(list, &FocusOrder::default());
        assert_eq!(
            stacks[0].behind().iter().map(|c| c.window).collect::<Vec<_>>(),
            vec![win(1, 2), win(1, 3)]
        );
        assert_eq!(stacks[0].count(), 5);
        assert_eq!(
            stacks[0].detail(),
            "5 windows",
            "the count covers the windows not drawn"
        );
    }

    #[test]
    fn an_application_with_one_window_is_a_stack_of_one() {
        let stacks = stacked(vec![candidate(1, 1, 1, 0)], &FocusOrder::default());
        assert!(stacks[0].behind().is_empty());
        assert_eq!(stacks[0].detail(), "1 window");
    }

    #[test]
    fn a_stack_counts_its_windows() {
        let list = (1..=3).map(|idx| candidate(1, idx, 1, 0)).collect();
        let stacks = stacked(list, &FocusOrder::default());
        assert_eq!(stacks[0].count(), 3);
        assert_eq!(stacks[0].detail(), "3 windows");
    }

    #[test]
    fn a_card_slot_past_the_rows_depth_is_hidden() {
        assert_eq!(cards_behind(&[]), [CardBehind::Hidden, CardBehind::Hidden]);
        assert_eq!(
            cards_behind(&[win(1, 2)]),
            [CardBehind::Shows(win(1, 2)), CardBehind::Hidden]
        );
        assert_eq!(
            cards_behind(&[win(1, 2), win(1, 3)]),
            [CardBehind::Shows(win(1, 2)), CardBehind::Shows(win(1, 3))]
        );
    }

    #[test]
    fn an_entry_holds_every_window_it_stands_for() {
        let list = (1..=5).map(|idx| candidate(1, idx, 1, 0)).collect();
        let stacks = stacked(list, &FocusOrder::default());
        for idx in 1..=5 {
            assert!(stacks[0].holds(win(1, idx)), "window {idx}");
        }
        assert!(!stacks[0].holds(win(2, 1)));
    }

    #[test]
    fn two_processes_are_two_stacks() {
        let mut first = candidate(1, 1, 1, 0);
        let mut second = candidate(2, 1, 1, 0);
        first.app_name = "Chrome".to_owned();
        second.app_name = "Chrome".to_owned();
        assert_eq!(stacked(vec![first, second], &FocusOrder::default()).len(), 2);
    }

    #[test]
    fn the_workspace_and_application_switches_are_one_entry_per_window() {
        let list = vec![candidate(7, 1, 1, 0), candidate(7, 2, 1, 0)];
        for scope in [Scope::Workspace(workspace(0)), Scope::App(7)] {
            let flat = entries(switch_list(list.clone(), &FocusOrder::default(), scope), scope);
            assert_eq!(flat.len(), 2, "{scope:?}");
            assert_eq!(
                flat[0].detail(),
                "window 1",
                "a window entry is captioned by its title"
            );
            assert_eq!(flat[0].count(), 1);
            assert!(flat[0].behind().is_empty());
        }
    }

    /// An application with no focus record has no recency to sort by, so it falls into the stable tail
    /// by its first window there, after every application that has one.
    #[test]
    fn never_focused_applications_keep_a_stable_order_whatever_order_they_arrive_in() {
        let forwards = vec![
            candidate(3, 1, 2, 0),
            candidate(2, 1, 1, 1),
            candidate(4, 1, 1, 0),
            candidate(2, 2, 1, 0),
            candidate(5, 1, 1, 0),
        ];
        let backwards: Vec<Candidate> = forwards.iter().rev().cloned().collect();
        let order = focused(&[win(5, 1)]);

        let a = stacked(forwards, &order);
        let b = stacked(backwards, &order);

        assert_eq!(a, b);
        assert_eq!(
            a.iter().map(|entry| entry.front().window.pid).collect::<Vec<_>>(),
            vec![5, 2, 4, 3],
            "the focused application, then by first window in the tail: space, workspace, window id"
        );
        assert_eq!(members(&a[1]), vec![win(2, 2), win(2, 1)]);
    }

    #[test]
    fn a_stacked_switch_opens_on_the_second_application() {
        let list = vec![
            candidate(1, 1, 1, 0),
            candidate(1, 2, 1, 0),
            candidate(2, 1, 1, 0),
        ];
        let stacks = stacked(list, &focused(&[win(1, 1), win(1, 2), win(2, 1)]));
        let opening = opening_selection(&stacks).expect("two applications");
        assert_eq!(
            stacks[opening].front().window,
            win(2, 1),
            "the previous application, not the current one's other window"
        );
    }

    #[test]
    fn one_application_has_nowhere_to_switch_to() {
        let list = vec![candidate(1, 1, 1, 0), candidate(1, 2, 1, 0)];
        assert_eq!(opening_selection(&stacked(list, &FocusOrder::default())), None);
    }

    #[test]
    fn stepping_wraps_across_applications() {
        let list = vec![
            candidate(1, 1, 1, 0),
            candidate(1, 2, 1, 0),
            candidate(2, 1, 1, 0),
            candidate(3, 1, 1, 0),
        ];
        let stacks = stacked(list, &focused(&[win(1, 1), win(2, 1), win(1, 2), win(3, 1)]));
        let mut cursor = Selection::new(stacks.len(), 1).expect("three applications");
        let mut landed = vec![stacks[cursor.index()].front().window];
        for _ in 0..3 {
            cursor.step(1);
            landed.push(stacks[cursor.index()].front().window);
        }
        assert_eq!(landed, vec![win(2, 1), win(3, 1), win(1, 1), win(2, 1)]);
        cursor.step(-1);
        cursor.step(-1);
        assert_eq!(stacks[cursor.index()].front().window, win(3, 1), "and backwards");
    }
}
