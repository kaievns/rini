//! What an application reports of its windows: each window it lists on a space is admitted under
//! its rules, its strip membership is reconciled with what it reported, and a window added or
//! removed one at a time joins or leaves its workspace.

use rini_core::ids::{SpaceId, WindowId, pid_t};
use rustc_hash::FxHashMap as HashMap;
use tracing::warn;

use crate::layout::WindowLayoutConstraints;
use crate::windows::domain::info::AppInfo;
use crate::workspaces::domain::app_rules::{AppRuleOutcome, AppRuleWorkspaceFocus};
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId;
use crate::workspaces::engine::{EventResponse, LayoutEngine, OnScreenEntry};
use crate::workspaces::{AppRuleEffects, AppRuleResult, WindowStore};

/// The layout constraints `entry` reports.
fn constraints_of(entry: &OnScreenEntry) -> WindowLayoutConstraints {
    WindowLayoutConstraints {
        is_resizable: entry.is_resizable,
        locked_width: entry.size.width,
        locked_height: entry.size.height,
        min_width: entry.min_size.map_or(0.0, |s| s.width),
        min_height: entry.min_size.map_or(0.0, |s| s.height),
        max_width: entry.max_size.map_or(0.0, |s| s.width),
        max_height: entry.max_size.map_or(0.0, |s| s.height),
    }
    .normalized()
}

/// Whether going from `previous` to `now` changes a layout: only a change into or out of
/// constraints that bind does.
fn constraints_change_layout(
    previous: Option<WindowLayoutConstraints>,
    now: WindowLayoutConstraints,
) -> bool {
    previous != Some(now)
        && (now.constrains_layout() || previous.is_some_and(|old| old.constrains_layout()))
}

impl LayoutEngine {
    /// Every window an app has on a space, as the app itself reports them.
    ///
    /// The authoritative list: windows missing from it are gone, and windows in it rini has not
    /// placed are new. App rules are applied here, because this is where a window first arrives with
    /// its bundle id and title known.
    #[allow(clippy::too_many_arguments, reason = "no roadmap task groups them")]
    pub(super) fn on_windows_on_screen_updated(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        space: SpaceId,
        pid: pid_t,
        windows_with_titles: Vec<OnScreenEntry>,
        app_info: Option<AppInfo>,
        app_rule_outcome: &mut AppRuleOutcome,
    ) -> EventResponse {
        self.debug_tree(space);
        self.floating.clear_active_for_app(space, pid);
        let mut constraints_changed = false;
        // Floating windows are left out: it is the strip's membership.
        let mut tiled_by_workspace: HashMap<VirtualWorkspaceId, Vec<WindowId>> = HashMap::default();
        let app_bundle_id = app_info.as_ref().and_then(|info| info.bundle_id.as_deref());
        let mut focus_request = None;

        for entry in windows_with_titles {
            let wid = entry.wid;
            self.observe_window_for_persistence(
                window_store,
                memory,
                space,
                wid,
                entry.title.as_deref(),
                entry.size,
                app_bundle_id,
            );
            constraints_changed |= self.note_constraints(&entry);

            let was_floating = self.floating.is_floating(wid);
            let Some(outcome) = self.admit_window(
                window_store,
                memory,
                space,
                &entry,
                app_info.as_ref(),
                was_floating,
            ) else {
                continue;
            };
            if let Some(request) = self.apply_app_rule_outcome(
                window_store,
                wid,
                space,
                was_floating,
                outcome,
                app_rule_outcome,
                &mut tiled_by_workspace,
            ) {
                focus_request = Some(request);
            }
        }

        self.sync_tiled_windows_for_app(window_store, memory, space, pid, &tiled_by_workspace);
        self.on_screen_response(
            window_store,
            space,
            focus_request,
            constraints_changed,
            app_rule_outcome,
        )
    }

    /// Records `entry`'s constraints, and says whether they change its layout. They arrive late,
    /// so a change has to ask for the pass itself; see "Column width and what the window will
    /// accept" in `src/layout/docs/strip.md`.
    fn note_constraints(&mut self, entry: &OnScreenEntry) -> bool {
        let constraints = constraints_of(entry);
        let previous = self.window_layout_constraints.insert(entry.wid, constraints);
        constraints_change_layout(previous, constraints)
    }

    /// What `entry`'s rules make of it, else the workspace auto-assignment gives it with its
    /// floating left as it was. `None`, logged, when neither finds it a workspace.
    fn admit_window(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        space: SpaceId,
        entry: &OnScreenEntry,
        app_info: Option<&AppInfo>,
        was_floating: bool,
    ) -> Option<AppRuleResult> {
        let wid = entry.wid;
        let ruled = self.assign_window_with_app_info(
            window_store,
            memory,
            wid,
            space,
            app_info.and_then(|info| info.bundle_id.as_deref()),
            app_info.and_then(|info| info.localized_name.as_deref()),
            entry.title.as_deref(),
            entry.ax_role.as_deref(),
            entry.ax_subrole.as_deref(),
            entry.is_modal,
        );
        if let Ok(outcome) = ruled {
            return Some(outcome);
        }
        match self.virtual_workspace_manager.auto_assign_window(window_store, wid, space) {
            Ok(ws) => Some(AppRuleResult::Managed(AppRuleEffects {
                workspace_id: ws,
                floating: was_floating,
                position: None,
                size: None,
                focus: false,
                prev_rule_decision: false,
            })),
            Err(_) => {
                warn!(
                    "Could not determine workspace for window {:?} on space {:?}; skipping assignment",
                    wid, space
                );
                None
            }
        }
    }

    /// The answer to an app's report: a rule's focus request is handed to the reactor when it
    /// names a workspace not showing, and committed here when it names the one that is.
    fn on_screen_response(
        &mut self,
        window_store: &mut WindowStore,
        space: SpaceId,
        focus_request: Option<(WindowId, VirtualWorkspaceId)>,
        constraints_changed: bool,
        app_rule_outcome: &mut AppRuleOutcome,
    ) -> EventResponse {
        if let Some((window, workspace)) = focus_request {
            let workspace_index = self
                .virtual_workspace_manager_mut()
                .list_workspaces(space)
                .iter()
                .position(|(id, _)| *id == workspace);
            if let Some(workspace_index) = workspace_index
                && self.virtual_workspace_manager.active_workspace(space) != Some(workspace)
            {
                app_rule_outcome.set_workspace_focus(AppRuleWorkspaceFocus {
                    window,
                    space,
                    workspace_index,
                });
                return EventResponse {
                    changed: constraints_changed,
                    ..EventResponse::default()
                };
            }
            self.commit_workspace_focus(window_store, space, Some(window));
            return EventResponse {
                changed: app_rule_outcome.has_resizes() || constraints_changed,
                raise_windows: vec![window],
                focus_window: Some(window),
                edge_hit: None,
                moved: None,
            };
        }
        EventResponse {
            changed: app_rule_outcome.has_resizes() || constraints_changed,
            ..EventResponse::default()
        }
    }

    pub(super) fn sync_tiled_windows_for_app(
        &mut self,
        window_store: &WindowStore,
        memory: &mut DisplayMemory,
        space: SpaceId,
        pid: pid_t,
        tiled_by_workspace: &HashMap<crate::workspaces::VirtualWorkspaceId, Vec<WindowId>>,
    ) {
        let total_tiled_count: usize = tiled_by_workspace.values().map(|v| v.len()).sum();

        for (ws_id, layout) in self.workspace_layouts.active_layouts_for_space(space) {
            let mut desired = tiled_by_workspace.get(&ws_id).cloned().unwrap_or_default();
            for wid in self.virtual_workspace_manager.workspace_windows(window_store, space, ws_id)
            {
                let authoritative_native_space =
                    window_store.current_window_server_space_for_window(wid);
                // Skip re-adding if the VWM no longer assigns this window to this space
                // (it was moved to another space during this discovery cycle).
                if wid.pid != pid
                    || self.floating.is_floating(wid)
                    || desired.contains(&wid)
                    || authoritative_native_space.is_some_and(|native_space| native_space != space)
                    || self.window_no_longer_assigned_to_space(window_store, space, wid)
                {
                    continue;
                }
                desired.push(wid);
            }

            if desired.is_empty() && total_tiled_count == 0 {
                // Empty discovery can mean AX temporarily omitted the app. Preserve
                // windows still assigned to this workspace, but allow moved windows
                // to be removed from this layout tree.
                let tree_windows = self.workspace_tree(ws_id).windows_for_app(layout, pid);
                desired = tree_windows
                    .into_iter()
                    .filter(|wid| {
                        self.virtual_workspace_manager.workspace_for_window(
                            window_store,
                            space,
                            *wid,
                        ) == Some(ws_id)
                    })
                    .collect();
            }

            desired.sort_unstable();
            let mut current = self.workspace_tree(ws_id).windows_for_app(layout, pid);
            current.sort_unstable();

            // AX/window-server discovery can temporarily omit windows. Keep windows that are
            // still assigned to this workspace so a partial snapshot does not tear them out
            // of the tree and cause their sibling weights to be rebuilt.
            for wid in current.iter().copied() {
                if !desired.contains(&wid)
                    && !self.floating.is_floating(wid)
                    && self.virtual_workspace_manager.workspace_for_window(window_store, space, wid)
                        == Some(ws_id)
                {
                    desired.push(wid);
                }
            }
            desired.sort_unstable();
            if desired == current {
                continue;
            }

            // Per-app membership reconciliation is not a focus operation.
            // Several layout systems select newly inserted windows as part of
            // their normal insertion semantics, so preserve the selection
            // explicitly across discovery-driven synchronization.
            let selected_window = self.workspace_tree(ws_id).selected_window(layout);
            // Windows entering the tree here, which is where a launching application's windows arrive.
            // A fresh column starts at the display's default ratio, so each one needs whatever it last
            // had re-applied — the record alone does not decide how wide a column is.
            let arriving: Vec<WindowId> =
                desired.iter().copied().filter(|wid| !current.contains(wid)).collect();
            self.workspace_tree_mut(ws_id).set_windows_for_app(layout, pid, desired);
            for wid in arriving {
                self.apply_remembered_column_width(memory, space, ws_id, layout, wid);
            }
            if let Some(selected_window) = selected_window
                && self.workspace_tree(ws_id).contains_window(layout, selected_window)
            {
                let _ = self.workspace_tree_mut(ws_id).select_window(layout, selected_window);
            }
        }
    }

    fn window_no_longer_assigned_to_space(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        wid: WindowId,
    ) -> bool {
        self.virtual_workspace_manager
            .workspace_for_window(window_store, space, wid)
            .is_none()
    }

    pub(super) fn add_window_to_layout(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        space: SpaceId,
        wid: WindowId,
    ) -> bool {
        let active_space_before = self.space_with_window(wid);

        // Where this application's windows were the last time this set of displays was attached. Only
        // consulted for a window with no workspace yet, so it cannot override an explicit rule or a
        // window rini has already placed. See `src/workspaces/docs/launch-memory.md` (in this crate).
        let remembered = self
            .virtual_workspace_manager
            .workspace_for_window(window_store, space, wid)
            .is_none()
            .then(|| {
                let state = window_store.window(wid)?;
                let app_id = state.info.bundle_id.clone()?;
                let title = state.info.title.clone();
                self.launch_slot_for_new_window(
                    window_store,
                    memory,
                    wid,
                    &app_id,
                    Some(title.as_str()),
                )
            })
            .flatten();
        if let Some(slot) = &remembered {
            let workspaces = self.virtual_workspace_manager.list_workspaces(space);
            if let Some((workspace_id, _)) = workspaces.get(slot.workspace_index) {
                let workspace_id = *workspace_id;
                self.virtual_workspace_manager.assign_window_to_workspace(
                    window_store,
                    space,
                    wid,
                    workspace_id,
                );
            }
            memory.affinity.set_window_home_if_absent(wid, &slot.display_uuid);
            if let Some(width) = slot.width {
                memory.affinity.set_window_width(&slot.display_uuid, wid, width);
            }
        }

        let assigned_workspace =
            match self.virtual_workspace_manager.workspace_for_window(window_store, space, wid) {
                Some(workspace_id) => workspace_id,
                None => match self.virtual_workspace_manager.auto_assign_window(
                    window_store,
                    wid,
                    space,
                ) {
                    Ok(workspace_id) => workspace_id,
                    Err(e) => {
                        warn!("Failed to auto-assign window to workspace: {:?}", e);
                        let Some(active) = self.virtual_workspace_manager.active_workspace(space)
                        else {
                            warn!(
                                "No active workspace on space {:?}; window {:?} not added",
                                space, wid
                            );
                            return false;
                        };
                        active
                    }
                },
            };

        // Establish a home for a window Rini has not placed before. Absent-only, so the
        // reassignment that follows an unplug cannot overwrite the home a window already
        // has — that record is what brings it back when its display returns.
        self.note_window_display_home(memory, wid, space);

        let should_be_floating = self.floating.is_floating(wid);

        if should_be_floating {
            self.floating.add_active(space, wid.pid, wid);
        } else if let Some(layout) = self.workspace_layouts.active(space, assigned_workspace) {
            if !self.workspace_tree(assigned_workspace).contains_window(layout, wid) {
                self.workspace_tree_mut(assigned_workspace)
                    .add_window_after_selection(layout, wid);
                // A fresh column starts at the display's default ratio, so re-apply whatever this
                // window last had here. Without this a relaunched window was recorded and looked up
                // correctly and still opened at the default: the width reached the affinity record and
                // never reached the layout, which is the thing that decides how wide the column is.
                self.apply_remembered_column_width(memory, space, assigned_workspace, layout, wid);
            }
        } else {
            warn!(
                "No active layout for workspace {:?} on space {:?}; window {:?} not added to tree",
                assigned_workspace, space, wid
            );
        }

        self.space_with_window(wid) != active_space_before
    }

    pub(super) fn remove_window_internal(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        wid: WindowId,
        preserve_floating: bool,
    ) {
        self.remove_window_layout_membership(window_store, wid);

        if preserve_floating {
            self.floating.remove_active_for_window(wid);
        } else {
            self.floating.remove_floating(wid);
        }

        if !preserve_floating {
            self.virtual_workspace_manager.remove_window(window_store, wid);
            self.floating_positions.remove_window(wid);
            self.forget_persisted_window(memory, wid);
        }

        if self.focused_window == Some(wid) {
            self.focused_window = None;
        }
        self.window_layout_constraints.remove(&wid);
    }

    fn remove_window_layout_membership(&mut self, window_store: &WindowStore, wid: WindowId) {
        let tiled_workspaces =
            self.virtual_workspace_manager.workspaces_for_window(window_store, wid);

        if !tiled_workspaces.is_empty() {
            for ws_id in &tiled_workspaces {
                self.workspace_tree_mut(*ws_id).remove_window(wid);
            }
            return;
        }

        // The store may already have dropped the record (for example after
        // WindowDestroyed). Layout membership is only a projection, so scrub
        // every tree when its authoritative assignment is unavailable.
        let ws_ids: Vec<_> = self.virtual_workspace_manager.workspaces.keys().collect();
        for ws_id in ws_ids {
            self.workspace_tree_mut(ws_id).remove_window(wid);
        }
    }

    pub(super) fn remove_window_from_all_tiling_trees(&mut self, wid: WindowId) {
        let ws_ids: Vec<_> = self.virtual_workspace_manager.workspaces.keys().collect();
        for ws_id in ws_ids {
            self.workspace_tree_mut(ws_id).remove_window(wid);
        }
    }

    pub(super) fn space_with_window(&self, wid: WindowId) -> Option<SpaceId> {
        for space in self.workspace_layouts.spaces() {
            if let Some(ws_id) = self.virtual_workspace_manager.active_workspace(space)
                && let Some(layout) = self.workspace_layouts.active(space, ws_id)
                && self.workspace_tree(ws_id).contains_window(layout, wid)
            {
                return Some(space);
            }

            if self.floating.active_flat(space).contains(&wid) {
                return Some(space);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;
    use crate::workspaces::engine::on_screen_entry;

    #[test]
    fn an_entry_reports_its_size_as_locked_and_a_maximum_under_its_minimum_as_the_minimum() {
        let entry = OnScreenEntry {
            size: CGSize::new(300.0, 200.0),
            min_size: Some(CGSize::new(100.0, 50.0)),
            max_size: Some(CGSize::new(60.0, 0.0)),
            ..on_screen_entry(WindowId::new(1, 1), None)
        };
        assert_eq!(constraints_of(&entry), WindowLayoutConstraints {
            is_resizable: true,
            locked_width: 300.0,
            locked_height: 200.0,
            min_width: 100.0,
            min_height: 50.0,
            max_width: 100.0,
            max_height: 0.0,
        });
    }

    /// Discovering a window that reports nothing must not ask for a layout pass; limits arriving
    /// late, or going away, must.
    #[test]
    fn only_constraints_that_bind_or_stop_binding_change_a_layout() {
        let free = WindowLayoutConstraints {
            is_resizable: true,
            ..WindowLayoutConstraints::default()
        };
        let fixed = WindowLayoutConstraints { is_resizable: false, ..free };
        let wide = WindowLayoutConstraints { min_width: 900.0, ..free };
        assert!(!constraints_change_layout(None, free));
        assert!(!constraints_change_layout(Some(free), fixed));
        assert!(constraints_change_layout(None, wide));
        assert!(constraints_change_layout(Some(free), wide));
        assert!(!constraints_change_layout(Some(wide), wide));
        assert!(constraints_change_layout(Some(wide), free));
    }
}
