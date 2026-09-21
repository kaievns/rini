//! The windows the workspaces context works over: the application's window catalogue
//! (`crate::windows::domain::catalogue`) together with each window's workspace assignment. Catalogue reads
//! and writes pass straight through (`Deref`); the operations that touch both halves live here so
//! the two can never disagree.
use std::ops::{Deref, DerefMut};

use rini_core::ids::SpaceId;
use crate::windows::domain::catalogue::{NativeFullscreenRecord, NativeFullscreenTransition, WindowCatalogue};
use rini_core::ids::{WindowId, WindowServerId, pid_t};
use crate::windows::domain::state::WindowState;

pub use crate::windows::domain::catalogue::{
    PendingNativeFullscreenRecord, PendingWindowOperation, WindowPlacement, WindowRecord,
    WindowVisibility,
};

pub use crate::workspaces::domain::assignment::{WindowWorkspaceInfo, WorkspaceAssignments};
use crate::workspaces::VirtualWorkspaceId;

#[derive(Debug, Default)]
pub struct WindowStore {
    catalogue: WindowCatalogue,
    assignments: WorkspaceAssignments,
}

impl Deref for WindowStore {
    type Target = WindowCatalogue;
    fn deref(&self) -> &WindowCatalogue {
        &self.catalogue
    }
}

impl DerefMut for WindowStore {
    fn deref_mut(&mut self) -> &mut WindowCatalogue {
        &mut self.catalogue
    }
}

impl WindowStore {
    pub fn assignments(&self) -> &WorkspaceAssignments {
        &self.assignments
    }

    // ---- assignment, and the catalogue facts that follow from it

    pub fn assign_window_to_workspace(
        &mut self,
        window_id: WindowId,
        assignment: WindowWorkspaceInfo,
    ) -> Option<WindowWorkspaceInfo> {
        let old = self.assignments.assign(window_id, assignment);
        self.catalogue.note_native_fullscreen_assigned_space(window_id, assignment.space);
        old
    }

    pub fn remove_window_assignment(&mut self, window_id: WindowId) -> Option<WindowWorkspaceInfo> {
        let old = self.assignments.remove(window_id);
        self.catalogue.prune_window_record(window_id);
        old
    }

    pub fn workspace_info_for_window(&self, window_id: WindowId) -> Option<WindowWorkspaceInfo> {
        self.assignments.info_for_window(window_id)
    }

    pub fn workspace_for_window(
        &self,
        space: SpaceId,
        window_id: WindowId,
    ) -> Option<VirtualWorkspaceId> {
        self.assignments.workspace_for_window(space, window_id)
    }

    pub fn workspaces_for_window(&self, window_id: WindowId) -> Vec<VirtualWorkspaceId> {
        self.assignments.info_for_window(window_id).map(|a| vec![a.workspace_id]).unwrap_or_default()
    }

    pub fn workspace_windows(
        &self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
    ) -> Vec<WindowId> {
        self.assignments.windows(space, workspace_id)
    }

    pub fn workspace_window_count(&self, space: SpaceId, workspace_id: VirtualWorkspaceId) -> usize {
        self.assignments.window_count(space, workspace_id)
    }

    pub fn has_workspace_assignments_in_space(&self, space: SpaceId) -> bool {
        self.assignments.has_assignments_in_space(space)
    }

    pub fn iter_workspace_assignments(
        &self,
    ) -> impl Iterator<Item = (WindowId, WindowWorkspaceInfo)> + '_ {
        self.assignments.iter()
    }

    pub fn spaces_with_assignments(&self) -> Vec<SpaceId> {
        self.assignments.spaces()
    }

    pub fn workspace_assignment_count(&self) -> usize {
        self.assignments.len()
    }

    // ---- catalogue operations that must keep the assignment in step

    pub fn insert_window(&mut self, window_id: WindowId, window: WindowState) {
        self.catalogue.insert_window(window_id, window);
        self.sync_native_fullscreen_assignment(window_id);
    }

    pub fn track_window_server_id(
        &mut self,
        wsid: WindowServerId,
        window_id: WindowId,
    ) -> Option<WindowId> {
        let old = self.catalogue.track_window_server_id(wsid, window_id);
        self.sync_native_fullscreen_assignment(window_id);
        old
    }

    pub fn suspend_window_to_native_fullscreen(
        &mut self,
        window_id: WindowId,
        window_server_id: Option<WindowServerId>,
        fallback_last_known_user_space: Option<SpaceId>,
        fullscreen_space: SpaceId,
        transition: NativeFullscreenTransition,
    ) -> NativeFullscreenRecord {
        let assigned_space = self.assignments.info_for_window(window_id).map(|a| a.space);
        self.catalogue.suspend_window_to_native_fullscreen(
            window_id,
            window_server_id,
            assigned_space,
            fallback_last_known_user_space,
            fullscreen_space,
            transition,
        )
    }

    pub fn transfer_persistent_window_metadata(&mut self, from: WindowId, to: WindowId) {
        if from == to {
            return;
        }
        let assignment = self.assignments.info_for_window(from);
        self.assignments.transfer(from, to);
        self.catalogue.transfer_persistent_window_metadata(from, to);
        if let Some(assignment) = assignment {
            self.catalogue.note_native_fullscreen_assigned_space(to, assignment.space);
        }
        self.catalogue.prune_window_record(from);
    }

    pub fn remove_window(&mut self, window_id: WindowId) {
        self.assignments.remove(window_id);
        self.catalogue.remove_window(window_id);
    }

    pub fn remove_windows_for_app(&mut self, pid: pid_t) {
        self.assignments.remove_all_for_pid(pid);
        self.catalogue.remove_windows_for_app(pid);
    }

    pub fn remap_space(&mut self, old_space: SpaceId, new_space: SpaceId) {
        self.assignments.remap_space(old_space, new_space);
        self.catalogue.remap_space(old_space, new_space);
    }

    /// A window in native fullscreen keeps the space of its assignment on its record, whichever
    /// half learned of the window first.
    fn sync_native_fullscreen_assignment(&mut self, window_id: WindowId) {
        if let Some(assignment) = self.assignments.info_for_window(window_id) {
            self.catalogue.note_native_fullscreen_assigned_space(window_id, assignment.space);
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn debug_assert_invariants(&self) {
        self.catalogue.debug_assert_invariants();
        self.assignments.debug_assert_invariants();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::domain::virtual_workspace::WorkspaceStore;
    use crate::windows::domain::catalogue::NativeFullscreenTransition;

    #[test]
    fn transfer_persistent_metadata_replaces_existing_target_workspace_assignment() {
        let mut window_store = WindowStore::default();
        let space = SpaceId::new(10);
        let mut workspaces = WorkspaceStore::new();
        let source_workspace = workspaces
            .create_workspace(space, Some("Source".to_string()))
            .expect("source workspace");
        let target_workspace = workspaces
            .create_workspace(space, Some("Target".to_string()))
            .expect("target workspace");
        let from = WindowId::new(1, 1);
        let to = WindowId::new(1, 2);

        window_store.assign_window_to_workspace(
            from,
            WindowWorkspaceInfo {
                space,
                workspace_id: source_workspace,
            },
        );
        window_store.assign_window_to_workspace(
            to,
            WindowWorkspaceInfo {
                space,
                workspace_id: target_workspace,
            },
        );

        window_store.transfer_persistent_window_metadata(from, to);

        assert_eq!(
            window_store.workspace_info_for_window(to),
            Some(WindowWorkspaceInfo {
                space,
                workspace_id: source_workspace,
            })
        );
        assert!(window_store.workspace_windows(space, target_workspace).is_empty());
        assert_eq!(window_store.workspace_windows(space, source_workspace), vec![to]);
    }

    #[test]
    fn transfer_persistent_metadata_rekeys_native_fullscreen_record() {
        let mut window_store = WindowStore::default();
        let space = SpaceId::new(10);
        let fullscreen_space = SpaceId::new(0x400000000 + space.get());
        let mut workspaces = WorkspaceStore::new();
        let workspace_id =
            workspaces.create_workspace(space, Some("Main".to_string())).expect("workspace");
        let from = WindowId::new(1, 1);
        let to = WindowId::new(1, 2);
        let wsid = WindowServerId::new(77);

        window_store.assign_window_to_workspace(from, WindowWorkspaceInfo { space, workspace_id });
        let _ = window_store.suspend_window_to_native_fullscreen(
            from,
            Some(wsid),
            Some(space),
            fullscreen_space,
            NativeFullscreenTransition::Suspended,
        );

        window_store.transfer_persistent_window_metadata(from, to);

        let record = window_store
            .native_fullscreen_record_for_window(to)
            .expect("fullscreen record should follow rekey");
        assert_eq!(record.current_window_id, to);
        assert_eq!(record.window_server_id, Some(wsid));
        assert_eq!(record.assigned_space, Some(space));
        assert_eq!(
            window_store.workspace_info_for_window(to),
            Some(WindowWorkspaceInfo { space, workspace_id })
        );
        assert_eq!(
            window_store
                .native_fullscreen_record_for_window(from)
                .expect("original key should still resolve the lifecycle")
                .current_window_id,
            to
        );
    }

    #[test]
    fn app_cleanup_removes_all_indexes_and_pending_operations() {
        let mut store = WindowStore::default();
        let wid = WindowId::new(4, 1);
        let wsid = WindowServerId::new(55);
        let mut workspaces = WorkspaceStore::new();
        let workspace_id = workspaces
            .create_workspace(SpaceId::new(9), Some("Cleanup".to_string()))
            .expect("workspace");
        store.track_window_server_id(wsid, wid);
        store.assign_window_to_workspace(
            wid,
            WindowWorkspaceInfo {
                space: SpaceId::new(9),
                workspace_id,
            },
        );
        store.begin_operation(wid, None, Some(SpaceId::new(9)));

        store.remove_windows_for_app(wid.pid);

        assert!(store.record(wid).is_none());
        assert_eq!(store.tracked_window_id(wsid), None);
        assert!(store.window_ids_for_pid(wid.pid).next().is_none());
        store.debug_assert_invariants();
    }
}
