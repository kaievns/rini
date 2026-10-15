//! A snapshot of the displays' spaces taken in: the screens and spaces replaced, a departing
//! display's windows given it as their home, the topology's window moves applied, windows coming
//! back from a native fullscreen space, and the snapshot held while Mission Control is open.

use objc2_core_foundation::CGSize;
use rini_core::ids::{SpaceId, WindowId, WindowServerId};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use tracing::{info, warn};

use super::events::EventOutcome;
use super::{Reactor, RefocusState, StaleCleanupState};
use crate::displays::domain::topology::{
    ForwardedSpaceState, TopologyWindowDelta, display_set_delta,
};
use crate::windows::domain::request::Request;
use crate::workspaces::LayoutEvent;

impl Reactor {
    pub(super) fn handle_authoritative_space_snapshot(
        &mut self,
        space_state: ForwardedSpaceState,
    ) -> anyhow::Result<EventOutcome> {
        let mut outcome = EventOutcome::window_membership_changed(false, true);
        let analysis = crate::displays::domain::topology::analyze_space_snapshot(
            &self.space_state,
            &self.active_spaces,
            &self.space_activation_policy,
            self.activation_cfg(),
            &space_state,
        );
        let pending_space_state = space_state.clone();
        let ForwardedSpaceState {
            screens,
            fullscreen_spaces,
            has_seen_display_set,
            active_spaces,
            menu_bar_space,
            command_space,
            display_space_ids,
            display_space_ids_from_screens,
            last_user_space_by_display,
            space_remaps,
            display_set_changed,
            should_force_refresh_layout,
            releases_lifecycle_refresh_quarantine,
            resized_spaces,
            topology_window_delta,
            active_window_spaces,
            ..
        } = space_state;
        self.space_state.active_window_spaces = active_window_spaces;
        let activation_config = self.activation_cfg();
        let crate::displays::domain::topology::SpaceSnapshotAnalysis {
            spaces,
            authoritative_spaces,
            command_space_only_update,
            invalidates_pending_targets,
        } = analysis;

        let current_display_spaces = screens
            .iter()
            .filter_map(|screen| screen.space.map(|space| (space, screen.display_uuid.clone())))
            .collect::<Vec<_>>();
        self.layout_manager.layout_engine.reconcile_startup_spaces(
            &mut self.state.windows,
            &mut self.state.display_memory,
            &current_display_spaces,
            screens.len(),
        );

        self.space_state.has_seen_display_set = has_seen_display_set;
        self.space_state.fullscreen_spaces = fullscreen_spaces;
        self.space_state.active_spaces = active_spaces;
        if command_space_only_update {
            self.space_state.menu_bar_space = menu_bar_space;
            self.space_state.command_space = command_space;
            return Ok(outcome);
        }
        // Note on pruning: display state is deliberately NOT pruned when a display goes
        // away. The UUID -> space entry is precisely the memory needed to put windows back
        // when that display returns, and macOS assigns a NEW space id on every reconnect
        // (observed 479 -> 484 -> 487 -> 516 -> 552 for one monitor), so the display UUID
        // is the only durable link between a physical display and its layout. The registry
        // holds one entry per display ever seen, which costs nothing and is what makes
        // dock/undock recoverable.
        self.space_state.menu_bar_space = menu_bar_space;
        self.space_state.command_space = command_space;
        self.space_state.display_space_ids = display_space_ids;
        self.space_state.display_space_ids_from_screens = display_space_ids_from_screens;
        self.space_state.last_user_space_by_display = last_user_space_by_display;

        if screens.is_empty() {
            self.refocus_manager.stale_cleanup_state = StaleCleanupState::Suppressed;
            if !self.space_state.screens.is_empty() {
                self.space_state.screens.clear();
                self.expose_all_spaces();
            }
            self.recompute_and_set_active_spaces(&[]);
            self.update_complete_window_server_info(Vec::new());
            self.try_apply_pending_space_change();
            return Ok(outcome);
        }

        self.refocus_manager.stale_cleanup_state = StaleCleanupState::Enabled;
        let display_set = display_set_delta(
            &self
                .space_state
                .screens
                .iter()
                .map(|screen| screen.display_uuid.clone())
                .collect::<Vec<_>>(),
            &screens.iter().map(|screen| screen.display_uuid.clone()).collect::<Vec<_>>(),
            display_set_changed,
        );
        // Capture affinity for a departing display BEFORE anything reacts to its absence.
        //
        // This is the only moment the truth is still available. `self.state.windows` still
        // holds the pre-change assignments here; the evacuation happens later in this same
        // handler, in apply_topology_window_delta, and once macOS has moved those windows to
        // the remaining display there is no way left to tell which of them had been on the
        // one that vanished. Recording it now is what lets a later replug move back exactly
        // the windows that were there, rather than whichever windows now occupy those slots.
        for display_uuid in &display_set.departed {
            let Some(departing_space) =
                self.state.display_memory.affinity.space_for_display(display_uuid)
            else {
                continue;
            };
            let residents: Vec<WindowId> = self
                .state
                .windows
                .iter_workspace_assignments()
                .filter(|(_, assignment)| assignment.space == departing_space)
                .map(|(window, _)| window)
                .collect();
            if residents.is_empty() {
                continue;
            }
            info!(
                display_uuid,
                ?departing_space,
                window_count = residents.len(),
                "Recording display affinity for windows on a departing display"
            );
            for window in residents {
                self.layout_manager.layout_engine.set_window_display_home(
                    &mut self.state.display_memory,
                    window,
                    departing_space,
                );
            }
        }
        self.space_state.screens = screens;
        // The topology the launch memory is keyed by. Set from here because this is the one place the
        // attached set is authoritatively replaced.
        let connected: Vec<String> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.display_uuid_owned())
            .collect();
        self.layout_manager.layout_engine.set_connected_displays(connected);
        // Cursor warping is derived from display geometry, so it has to be told whenever
        // that geometry changes — docking, undocking, or rearranging in System Settings.
        // Pushing it from here rather than having the actor poll CGDisplayBounds keeps one
        // source of truth for the screen set.
        self.publish_cursor_warp_screens();
        if invalidates_pending_targets {
            self.clear_pending_hidden_window_targets();
        }
        if self.is_mission_control_active() {
            self.pending_space_change_manager.pending_space_change = Some(pending_space_state);
            return Ok(outcome);
        }
        for (previous_space, space) in space_remaps {
            self.layout_manager.layout_engine.remap_space(
                &mut self.state.windows,
                &mut self.state.display_memory,
                previous_space,
                space,
            );
        }
        // Deliberately NOT remapping a reconnected display's whole SPACE onto its new id.
        //
        // An earlier fix did exactly that, keyed by display UUID, because macOS mints a new
        // space id on every reconnect (observed 479 -> 484 -> 487 -> 516 -> 552 for one
        // monitor) and the layout saved under the old id is otherwise orphaned. It made the
        // replug worse, not better, for two reasons:
        //
        //   - The old space is a snapshot from before the unplug. Its windows were long
        //     since evacuated to the remaining display and the user carried on working
        //     there, so replaying it moved back whichever windows now occupied those slots
        //     rather than the ones that had actually been on the external.
        //   - remap_space deletes the auto-created workspaces already sitting on the target
        //     space id, and that drops the WindowStore assignments of any window macOS had
        //     placed there. Those windows were then re-assigned from scratch, which is what
        //     reshuffled the other display's strip.
        //
        // Per-window affinity handles this instead: see repatriate_windows_to_display, run
        // once the topology delta has settled. Startup space-id churn is a genuinely
        // different case and is still remapped, by reconcile_startup_spaces.
        for screen in &self.space_state.screens {
            let (Some(space), Some(display_uuid)) = (screen.space, screen.display_uuid_opt())
            else {
                continue;
            };
            self.layout_manager.layout_engine.update_space_display(
                &mut self.state.display_memory,
                space,
                Some(display_uuid.to_string()),
            );
        }
        // Which hardware arrangement is in force. Every window record is scoped to it, so this has
        // to be settled before anything below reads or writes one — the repatriation at the end of
        // this function above all.
        let setup_change = self.adopt_current_display_setup();
        // Re-observe where windows are, and in what order, whenever the topology is settled.
        //
        // Only while the display set is UNCHANGED. During a display change the current
        // assignments are mid-evacuation and would record the wrong display; on a settled
        // topology they are exactly right.
        if display_set.unchanged {
            self.sync_display_affinity_from_live_layout();
        }
        let current_screens = self.screens_for_current_spaces();
        self.space_activation_policy
            .on_spaces_updated(activation_config, &current_screens);
        self.recompute_and_set_active_spaces(&authoritative_spaces);
        self.restore_windows_after_fullscreen_exit(&spaces);

        for (space, size) in resized_spaces {
            if !self.is_space_active(space) {
                continue;
            }
            self.layout_manager
                .layout_engine
                .virtual_workspace_manager_mut()
                .list_workspaces(space);
            outcome = outcome.with_layout_event(LayoutEvent::SpaceExposed(space, size));
        }
        if let Some(delta) = topology_window_delta {
            outcome.absorb(self.apply_topology_window_delta(delta));
        }
        let active_windows = self.authoritative_active_space_windows();
        self.finalize_space_change(&spaces, active_windows, releases_lifecycle_refresh_quarantine);
        self.try_apply_pending_space_change();
        // Repatriate LAST, after finalize_space_change.
        //
        // Everything above this line derives window ownership from where macOS currently
        // reports each window, and finalize_space_change's
        // reconcile_windows_with_authoritative_spaces re-derives it for every tracked
        // window. Repatriating before that point is silently undone: the moved windows are
        // still physically on the old display when the reconciliation reads their position,
        // so it puts them straight back. Running afterwards makes affinity the last word on
        // a display arrival, which is the whole point of recording it.
        //
        // A display nobody has used before takes nothing, and that needs no check here: the records
        // are scoped to the arrangement in force, an arrangement nobody has seen holds none, so
        // there is nothing homed to the new display to move. Saying so in the log is worth it —
        // "why did my windows not move onto the projector" has an answer — but a guard around the
        // loop would be a branch that cannot change the outcome.
        if !setup_change.restores_windows() && !display_set.arrived.is_empty() {
            info!(
                setup = %self.state.display_memory.affinity.current_setup(),
                arrived = ?display_set.arrived,
                "a display arrangement nobody has seen: every window stays where it is"
            );
        }
        for display_uuid in &display_set.arrived {
            outcome.absorb(self.repatriate_windows_to_display(display_uuid));
        }
        if should_force_refresh_layout {
            outcome = outcome.with_force_window_refresh().with_arrange_passes(1);
        }
        Ok(outcome)
    }

    fn try_apply_pending_space_change(&mut self) {
        if let Some(pending) = self.pending_space_change_manager.pending_space_change.take() {
            if pending.screens.len() == self.space_state.screens.len() {
                // During native Mission Control we must preserve the full forwarded snapshot,
                // not just the raw spaces vector, otherwise command-space and per-display space
                // metadata can remain stale after exit.
                if let Ok(outcome) = self.handle_authoritative_space_snapshot(pending) {
                    self.apply_event_outcome(outcome);
                }
            } else {
                self.pending_space_change_manager.pending_space_change = Some(pending);
            }
        }
    }

    pub(super) fn repair_spaces_after_mission_control(&mut self) {
        // First, apply any SpaceChanged that arrived while MC was active.
        self.try_apply_pending_space_change();
    }

    fn finalize_space_change(
        &mut self,
        spaces: &[Option<SpaceId>],
        active_windows: Vec<(WindowServerId, Option<SpaceId>)>,
        preserve_missing_assignments: bool,
    ) {
        self.refocus_manager.stale_cleanup_state = if spaces.iter().all(|space| space.is_none()) {
            StaleCleanupState::Suppressed
        } else {
            StaleCleanupState::Enabled
        };
        self.expose_all_spaces();
        if let Some(main_window) = self.main_window()
            && let Some(space) = self.main_window_space()
        {
            self.send_layout_event(LayoutEvent::WindowFocused(space, main_window));
        }
        self.reconcile_authoritative_active_window_snapshot(
            active_windows,
            preserve_missing_assignments,
        );
        self.check_for_new_windows();

        if let Some(space) = self.workspace_command_space() {
            self.focus_desktop_if_active_workspace_empty(space);
        }

        if let Some(space) = self
            .workspace_command_space()
            .or_else(|| spaces.iter().copied().flatten().find(|space| self.is_space_active(*space)))
        {
            self.layout_manager.layout_engine.ensure_active_workspace_info(space);
        }
    }

    fn restore_windows_after_fullscreen_exit(&mut self, spaces: &[Option<SpaceId>]) {
        let refresh_spaces: Vec<SpaceId> = spaces
            .iter()
            .copied()
            .flatten()
            .filter(|space| !self.is_fullscreen_space(*space))
            .collect();

        for space in refresh_spaces {
            let records: Vec<_> = self
                .state
                .windows
                .iter_native_fullscreen_records()
                .filter(|record| {
                    record.last_known_user_space == Some(space)
                        || record.assigned_space == Some(space)
                })
                .collect();

            if records.is_empty() {
                continue;
            }

            for record in records {
                let _ = self
                    .state
                    .windows
                    .restore_window_from_native_fullscreen(record.current_window_id);

                if let Some(app) = self.app_manager.apps.get(&record.current_window_id.pid)
                    && let Err(e) = app.handle.send(Request::GetVisibleWindows)
                {
                    warn!(
                        "Failed to send GetVisibleWindows to app {}: {}",
                        record.current_window_id.pid, e
                    );
                }

                let live_window_id = record
                    .window_server_id
                    .and_then(|wsid| self.state.windows.tracked_window_id(wsid))
                    .or_else(|| {
                        self.state
                            .windows
                            .contains_window(record.current_window_id)
                            .then_some(record.current_window_id)
                    });

                let target_space = record.assigned_space.or(record.last_known_user_space);

                if let (Some(window_id), Some(target_space)) = (live_window_id, target_space)
                    && let Some(source_space) =
                        self.affinity().best_space_for_window_id(window_id).or(Some(target_space))
                    && source_space != target_space
                {
                    let target_screen_size = self
                        .space_state
                        .screen_by_space(target_space)
                        .map(|screen| screen.frame.size)
                        .unwrap_or_else(|| CGSize::new(0.0, 0.0));

                    let response = self.layout_manager.layout_engine.move_window_to_space(
                        &mut self.state.windows,
                        &mut self.state.display_memory,
                        source_space,
                        target_space,
                        target_screen_size,
                        window_id,
                    );
                    self.handle_layout_response(response, None);
                }
            }

            self.refocus_manager.refocus_state = RefocusState::Pending(space);
            self.update_layout(false, false, None);
            self.update_focus_follows_mouse_state();
        }
    }

    fn apply_topology_window_delta(&mut self, delta: TopologyWindowDelta) -> EventOutcome {
        let appeared: HashMap<WindowServerId, SpaceId> = delta.appeared.into_iter().collect();
        let disappeared: HashMap<WindowServerId, SpaceId> = delta.disappeared.into_iter().collect();
        let window_server_ids: HashSet<WindowServerId> =
            appeared.keys().chain(disappeared.keys()).copied().collect();
        let mut outcome = EventOutcome::default();

        for window_server_id in window_server_ids {
            let appeared_space = appeared.get(&window_server_id).copied();
            let disappeared_space = disappeared.get(&window_server_id).copied();
            let authoritative_space =
                self.affinity().resolve_native_space(window_server_id, appeared_space);
            if let Some(target_space) = authoritative_space {
                self.state.windows.set_window_server_space(window_server_id, Some(target_space));
                if appeared_space == Some(target_space) {
                    self.clear_pending_target_if_confirmed_space(window_server_id, target_space);
                }
                if self.is_space_active(target_space) {
                    self.state.windows.mark_window_visible(window_server_id);
                } else {
                    self.state.windows.mark_window_hidden(window_server_id);
                }
                if let Some(window) = self.state.windows.tracked_window_id(window_server_id) {
                    let restored = self.restore_fullscreen_window_to_user_space(
                        window_server_id,
                        target_space,
                        window,
                        &mut outcome,
                    );
                    if restored.is_none() {
                        self.reassign_window_to_authoritative_space_preserving_workspace_ordinal(
                            window,
                            target_space,
                        );
                    }
                }
            } else if let Some(previous_space) = disappeared_space {
                self.state
                    .windows
                    .set_window_server_space(window_server_id, Some(previous_space));
                self.state.windows.mark_window_hidden(window_server_id);
                if let Some(window) = self.state.windows.tracked_window_id(window_server_id)
                    && self.affinity().assigned_space_for_window_id(window) == Some(previous_space)
                    && self.is_space_active(previous_space)
                {
                    outcome = outcome
                        .with_layout_event(LayoutEvent::WindowRemovedPreserveFloating(window));
                }
            }
        }
        outcome
    }

    pub(super) fn restore_fullscreen_window_to_user_space(
        &mut self,
        window_server_id: WindowServerId,
        space: SpaceId,
        original_window: WindowId,
        outcome: &mut EventOutcome,
    ) -> Option<bool> {
        let restored = self
            .state
            .windows
            .restore_window_from_native_fullscreen_by_window_server_id(window_server_id)
            .or_else(|| {
                self.state.windows.restore_window_from_native_fullscreen(original_window)
            })?;
        let owner = self
            .state
            .windows
            .contains_window(restored.current_window_id)
            .then_some(restored.current_window_id)
            .or_else(|| {
                restored
                    .window_server_id
                    .and_then(|id| self.state.windows.tracked_window_id(id))
            })
            .or_else(|| self.state.windows.tracked_window_id(window_server_id))
            .or_else(|| {
                self.state.windows.contains_window(original_window).then_some(original_window)
            })?;
        if owner != original_window
            && self.affinity().assigned_space_for_window_id(original_window).is_some()
        {
            *outcome = std::mem::take(outcome)
                .with_layout_event(LayoutEvent::WindowRemoved(original_window));
        }
        *outcome = std::mem::take(outcome).with_app_request(owner.pid, Request::GetVisibleWindows);
        Some(
            if self.affinity().assigned_space_for_window_id(owner) == Some(space) {
                self.is_space_active(space)
                    && self.restore_window_to_active_layout_if_visible(owner, space)
            } else {
                self.reassign_window_to_authoritative_space(owner, space)
            },
        )
    }
}
