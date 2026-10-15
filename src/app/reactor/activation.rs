//! An application activating: whether its window's workspace is switched to, and which of its
//! windows takes focus when macOS picks a parked one.

use rini_core::ids::{SpaceId, WindowId, pid_t};
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, trace};

use super::events::EventOutcome;
use super::{Reactor, WorkspaceSwitchOrigin};
use crate::windows::domain::request::Quiet;
use crate::windows::domain::{raise as raise_manager, raise_order};
use crate::workspaces::LayoutEvent;

impl Reactor {
    pub(super) fn handle_app_activation_workspace_switch(&mut self, pid: pid_t) -> EventOutcome {
        if self.workspace_switch_manager.active_workspace_switch.is_some() {
            trace!(
                "Skipping auto workspace switch for pid {} because a workspace switch is in progress",
                pid
            );
            return EventOutcome::no_change();
        }

        if self.workspace_switch_manager.manual_switch_in_progress() {
            debug!(
                "Skipping auto workspace switch for pid {} because a manual switch is in progress",
                pid
            );
            return EventOutcome::no_change();
        }

        if let Some(active_space) = self.raw_command_space()
            && self.is_fullscreen_space(active_space)
        {
            debug!(
                "Skipping auto workspace switch for pid {} because the active space is fullscreen",
                pid
            );
            return EventOutcome::no_change();
        }

        if let Some(wsid) = self.activation_from_unmanageable_window(pid) {
            debug!(
                ?wsid,
                "Skipping auto workspace switch for pid {} because the activated window is not manageable",
                pid
            );
            return EventOutcome::no_change();
        }

        let Some(bundle_id_str) = self.apps.get(&pid).and_then(|app| app.info.bundle_id.clone())
        else {
            return EventOutcome::no_change();
        };

        if self.config.settings.auto_focus_denylist.contains(&bundle_id_str) {
            debug!(
                "App {} is on the auto-focus denylist, ignoring activation",
                bundle_id_str
            );
            return EventOutcome::no_change();
        }

        debug!(
            "App activation detected: {} (pid: {}), checking for workspace switch",
            bundle_id_str, pid
        );

        // Carbon activation is reconciled by the app thread before this runs,
        // so a missing main window means there is no authoritative switch
        // target. Picking an arbitrary window for the process is especially
        // unsafe for apps whose windows span multiple virtual workspaces.
        let app_window =
            self.main_window().filter(|wid| wid.pid == pid && self.window_is_standard(*wid));

        let Some(app_window_id) = app_window else {
            return EventOutcome::no_change();
        };

        let Some(window_space) = self.affinity().best_space_for_window_id(app_window_id) else {
            return EventOutcome::no_change();
        };

        self.maybe_auto_switch_to_window_workspace(pid, app_window_id, window_space)
    }

    /// Sends focus back to the window the app was in, when an activation picked a parked one.
    ///
    /// macOS chooses the window on cmd-tab, and it chose a window rini had parked off screen for a
    /// workspace it was not showing. Following that switched the parked window's workspace, animated a
    /// display the user had not asked to move, and landed them in the wrong terminal. Only ever avoids a
    /// switch: it acts when the pick is parked and the remembered window is visible.
    pub(super) fn activation_redirect(
        &mut self,
        picked: WindowId,
        picked_space: SpaceId,
    ) -> Option<EventOutcome> {
        // Peeked, not taken: when this does not redirect, the workspace switch below may still
        // use the remembered window (`maybe_auto_switch_to_window_workspace`).
        let remembered = self.main_window_tracker.peek_activation_target(picked.pid)?;
        let target = crate::windows::domain::focus::activation_focus_target(
            picked,
            self.window_is_in_active_workspace(picked, Some(picked_space)),
            Some(remembered),
            self.window_is_standard(remembered)
                && self.window_is_in_active_workspace(remembered, None),
        )?;
        self.main_window_tracker.take_activation_target(picked.pid);
        debug!(
            ?picked,
            ?target,
            "app activation picked a parked window; focusing the one it was in instead"
        );
        let mut app_handles = HashMap::default();
        if let Some(app) = self.apps.get(&target.pid) {
            app_handles.insert(target.pid, app.handle.clone());
        }
        let space = self.affinity().best_space_for_window_id(target);
        let mut outcome = EventOutcome::default().with_raise_request(
            raise_manager::Event::RaiseRequest(raise_manager::RaiseRequest {
                raise_windows: vec![raise_order::AppBatch::new(target)],
                focus_window: Some((target, None)),
                app_handles,
                focus_quiet: Quiet::No,
            }),
        );
        if let Some(space) = space {
            outcome = outcome.with_layout_event(LayoutEvent::WindowFocused(space, target));
        }
        Some(outcome)
    }

    /// Whether `window` is in the active workspace of the space it belongs to.
    fn window_is_in_active_workspace(&self, window: WindowId, space: Option<SpaceId>) -> bool {
        let Some(space) = space.or_else(|| self.affinity().best_space_for_window_id(window)) else {
            return false;
        };
        self.is_space_active(space)
            && self
                .layout_engine
                .is_window_in_active_workspace(&self.state.windows, space, window)
    }

    pub(super) fn maybe_auto_switch_to_window_workspace(
        &mut self,
        pid: pid_t,
        app_window_id: WindowId,
        window_space: SpaceId,
    ) -> EventOutcome {
        let workspace_state = self.layout_engine.virtual_workspace_manager();
        let Some(window_workspace) =
            workspace_state.workspace_for_window(&self.state.windows, window_space, app_window_id)
        else {
            return EventOutcome::no_change();
        };
        // macOS picks the app's main window on cmd-tab; the user was in another window of the app
        // on the same workspace (Kiro, two windows on one row: the pick scrolled the strip to the
        // far one). The switch is right, the window is the remembered one.
        let remembered = self.main_window_tracker.take_activation_target(pid).filter(|wid| {
            self.window_is_standard(*wid)
                && workspace_state.workspace_for_window(&self.state.windows, window_space, *wid)
                    == Some(window_workspace)
        });
        let picked = app_window_id;
        let app_window_id = switch_focus_within_workspace(app_window_id, remembered);
        if app_window_id != picked {
            debug!(
                ?picked,
                target = ?app_window_id,
                "app activation picked another of its windows on the same workspace; focusing the one the user was in"
            );
        }

        let Some(current_workspace) = self.layout_engine.active_workspace(window_space) else {
            return EventOutcome::no_change();
        };

        if window_workspace != current_workspace {
            let workspaces =
                self.layout_engine.virtual_workspace_manager_mut().list_workspaces(window_space);
            if let Some((workspace_index, _)) =
                workspaces.iter().enumerate().find(|(_, (ws_id, _))| *ws_id == window_workspace)
            {
                debug!(
                    "Auto-switching to workspace {} for activated app (pid: {})",
                    workspace_index, pid
                );

                self.store_current_floating_positions(window_space);
                self.workspace_switch_manager
                    .start_workspace_switch(WorkspaceSwitchOrigin::Auto);

                let response = self.layout_engine.switch_to_workspace_with_focus(
                    &self.state.windows,
                    window_space,
                    workspace_index,
                    app_window_id,
                );
                return EventOutcome::layout_changed(false)
                    .with_layout_response(response, Some(window_space));
            }
        }

        EventOutcome::no_change()
    }
}

/// The window an activation-driven switch focuses: the window the user was last in when it sits on
/// the same workspace as macOS's pick, else the pick.
pub(crate) fn switch_focus_within_workspace<W: Copy + PartialEq>(
    picked: W,
    remembered_on_same_workspace: Option<W>,
) -> W {
    remembered_on_same_workspace.unwrap_or(picked)
}
