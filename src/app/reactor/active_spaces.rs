//! Which spaces rini is active on, and which windows the window server reports on them: the
//! active set recomputed from the screens, and each window's visibility and space reconciled
//! against the active spaces' window lists.

use rini_core::ids::{DisplayUuid, SpaceId, WindowId, WindowServerId};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use tracing::debug;

use super::{Reactor, ScreenInfo};
use crate::displays::domain::space_activation::SpaceActivationConfig;
use crate::workspaces::LayoutEvent;

impl Reactor {
    pub(super) fn set_active_spaces(&mut self, spaces: &[Option<SpaceId>]) {
        self.active_spaces.clear();
        for space in spaces.iter().flatten().copied() {
            self.active_spaces.insert(space);
        }
    }

    pub(super) fn is_space_active(&self, space: SpaceId) -> bool {
        self.active_spaces.contains(&space)
    }

    pub(super) fn iter_active_spaces(&self) -> impl Iterator<Item = SpaceId> + '_ {
        self.active_spaces.iter().copied()
    }

    pub(super) fn is_window_on_active_space(&self, wid: WindowId) -> bool {
        self.affinity()
            .best_space_for_window_id(wid)
            .is_some_and(|space| self.is_space_active(space))
    }

    pub(super) fn activation_cfg(&self) -> SpaceActivationConfig {
        SpaceActivationConfig {
            default_disable: !self.config.settings.activate_spaces_by_default,
        }
    }

    pub(super) fn screens_for_current_spaces(&self) -> Vec<ScreenInfo> {
        self.space_state.screens.clone()
    }

    fn display_uuids_for_current_screens(&self) -> Vec<Option<DisplayUuid>> {
        self.space_state
            .screens
            .iter()
            .map(|screen| screen.display_uuid_owned())
            .collect()
    }

    #[cfg(test)]
    pub(super) fn raw_spaces_for_current_screens(&self) -> Vec<Option<SpaceId>> {
        self.space_state.screens.iter().map(|s| s.space).collect()
    }

    fn expose_space_if_known(&mut self, space: SpaceId) {
        let Some(screen) = self.space_state.screen_by_space(space) else {
            return;
        };
        self.layout_engine.virtual_workspace_manager_mut().list_workspaces(space);
        self.send_layout_event(LayoutEvent::SpaceExposed(space, screen.frame.size));
    }

    pub(super) fn expose_all_spaces(&mut self) {
        let spaces: Vec<SpaceId> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.space)
            .filter(|space| self.is_space_active(*space))
            .collect();
        for space in spaces {
            self.expose_space_if_known(space);
        }
    }

    pub(super) fn recompute_and_set_active_spaces(&mut self, spaces: &[Option<SpaceId>]) {
        let cfg = self.activation_cfg();
        let display_uuids = self.display_uuids_for_current_screens();
        let active_spaces =
            self.space_activation_policy.compute_active_spaces(cfg, spaces, &display_uuids);
        let previous_active = self.active_spaces.clone();
        self.set_active_spaces(&active_spaces);
        self.handle_active_space_change(previous_active);
    }

    pub(super) fn recompute_and_set_active_spaces_from_current_screens(&mut self) {
        let raw_spaces = self.authoritative_spaces_for_current_screens();
        self.recompute_and_set_active_spaces(&raw_spaces);
    }

    fn authoritative_spaces_for_current_screens(&self) -> Vec<Option<SpaceId>> {
        self.space_state
            .screens
            .iter()
            .map(|screen| {
                screen.space.filter(|space| self.space_state.active_spaces.contains(space))
            })
            .collect()
    }

    fn handle_active_space_change(&mut self, previous_active: HashSet<SpaceId>) {
        if previous_active == self.active_spaces {
            return;
        }

        let deactivated: Vec<SpaceId> =
            previous_active.difference(&self.active_spaces).copied().collect();
        let activated: Vec<SpaceId> =
            self.active_spaces.difference(&previous_active).copied().collect();

        // Do not remove windows when a space is merely deactivated (e.g. macOS Space
        // switches). Removing them clears workspace assignments and causes windows
        // without app rules to be re-assigned to the current workspace.

        if !activated.is_empty() {
            for space in &activated {
                self.expose_space_if_known(*space);
            }
        }

        if !activated.is_empty() || !deactivated.is_empty() {
            self.refresh_window_server_snapshot_for_active_spaces();
            self.check_for_new_windows();
        }

        if !activated.is_empty() {
            self.apply_app_rules_for_activated_spaces(&activated);
        }
    }

    pub(super) fn refresh_window_server_snapshot_for_active_spaces(&mut self) {
        let active_windows = self.authoritative_active_space_windows();
        self.reconcile_authoritative_active_window_snapshot(active_windows, false);
    }

    pub(super) fn authoritative_active_space_windows(
        &self,
    ) -> Vec<(WindowServerId, Option<SpaceId>)> {
        let mut queried = HashMap::default();
        for space in self.iter_active_spaces() {
            for wsid in self
                .reads
                .window_server
                .space_window_list_for_connection(&[space.get()])
                .into_iter()
                .map(WindowServerId::new)
            {
                queried.entry(wsid).or_insert(space);
            }
        }

        // A refresh can be partial while WindowServer is waking. Keep the last
        // forwarded per-space sample in that case, but never use the global
        // visible-window union as a substitute for querying each active space.
        let membership = if queried.is_empty() {
            self.space_state.active_window_spaces.clone()
        } else {
            queried
        };

        let mut membership: Vec<_> = membership
            .into_iter()
            .map(|(wsid, space)| (wsid, self.affinity().resolve_native_space(wsid, Some(space))))
            .collect();
        membership.sort_by_key(|(wsid, _)| *wsid);
        membership
    }

    fn has_known_windows_for_active_spaces(&self) -> bool {
        self.state.windows.iter_windows().any(|(wid, _)| {
            self.affinity()
                .authoritative_space_for_window_id(wid)
                .is_some_and(|space| self.is_space_active(space))
        })
    }

    pub(super) fn refresh_active_space_window_membership(
        &mut self,
        active_windows: Vec<(WindowServerId, Option<SpaceId>)>,
    ) {
        let active_wsids: HashSet<WindowServerId> =
            active_windows.iter().map(|(wsid, _)| *wsid).collect();

        // An empty active-space list is valid, but an empty WS-id result while we
        // already know about windows assigned to the active space is typically the
        // transient post-wake race on same-display space switches. Preserve the
        // existing visibility basis in that case and let the follow-up AX refresh
        // reconcile instead of blanking the workspace immediately.
        if active_wsids.is_empty() && self.has_known_windows_for_active_spaces() {
            return;
        }

        let previously_visible_wsids: Vec<_> =
            self.state.windows.iter_visible_window_server_ids().collect();
        for wsid in previously_visible_wsids {
            if !active_wsids.contains(&wsid) {
                self.state.windows.mark_window_hidden(wsid);
            }
        }

        for (wsid, space) in active_windows {
            let space = self.affinity().resolve_native_space(wsid, space);
            if let Some(space) = space {
                self.state.windows.set_window_server_space(wsid, Some(space));
                self.clear_pending_target_if_confirmed_space(wsid, space);
            }
            self.state.windows.mark_window_visible(wsid);
            self.state.windows.clear_window_server_observed(wsid);
        }
    }

    fn remove_windows_missing_from_active_space_snapshot(
        &mut self,
        previously_visible_wsids: Vec<WindowServerId>,
        preserve_assignments: bool,
    ) {
        for wsid in previously_visible_wsids {
            if self.state.windows.is_window_visible(wsid) {
                continue;
            }
            let Some(wid) = self.state.windows.tracked_window_id(wsid) else {
                continue;
            };
            let Some(space) = self.affinity().assigned_space_for_window_id(wid) else {
                continue;
            };
            if !self.is_space_active(space) {
                continue;
            }

            let inactive_target = self
                .affinity()
                .resolve_native_space(wsid, None)
                .filter(|current_space| *current_space != space)
                .filter(|current_space| !self.is_space_active(*current_space));
            if let Some(current_space) = inactive_target {
                self.state.windows.set_window_server_space(wsid, Some(current_space));
                let _ = self.reassign_window_to_authoritative_space(wid, current_space);
                continue;
            }

            if preserve_assignments {
                debug!(
                    ?wid,
                    ?wsid,
                    "Preserving workspace assignment omitted from partial authoritative snapshot"
                );
                continue;
            }

            // If the authoritative active-space snapshot leaves out a window
            // that was visible and WindowServer cannot confirm a new
            // native space for it, drop the stale origin-space ownership. Keeping
            // the old assignment lets later discovery/MC refresh rebuild the
            // origin layout from stale workspace state.
            self.state.windows.set_window_server_space(wsid, None);
            self.send_layout_event(LayoutEvent::WindowRemoved(wid));
        }
    }

    pub(super) fn reconcile_authoritative_active_window_snapshot(
        &mut self,
        active_windows: Vec<(WindowServerId, Option<SpaceId>)>,
        preserve_missing_assignments: bool,
    ) {
        let previously_visible_wsids: Vec<_> =
            self.state.windows.iter_visible_window_server_ids().collect();
        self.refresh_active_space_window_membership(active_windows);
        self.remove_windows_missing_from_active_space_snapshot(
            previously_visible_wsids,
            preserve_missing_assignments,
        );
        self.reconcile_windows_with_authoritative_spaces();
    }

    pub(super) fn clear_pending_hidden_window_targets(&self) {
        for (wid, window) in self.state.windows.iter_windows() {
            if self.affinity().hidden_assigned_space_for_window_id(wid).is_none() {
                continue;
            }
            if let Some(wsid) = window.info.sys_id {
                self.transactions.clear_target(&wsid);
            }
        }
    }

    pub(super) fn clear_pending_target_if_confirmed_space(
        &self,
        wsid: WindowServerId,
        confirmed_space: SpaceId,
    ) {
        if self.affinity().pending_target_space_for_window_server_id(wsid) == Some(confirmed_space)
        {
            self.transactions.clear_target(&wsid);
        }
    }

    pub(super) fn is_fullscreen_space(&self, space: SpaceId) -> bool {
        self.space_state.fullscreen_spaces.contains(&space)
    }
}
