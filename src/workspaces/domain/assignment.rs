//! Which workspace each window belongs to. One assignment per window, indexed both ways, so
//! "which workspace owns this window" and "which windows does this workspace hold" are one lookup
//! each and can never disagree. See "One workspace list" in `src/workspaces/docs/workspaces-and-displays.md`.
use rini_core::ids::SpaceId;
use rini_core::ids::{WindowId, pid_t};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use serde::{Deserialize, Serialize};

use crate::workspaces::VirtualWorkspaceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowWorkspaceInfo {
    pub space: SpaceId,
    pub workspace_id: VirtualWorkspaceId,
}

#[derive(Debug, Default)]
pub struct WorkspaceAssignments {
    by_window: HashMap<WindowId, WindowWorkspaceInfo>,
    by_workspace: HashMap<WindowWorkspaceInfo, HashSet<WindowId>>,
}

impl WorkspaceAssignments {
    /// Assigns `window` to `assignment`, returning the assignment it replaced.
    pub fn assign(
        &mut self,
        window: WindowId,
        assignment: WindowWorkspaceInfo,
    ) -> Option<WindowWorkspaceInfo> {
        let old = self.by_window.insert(window, assignment);
        if let Some(old) = old {
            self.unindex(window, old);
        }
        self.by_workspace.entry(assignment).or_default().insert(window);
        old
    }

    pub fn remove(&mut self, window: WindowId) -> Option<WindowWorkspaceInfo> {
        let old = self.by_window.remove(&window)?;
        self.unindex(window, old);
        Some(old)
    }

    pub fn remove_all_for_pid(&mut self, pid: pid_t) {
        let windows: Vec<_> = self.by_window.keys().copied().filter(|w| w.pid == pid).collect();
        for window in windows {
            self.remove(window);
        }
    }

    /// Carries `from`'s assignment over to `to`, replacing whatever `to` had. A no-op when `from`
    /// has none.
    pub fn transfer(&mut self, from: WindowId, to: WindowId) {
        if from == to {
            return;
        }
        if let Some(assignment) = self.remove(from) {
            self.assign(to, assignment);
        }
    }

    pub fn info_for_window(&self, window: WindowId) -> Option<WindowWorkspaceInfo> {
        self.by_window.get(&window).copied()
    }

    pub fn workspace_for_window(
        &self,
        space: SpaceId,
        window: WindowId,
    ) -> Option<VirtualWorkspaceId> {
        self.info_for_window(window)
            .filter(|a| a.space == space)
            .map(|a| a.workspace_id)
    }

    /// The windows on `workspace_id` on `space`, in a stable order (by pid, then index).
    pub fn windows(&self, space: SpaceId, workspace_id: VirtualWorkspaceId) -> Vec<WindowId> {
        let key = WindowWorkspaceInfo { space, workspace_id };
        let mut windows: Vec<_> = self
            .by_workspace
            .get(&key)
            .into_iter()
            .flat_map(|w| w.iter().copied())
            .collect();
        windows.sort_unstable_by_key(|wid| (wid.pid, wid.idx.get()));
        windows
    }

    pub fn window_count(&self, space: SpaceId, workspace_id: VirtualWorkspaceId) -> usize {
        let key = WindowWorkspaceInfo { space, workspace_id };
        self.by_workspace.get(&key).map_or(0, HashSet::len)
    }

    pub fn has_assignments_in_space(&self, space: SpaceId) -> bool {
        self.by_workspace.keys().any(|key| key.space == space)
    }

    pub fn iter(&self) -> impl Iterator<Item = (WindowId, WindowWorkspaceInfo)> + '_ {
        self.by_window.iter().map(|(&w, &a)| (w, a))
    }

    pub fn spaces(&self) -> Vec<SpaceId> {
        let mut spaces: Vec<SpaceId> = self.by_window.values().map(|a| a.space).collect();
        spaces.sort_unstable();
        spaces.dedup();
        spaces
    }

    pub fn len(&self) -> usize {
        self.by_window.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_window.is_empty()
    }

    /// Every assignment on `old_space` moves to `new_space`; a workspace that already had windows
    /// on `new_space` keeps them (macOS has usually already placed windows on the incoming id).
    pub fn remap_space(&mut self, old_space: SpaceId, new_space: SpaceId) {
        if old_space == new_space {
            return;
        }
        let moved: Vec<_> =
            self.by_workspace.keys().copied().filter(|key| key.space == old_space).collect();
        for old_key in moved {
            if let Some(windows) = self.by_workspace.remove(&old_key) {
                let new_key = WindowWorkspaceInfo {
                    space: new_space,
                    workspace_id: old_key.workspace_id,
                };
                self.by_workspace.entry(new_key).or_default().extend(windows);
            }
        }
        for assignment in self.by_window.values_mut() {
            if assignment.space == old_space {
                assignment.space = new_space;
            }
        }
    }

    fn unindex(&mut self, window: WindowId, assignment: WindowWorkspaceInfo) {
        if let Some(windows) = self.by_workspace.get_mut(&assignment) {
            windows.remove(&window);
            if windows.is_empty() {
                self.by_workspace.remove(&assignment);
            }
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn debug_assert_invariants(&self) {
        for (window, assignment) in &self.by_window {
            debug_assert!(
                self.by_workspace.get(assignment).is_some_and(|w| w.contains(window)),
                "{window:?} assigned to {assignment:?} but not indexed under it"
            );
        }
        for (assignment, windows) in &self.by_workspace {
            debug_assert!(!windows.is_empty(), "empty index entry for {assignment:?}");
            for window in windows {
                debug_assert_eq!(self.by_window.get(window), Some(assignment));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::domain::virtual_workspace::WorkspaceStore;

    fn two_workspaces(space: SpaceId) -> (VirtualWorkspaceId, VirtualWorkspaceId) {
        let mut store = WorkspaceStore::new();
        let a = store.create_workspace(space, Some("a".into())).unwrap();
        let b = store.create_workspace(space, Some("b".into())).unwrap();
        (a, b)
    }

    #[test]
    fn a_window_has_one_assignment_and_reassigning_moves_it() {
        let space = SpaceId::new(1);
        let (a, b) = two_workspaces(space);
        let mut idx = WorkspaceAssignments::default();
        let w = WindowId::new(1, 1);
        assert_eq!(
            idx.assign(w, WindowWorkspaceInfo { space, workspace_id: a }),
            None
        );
        assert_eq!(idx.windows(space, a), vec![w]);
        let old = idx.assign(w, WindowWorkspaceInfo { space, workspace_id: b });
        assert_eq!(old.map(|o| o.workspace_id), Some(a));
        assert!(idx.windows(space, a).is_empty());
        assert_eq!(idx.windows(space, b), vec![w]);
        assert_eq!(idx.workspace_for_window(space, w), Some(b));
        assert_eq!(idx.workspace_for_window(SpaceId::new(2), w), None, "other space");
        idx.debug_assert_invariants();
    }

    #[test]
    fn windows_come_back_in_pid_then_index_order() {
        let space = SpaceId::new(1);
        let (a, _) = two_workspaces(space);
        let mut idx = WorkspaceAssignments::default();
        for w in [
            WindowId::new(2, 1),
            WindowId::new(1, 2),
            WindowId::new(1, 1),
        ] {
            idx.assign(w, WindowWorkspaceInfo { space, workspace_id: a });
        }
        assert_eq!(
            idx.windows(space, a),
            vec![
                WindowId::new(1, 1),
                WindowId::new(1, 2),
                WindowId::new(2, 1)
            ]
        );
        assert_eq!(idx.window_count(space, a), 3);
    }

    #[test]
    fn transfer_replaces_the_targets_assignment_and_clears_the_source() {
        let space = SpaceId::new(1);
        let (a, b) = two_workspaces(space);
        let mut idx = WorkspaceAssignments::default();
        let (from, to) = (WindowId::new(1, 1), WindowId::new(1, 2));
        idx.assign(from, WindowWorkspaceInfo { space, workspace_id: a });
        idx.assign(to, WindowWorkspaceInfo { space, workspace_id: b });
        idx.transfer(from, to);
        assert_eq!(idx.info_for_window(from), None);
        assert_eq!(idx.workspace_for_window(space, to), Some(a));
        assert!(idx.windows(space, b).is_empty());
        assert_eq!(idx.len(), 1);
        idx.debug_assert_invariants();
    }

    #[test]
    fn remap_space_merges_into_windows_already_on_the_new_space() {
        let (old, new) = (SpaceId::new(1), SpaceId::new(2));
        let mut store = WorkspaceStore::new();
        let a = store.create_workspace(old, Some("a".into())).unwrap();
        let mut idx = WorkspaceAssignments::default();
        idx.assign(
            WindowId::new(1, 1),
            WindowWorkspaceInfo { space: old, workspace_id: a },
        );
        idx.assign(
            WindowId::new(1, 2),
            WindowWorkspaceInfo { space: new, workspace_id: a },
        );
        idx.remap_space(old, new);
        assert_eq!(
            idx.windows(new, a),
            vec![WindowId::new(1, 1), WindowId::new(1, 2)]
        );
        assert!(idx.windows(old, a).is_empty());
        assert_eq!(idx.spaces(), vec![new]);
        idx.debug_assert_invariants();
    }

    #[test]
    fn removing_an_apps_windows_leaves_other_apps_alone() {
        let space = SpaceId::new(1);
        let (a, _) = two_workspaces(space);
        let mut idx = WorkspaceAssignments::default();
        idx.assign(
            WindowId::new(1, 1),
            WindowWorkspaceInfo { space, workspace_id: a },
        );
        idx.assign(
            WindowId::new(2, 1),
            WindowWorkspaceInfo { space, workspace_id: a },
        );
        idx.remove_all_for_pid(1);
        assert_eq!(idx.windows(space, a), vec![WindowId::new(2, 1)]);
        assert!(idx.has_assignments_in_space(space));
        idx.remove(WindowId::new(2, 1));
        assert!(idx.is_empty());
        assert!(!idx.has_assignments_in_space(space));
    }
}
