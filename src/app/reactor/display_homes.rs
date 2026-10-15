//! Which display each window belongs on: the display arrangement in force, display pins, homes
//! observed from the live layout, windows moved back to a display that returns or on the
//! redistribute command, and a window reassigned to the space macOS reports it on.

use rini_core::ids::{SpaceId, WindowId};
use tracing::{debug, info};

use super::events::EventOutcome;
use super::{Reactor, ScreenInfo, WindowFilter};
use crate::windows::domain::rules::AppRuleDecision;
use crate::workspaces::LayoutEvent;
use crate::workspaces::domain::display_affinity::SetupChange;
use crate::workspaces::domain::display_pin::display_for_role;

impl Reactor {
    /// Put the arrangement the attached displays form in force, and say what that turned out to be.
    ///
    /// Called on every settled topology. An arrangement nobody has seen answers `New`, which is the
    /// caller's instruction to move nothing: see `display_setup` for why a display's identity, not
    /// just the number of displays, is what names an arrangement.
    pub(super) fn adopt_current_display_setup(&mut self) -> SetupChange {
        let attached: Vec<String> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.display_uuid_owned())
            .collect();
        let change = self.state.display_memory.affinity.use_setup(attached);
        if !matches!(change, SetupChange::Unchanged | SetupChange::Refused) {
            info!(
                setup = %self.state.display_memory.affinity.current_setup(),
                ?change,
                "display arrangement in force"
            );
        }
        change
    }

    /// Give every pinned window the home its rule asks for, in the arrangement now in force.
    ///
    /// `set_window_home_if_absent`, deliberately: a pin is the DEFAULT home, so an explicit move in
    /// this arrangement wins and the window stays where the user put it. That is what "unless I
    /// explicitly move it there" means. A role nothing fills — pinned to the internal display with the
    /// lid shut — resolves to nothing and writes nothing, so the window goes wherever there is.
    fn apply_display_pins(&mut self) {
        let screens: Vec<ScreenInfo> =
            self.space_state.screens_in_physical_order().into_iter().cloned().collect();
        let pins: Vec<(WindowId, String)> = self
            .state
            .windows
            .iter_windows()
            .filter_map(|(window, state)| {
                let app = self.apps.get(&window.pid);
                let decision = self.layout_engine.evaluate_app_rules(
                    app.and_then(|app| app.info.bundle_id.as_deref()),
                    app.and_then(|app| app.info.localized_name.as_deref()),
                    Some(state.info.title.as_str()),
                    state.info.ax_role.as_deref(),
                    state.info.ax_subrole.as_deref(),
                    state.info.is_modal,
                );
                let AppRuleDecision::Managed { display: Some(role), .. } = decision else {
                    return None;
                };
                let target = display_for_role(role, screens.iter())?;
                Some((window, target.to_owned()))
            })
            .collect();
        // Not named `display`: that shadows tracing's own `display` helper inside the macro.
        for (window, target) in pins {
            if self.state.display_memory.affinity.set_pinned_home(window, &target) {
                debug!(
                    idx = window.idx.get(),
                    pinned_to = %target,
                    "pinning a window to the display its rule names"
                );
            }
        }
    }

    /// Re-observe display affinity and strip order for every attached display.
    ///
    /// A home written once and never revised goes stale the moment anything is rearranged: a
    /// window dragged from the external to the built-in would be hauled back on the next replug.
    ///
    /// Cheap enough to run on every settled layout because it only walks the active
    /// workspace of each screen, which is what the layout pass just computed anyway.
    pub(super) fn sync_display_affinity_from_live_layout(&mut self) {
        if self.display_churning() {
            return;
        }
        // Closed windows keep their affinity otherwise, because the display-change path
        // removes windows with WindowRemovedPreserveFloating, which does not clear it.
        // Measured: the external's affinity list held three long-closed windows while every
        // live window was homed to the built-in, so a replug had nothing to move back.
        self.layout_engine
            .forget_affinity_for_dead_windows(&self.state.windows, &mut self.state.display_memory);
        // Pins first. Both this and the observation below write a home only if one is absent, so
        // whichever runs first decides — and for a pinned window the answer must be its rule, not
        // wherever macOS happens to have parked it. A new arrangement starts with every home absent,
        // so without this the pin would lapse on every dock.
        self.apply_display_pins();
        let observations: Vec<(String, Vec<WindowId>)> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| {
                let (space, uuid) = (screen.space?, screen.display_uuid_owned()?);
                self.is_space_active(space).then_some((space, uuid))
            })
            .map(|(space, uuid)| {
                let windows = self.layout_engine.ordered_windows_in_active_workspace(space);
                (uuid, windows)
            })
            .collect();
        for (uuid, windows) in observations {
            if windows.is_empty() {
                continue;
            }
            self.layout_engine.sync_display_affinity(
                &mut self.state.display_memory,
                &uuid,
                &windows,
            );
        }
    }

    /// Spread every window back onto the display it belongs to, keeping its workspace.
    ///
    /// Recovery command for windows piled into a single workspace on a single display. It undoes
    /// that without deleting ~/.rini/layout.ron, which would lose every window's size and position.
    ///
    /// Only windows whose recorded home display differs from where they currently sit are
    /// moved, so running this on a healthy layout does nothing. Workspace membership is never
    /// changed — that is the window's identity, and a recovery command has no business
    /// guessing at it.
    pub(super) fn redistribute_windows(&mut self) -> EventOutcome {
        let mut outcome = EventOutcome::default();
        let attached: Vec<(String, SpaceId)> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| Some((screen.display_uuid_owned()?, screen.space?)))
            .collect();
        if attached.is_empty() {
            return outcome;
        }

        let mut moved = 0usize;
        for (display_uuid, target_space) in &attached {
            let windows = self.layout_engine.windows_to_repatriate(
                &self.state.windows,
                &self.state.display_memory,
                display_uuid,
                *target_space,
            );
            for window in windows {
                let Some(source_space) = self.affinity().assigned_space_for_window_id(window)
                else {
                    continue;
                };
                // Do not fight macOS over a window on a display that is no longer attached;
                // those are handled by repatriation when the display returns.
                if !attached.iter().any(|(_, space)| *space == source_space) {
                    continue;
                }
                let Some(assignment) = self.state.windows.workspace_info_for_window(window) else {
                    continue;
                };
                if self.layout_engine.virtual_workspace_manager_mut().assign_window_to_workspace(
                    &mut self.state.windows,
                    *target_space,
                    window,
                    assignment.workspace_id,
                ) {
                    moved += 1;
                }
            }
        }

        info!(
            moved,
            displays = attached.len(),
            "Redistributed windows to their home displays"
        );
        if moved > 0 {
            outcome.absorb(EventOutcome::layout_changed(false).with_arrange_passes(1));
        }
        outcome
    }

    /// Send windows that belong on `display_uuid` back to it after it reappears.
    ///
    /// A replug used to be handled by remapping whole SPACES: the display's old space id
    /// was migrated onto its new one. That is wrong whenever the windows moved in the
    /// meantime. Unplugging evacuates the external's windows onto the built-in, where the
    /// user keeps working — closing some, opening others, reordering the strip. The old
    /// space is by then a stale snapshot, so remapping it wholesale sent back whichever
    /// windows happened to occupy those slots (Excel and Slack, in the reported case,
    /// instead of the two terminals) and reshuffled the built-in's strip on the way out.
    ///
    /// Per-window affinity replaces that: each window remembers the display it was last
    /// deliberately placed on, and only windows whose home is this display move. Windows
    /// with no affinity for it are left exactly where they are, which is what keeps the
    /// other display's column order intact.
    pub(super) fn repatriate_windows_to_display(&mut self, display_uuid: &str) -> EventOutcome {
        let mut outcome = EventOutcome::default();
        let Some(target_screen) = self
            .space_state
            .screens
            .iter()
            .find(|screen| screen.display_uuid_opt() == Some(display_uuid))
            .cloned()
        else {
            return outcome;
        };
        let Some(target_space) = target_screen.space else {
            return outcome;
        };

        let windows = self.layout_engine.windows_to_repatriate(
            &self.state.windows,
            &self.state.display_memory,
            display_uuid,
            target_space,
        );
        if windows.is_empty() {
            return outcome;
        }

        info!(
            display_uuid,
            ?target_space,
            window_count = windows.len(),
            "Repatriating windows to their home display"
        );

        // Order matters, and this loop depends on two things to rebuild adjacency.
        //
        // `windows` arrives in the strip order last observed on this display, and
        // move_window_to_space inserts each window after the current selection and then
        // selects it. So moving them in order lays them down contiguously, in that order,
        // instead of scattering them among whatever is already on the display. Repatriating
        // in WindowId order — which is what it used to do — is why two terminals the user
        // had kept side by side came back as terminal, Chrome, terminal, editor.
        for window in windows {
            let Some(source_space) = self.affinity().assigned_space_for_window_id(window) else {
                continue;
            };
            // Place the window inside the target display's frame before reassigning it.
            //
            // Reassignment alone is not enough. WindowServer decides which space a window
            // belongs to from where it physically is, so a window still sitting on the old
            // display's coordinates gets reported back on the old space at the next
            // snapshot and the repatriation is silently undone. It is placed as an explicit
            // move-to-display places it; tiling supplies the final frame on the following
            // arrange pass.
            let target_frame = rini_geometry::centered_in(
                self.state
                    .windows
                    .window(window)
                    .map(|window| window.frame_monotonic)
                    .unwrap_or(target_screen.frame),
                target_screen.frame,
            );

            let window_server_id =
                self.state.windows.window(window).and_then(|window| window.info.sys_id);
            if let Some(state) = self.state.windows.window_mut(window) {
                state.frame_monotonic = target_frame;
            }
            let response = self.layout_engine.move_window_to_space(
                &mut self.state.windows,
                &mut self.state.display_memory,
                source_space,
                target_space,
                target_screen.frame.size,
                window,
            );
            if let Some(window_server_id) = window_server_id {
                self.state.windows.set_window_server_space(window_server_id, Some(target_space));
                self.state.windows.mark_window_visible(window_server_id);
            }
            outcome.absorb(
                EventOutcome::layout_changed(false)
                    .with_layout_response(response, None)
                    .with_pre_layout_window_frame_write(window, target_frame),
            );
        }

        outcome.absorb(EventOutcome::layout_changed(false).with_arrange_passes(1));
        outcome
    }

    pub(super) fn reassign_window_to_authoritative_space(
        &mut self,
        wid: WindowId,
        authoritative_space: SpaceId,
    ) -> bool {
        self.reassign_window_to_authoritative_space_with_workspace_preservation(
            wid,
            authoritative_space,
            false,
        )
    }

    pub(crate) fn reassign_window_to_authoritative_space_preserving_workspace_ordinal(
        &mut self,
        wid: WindowId,
        authoritative_space: SpaceId,
    ) -> bool {
        self.reassign_window_to_authoritative_space_with_workspace_preservation(
            wid,
            authoritative_space,
            true,
        )
    }

    fn reassign_window_to_authoritative_space_with_workspace_preservation(
        &mut self,
        wid: WindowId,
        authoritative_space: SpaceId,
        preserve_workspace_ordinal: bool,
    ) -> bool {
        // Native WindowServer visibility is not enough to participate in Rini's
        // layout. Fullscreen exit can surface transient AppKit/Electron windows
        // that are visible and space-owned but are filtered out of query output.
        // Treat this as the one condition for authoritative-space reconciliation:
        // if a window is not query-manageable, remove any stale layout/workspace
        // membership instead of re-assigning it from the WindowServer snapshot.
        if !self
            .state
            .windows
            .window(wid)
            .is_some_and(|window| window.matches_filter(WindowFilter::EffectivelyManageable))
        {
            let changed_space = self.affinity().assigned_space_for_window_id(wid);
            self.send_layout_event(LayoutEvent::WindowRemoved(wid));
            return changed_space.is_some_and(|space| self.is_space_active(space));
        }

        let assigned_space = self.affinity().assigned_space_for_window_id(wid);
        if assigned_space == Some(authoritative_space) {
            return self.restore_window_to_active_layout_if_visible(wid, authoritative_space);
        }
        let frame = self.state.windows.window(wid).map(|window| window.frame_monotonic);
        info!(
            pid = wid.pid,
            idx = wid.idx.get(),
            from = ?assigned_space,
            to = ?authoritative_space,
            frame = ?frame,
            "window moved to the space the window server has it on"
        );

        self.send_layout_event(LayoutEvent::WindowRemovedPreserveFloating(wid));

        let _ = self
            .layout_engine
            .virtual_workspace_manager_mut()
            .list_workspaces(authoritative_space);

        let assigned = if preserve_workspace_ordinal {
            self.layout_engine
                .virtual_workspace_manager_mut()
                .assign_window_to_workspace_preserving_ordinal(
                    &mut self.state.windows,
                    authoritative_space,
                    wid,
                )
                .is_some()
        } else {
            let Some(target_workspace) = self
                .layout_engine
                .ensure_active_workspace_info(authoritative_space)
                .map(|(workspace_id, _)| workspace_id)
                .or_else(|| self.layout_engine.active_workspace(authoritative_space))
            else {
                return assigned_space.is_some_and(|space| self.is_space_active(space));
            };

            self.layout_engine.virtual_workspace_manager_mut().assign_window_to_workspace(
                &mut self.state.windows,
                authoritative_space,
                wid,
                target_workspace,
            )
        };
        if !assigned {
            return assigned_space.is_some_and(|space| self.is_space_active(space));
        }

        let target_active = self.is_space_active(authoritative_space);
        let _ = self.restore_window_to_active_layout_if_visible(wid, authoritative_space);

        assigned_space.is_some_and(|space| self.is_space_active(space)) || target_active
    }

    pub(super) fn restore_window_to_active_layout_if_visible(
        &mut self,
        wid: WindowId,
        authoritative_space: SpaceId,
    ) -> bool {
        if !self.is_space_active(authoritative_space) {
            return false;
        }

        let Some(window) = self.state.windows.window(wid) else {
            return false;
        };
        // Same invariant as `reassign_window_to_authoritative_space`: a visible
        // WindowServer id may be a transient fullscreen projection. Do not let
        // visibility alone add it back to the active layout.
        if !window.matches_filter(WindowFilter::EffectivelyManageable) {
            self.send_layout_event(LayoutEvent::WindowRemoved(wid));
            return false;
        }

        let Some(wsid) = window.info.sys_id else {
            return false;
        };
        if !self.state.windows.is_window_visible(wsid) {
            return false;
        }

        let was_on_active_space = self.is_window_on_active_space(wid);
        self.send_layout_event(LayoutEvent::WindowAdded(authoritative_space, wid));
        !was_on_active_space && self.is_window_on_active_space(wid)
    }

    pub(super) fn reconcile_windows_with_authoritative_spaces(&mut self) -> bool {
        if self.refreshes_blocked() {
            self.defer_visible_refresh(true);
            return false;
        }

        let windows: Vec<_> = self.state.windows.iter_windows().map(|(wid, _)| wid).collect();
        let mut layout_changed = false;

        for wid in windows {
            let Some(authoritative_space) = self.affinity().authoritative_space_for_window_id(wid)
            else {
                continue;
            };
            layout_changed |= self.reassign_window_to_authoritative_space(wid, authoritative_space);
        }

        layout_changed
    }

    /// Send the current display geometry to the cursor-warp actor.
    ///
    /// Also re-sends the enabled flag, so a config reload that toggles
    /// `warp_cursor_between_stacked_displays` takes effect without a restart.
    pub(super) fn publish_cursor_warp_screens(&self) {
        let Some(tx) = &self.communication_manager.cursor_warp_tx else {
            return;
        };
        tx.send(crate::displays::platform::cursor_warp::Request::SetEnabled(
            self.config.settings.warp_cursor_between_stacked_displays,
        ));
        tx.send(crate::displays::platform::cursor_warp::Request::SetUpperSide(
            self.config.settings.stacked_display_upper_is,
        ));
        tx.send(crate::displays::platform::cursor_warp::Request::SetLowerTopAt(
            self.config.settings.stacked_display_lower_top_at,
        ));
        tx.send(crate::displays::platform::cursor_warp::Request::ScreensChanged(
            crate::displays::platform::cursor_warp::screens_of(&self.space_state.screens),
        ));
    }
}
