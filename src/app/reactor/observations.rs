//! What the system reported, before anything is decided about it.
//!
//! The other half of the reactor's inbox from `super::commands`: nobody asked for these. A window
//! moved, an app launched, the machine went to sleep. Each method here gathers what the reducers in
//! `super::events` need — which space a frame is on, which screens exist, what the drag manager is
//! doing — and hands it over as a settled payload.
//!
//! Observations that need no gathering are dispatched straight from
//! `super::Reactor::dispatch_workflow` and are not here.

use objc2_core_foundation::CGRect;

use rini_core::ids::{SpaceId, WindowId, WindowServerId};

use crate::displays::domain::topology::SpaceEventKind;
use crate::windows::domain::info::AppInfo;
use crate::windows::domain::transaction::{Requested, TransactionId};
use crate::windows::platform::mouse::MouseState;
use crate::windows::platform::window_server;

use tracing::{debug, trace};

use rini_core::ids::pid_t;

use crate::displays::domain::topology::ForwardedSpaceState;
use crate::windows::domain::request::{Quiet, Request};
use crate::workspaces::LayoutEvent;

use super::DragState;
use super::events::EventOutcome;
use super::events::app as application_workflow;
use super::events::drag as interaction_workflow;
use super::events::space as topology_workflow;
use super::events::system as system_workflow;
use super::events::window as window_workflow;
use super::{Event, Reactor};

impl Reactor {
    /// The six events that only move the refresh quarantine: sleep, wake, session activation and
    /// display churn.
    ///
    /// They are answered before focus tracking runs, because none of them is a focus change and the
    /// quarantine has to close before the snapshot that follows is trusted. `None` means the event is
    /// not one of them.
    pub(super) fn on_quarantine_event(
        &mut self,
        event: &Event,
    ) -> Option<anyhow::Result<EventOutcome>> {
        let quarantine = &mut self.refresh_quarantine_manager;
        match event {
            Event::SystemWillSleep => {
                quarantine.sleeping = true;
                quarantine.awaiting_post_wake_snapshot = false;
                // Sleep is the last chance to persist before a possible reboot, so write out any
                // debounced change rather than risk losing it.
                if self.autosave_pending {
                    self.save_layout_now();
                }
                Some(Ok(EventOutcome::default()))
            }
            Event::SystemWoke => {
                quarantine.sleeping = true;
                quarantine.awaiting_post_wake_snapshot = true;
                let outcome = system_workflow::handle_system_woke();
                self.defer_visible_refresh(true);
                Some(outcome.map_err(Into::into))
            }
            Event::SessionDidResignActive => {
                quarantine.session_inactive = true;
                quarantine.awaiting_post_session_snapshot = false;
                Some(Ok(EventOutcome::default()))
            }
            Event::SessionDidBecomeActive => {
                quarantine.session_inactive = true;
                quarantine.awaiting_post_session_snapshot = true;
                self.defer_visible_refresh(true);
                Some(Ok(EventOutcome::default()))
            }
            Event::DisplayChurnBegin => {
                quarantine.display_churn_active = true;
                Some(Ok(EventOutcome::default()))
            }
            Event::DisplayChurnEnd => {
                quarantine.display_churn_active = false;
                self.request_refresh_when_spaces_actor_stabilizes();
                Some(Ok(EventOutcome::default()))
            }
            _ => None,
        }
    }
}

/// What a window-server lifecycle event needs to know about the window it names.
///
/// Gathered before the reducer runs, because "appeared" and "was destroyed" both have to be read
/// against where rini thought the window was, and the reducer is not allowed to ask the reactor.
pub(super) struct WindowServerContext {
    pub assigned_space: Option<SpaceId>,
    pub last_known_user_space: Option<SpaceId>,
}

impl Reactor {
    pub(super) fn window_server_context(&self, wsid: WindowServerId) -> WindowServerContext {
        let tracked_window = self.state.windows.tracked_window_id(wsid);
        WindowServerContext {
            assigned_space: tracked_window
                .and_then(|window| self.affinity().assigned_space_for_window_id(window)),
            last_known_user_space: topology_workflow::resolve_last_known_user_space(
                tracked_window.and_then(|window| self.affinity().best_space_for_window_id(window)),
                self.space_state.iter_known_spaces().next(),
            ),
        }
    }

    /// The active space a frame sits on, or the command space when there is no window-server id.
    ///
    /// Without a server id the window cannot be placed geometrically at all, so falling back to the
    /// space the command came from is the only answer available.
    fn active_space_for_frame(
        &self,
        frame: &CGRect,
        server_id: Option<WindowServerId>,
    ) -> Option<SpaceId> {
        self.affinity()
            .best_space_for_window(frame, server_id)
            .filter(|space| self.is_space_active(*space))
            .or_else(|| server_id.is_none().then(|| self.workspace_command_space()).flatten())
    }

    /// As [`Self::active_space_for_frame`], for a window rini is tracking.
    fn active_space_for_window(&self, window: WindowId) -> Option<SpaceId> {
        let state = self.state.windows.window(window)?;
        self.active_space_for_frame(&state.frame_monotonic, state.info.sys_id)
    }

    pub(super) fn on_window_server_destroyed(
        &mut self,
        wsid: WindowServerId,
        space: SpaceId,
        kind: SpaceEventKind,
    ) -> anyhow::Result<EventOutcome> {
        let context = self.window_server_context(wsid);
        let observations = topology_workflow::WindowServerDestroyedObservations {
            resolved_space: self.affinity().resolve_native_space(wsid, None),
            active_spaces: self.active_spaces.clone(),
            mission_control_active: self.is_mission_control_active(),
            ordered_in: window_server::window_ordered_in(wsid),
            assigned_space: context.assigned_space,
            last_known_user_space: context.last_known_user_space,
        };
        topology_workflow::handle_window_server_destroyed(
            &mut self.state,
            &self.transaction_manager,
            &mut self.drag_manager,
            topology_workflow::WindowServerLifecyclePayload {
                window_server_id: wsid,
                space,
                kind,
            },
            observations,
        )
    }

    /// Whether an appearance on `space` is rini's own parking rather than a move.
    ///
    /// A window belonging to a workspace its display is not showing is one rini parked off-screen, so
    /// its POSITION proves nothing — the parked coordinates deliberately sit inside the neighbouring
    /// display. WindowServer's own space membership still does prove something: Mission Control and a
    /// genuine cross-display move both update it, whereas parking only changes coordinates. So the
    /// appearance is distrusted only when membership fails to corroborate it.
    fn is_parked_by_rini(&self, wsid: WindowServerId, space: SpaceId) -> bool {
        self.state
            .windows
            .tracked_window_id(wsid)
            .and_then(|wid| {
                let assignment = self.state.windows.workspace_info_for_window(wid)?;
                let showing = self.layout_manager.layout_engine.active_workspace(assignment.space)?;
                if assignment.workspace_id == showing {
                    return Some(false);
                }
                Some(!window_server::window_spaces(wsid).contains(&space))
            })
            .unwrap_or(false)
    }

    pub(super) fn on_window_server_appeared(
        &mut self,
        wsid: WindowServerId,
        space: SpaceId,
        kind: SpaceEventKind,
    ) -> anyhow::Result<EventOutcome> {
        let context = self.window_server_context(wsid);
        let window_server_info = window_server::get_window(wsid);
        let owner_pid = window_server_info.as_ref().map(|info| info.pid);
        let app_known = owner_pid.is_some_and(|pid| self.app_manager.apps.contains_key(&pid));
        let running_app_info = owner_pid.filter(|_| !app_known).and_then(|pid| {
            objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                .map(|app| AppInfo::from(&*app))
        });
        let observations = topology_workflow::WindowServerAppearedObservations {
            is_parked_by_rini: self.is_parked_by_rini(wsid, space),
            resolved_space: self.affinity().resolve_native_space(wsid, Some(space)),
            active_spaces: self.active_spaces.clone(),
            mission_control_active: self.is_mission_control_active(),
            assigned_space: context.assigned_space,
            last_known_user_space: context.last_known_user_space,
            window_server_info,
            app_known,
            running_app_info,
        };
        topology_workflow::handle_window_server_appeared(
            &mut self.state,
            topology_workflow::WindowServerLifecyclePayload {
                window_server_id: wsid,
                space,
                kind,
            },
            observations,
        )
    }

    pub(super) fn on_window_deminiaturized(
        &mut self,
        window: WindowId,
    ) -> anyhow::Result<EventOutcome> {
        let active_space = self.active_space_for_window(window);
        window_workflow::handle_window_deminiaturized(
            &mut self.state,
            window_workflow::WindowDeminiaturizedPayload { window, active_space },
        )
    }

    pub(super) fn on_mouse_moved(&mut self, wsid: WindowServerId) -> anyhow::Result<EventOutcome> {
        let window = self.state.windows.tracked_window_id(wsid);
        let active_space = window.and_then(|window| self.active_space_for_window(window));
        window_workflow::handle_mouse_moved_over_window(
            &self.app_manager,
            window_workflow::MouseMovedPayload {
                window,
                should_sync: window.is_some_and(|window| self.should_raise_on_mouse_over(window)),
                is_main: window.is_some_and(|window| self.main_window() == Some(window)),
                needs_layout_sync: window.is_some_and(|window| {
                    self.layout_manager.layout_engine.focused_window() != Some(window)
                }),
                active_space,
            },
        )
    }

    pub(super) fn on_window_frame_changed(
        &mut self,
        window: WindowId,
        new_frame: CGRect,
        last_seen: Option<TransactionId>,
        requested: Requested,
        mouse_state: Option<MouseState>,
        raised_window: Option<WindowId>,
    ) -> anyhow::Result<EventOutcome> {
        let mission_control_active = self.is_mission_control_active();
        let mut mouse_state = mouse_state;
        let disposition = window_workflow::classify_window_frame_change(
            &mut self.state,
            &self.transaction_manager,
            &mut self.drag_manager,
            window,
            new_frame,
            last_seen,
            requested.0,
            &mut mouse_state,
            mission_control_active,
        );
        // A mouse release still has to terminate an open drag session, including on the paths the
        // reducer returns early from: frame acknowledgements and no-op geometry changes.
        let ends_a_drag = mouse_state == Some(MouseState::Up) && self.is_in_drag();
        if matches!(disposition, window_workflow::FrameChangeDisposition::Handled) {
            let mut outcome = EventOutcome::no_change();
            outcome.dispatch_mouse_up = ends_a_drag;
            outcome.focused_window = raised_window;
            return Ok(outcome);
        }
        let (server_id, old_frame) = self
            .state
            .windows
            .window(window)
            .map(|state| (state.info.sys_id, state.frame_monotonic))
            .unwrap_or((None, new_frame));
        let old_space = self.affinity().geometry_space_for_window(&old_frame, server_id);
        let new_space = self.affinity().geometry_space_for_window(&new_frame, server_id);
        let payload = window_workflow::WindowFrameChangedPayload {
            window,
            new_frame,
            mouse_state,
            old_space,
            new_space,
            old_space_active: old_space.is_some_and(|space| self.is_space_active(space)),
            new_space_active: new_space.is_some_and(|space| self.is_space_active(space)),
            active_resize_space: self.active_space_for_frame(&new_frame, server_id),
            pending_target_space: server_id.and_then(|server| {
                self.affinity().pending_target_space_for_window_server_id(server)
            }),
            assigned_space: self.affinity().assigned_space_for_window_id(window),
            keep_assigned_for_scrolling: old_space.is_some_and(|space| {
                !self.layout_manager.layout_engine.is_window_floating(window)
                    && self
                        .layout_manager
                        .layout_engine
                        .virtual_workspace_manager()
                        .workspace_for_window(&self.state.windows, space, window)
                        .is_some()
            }),
            screens: self
                .space_state
                .screens
                .iter()
                .filter_map(|screen| Some((screen.space?, screen.frame, screen.display_uuid_owned())))
                .collect(),
        };
        let mut outcome = window_workflow::handle_window_frame_changed(
            &mut self.state,
            &mut self.layout_manager,
            &mut self.drag_manager,
            payload,
        )?;
        if ends_a_drag {
            outcome.dispatch_mouse_up = true;
        }
        outcome.focused_window = raised_window;
        Ok(outcome)
    }
}

impl Reactor {
    pub(super) fn on_application_launched(
        &mut self,
        payload: application_workflow::ApplicationLaunchedPayload,
        raised_window: Option<WindowId>,
    ) -> anyhow::Result<EventOutcome> {
        let pid = payload.pid;
        let mut outcome =
            application_workflow::handle_application_launched(&mut self.app_manager, payload)?;
        if self.main_window_tracker.is_globally_frontmost(pid) {
            outcome.app_requests.push((pid, Request::ApplicationGloballyActivated(pid)));
        }
        outcome.focused_window = raised_window;
        Ok(outcome)
    }

    pub(super) fn on_application_activated(
        &mut self,
        pid: pid_t,
        quiet: Quiet,
        raised_window: Option<WindowId>,
    ) -> anyhow::Result<EventOutcome> {
        self.clear_menu_state_for_non_owner(pid);
        let mut outcome = application_workflow::handle_application_activated(
            application_workflow::ApplicationActivatedPayload { pid, quiet },
        )?;
        if quiet == Quiet::No {
            outcome.absorb(self.handle_app_activation_workspace_switch(pid));
        }
        outcome.focused_window = raised_window;
        Ok(outcome)
    }

    /// A global activation the tracker has already seen is dropped.
    ///
    /// The app thread resolves the current AX main window and emits `ApplicationActivated` in reply,
    /// so cached focus is deliberately not replayed here.
    pub(super) fn on_application_globally_activated(
        &mut self,
        pid: pid_t,
        duplicate: bool,
        should_update_notifications: bool,
    ) -> anyhow::Result<EventOutcome> {
        if duplicate {
            trace!(pid, "Ignoring duplicate global application activation");
            return Ok(EventOutcome::focus_changed(None, should_update_notifications));
        }
        self.clear_menu_state_for_non_owner(pid);
        if !self.is_login_window_pid(pid)
            && let Some(app) = self.app_manager.apps.get(&pid)
        {
            let _ = app.handle.send(Request::ApplicationGloballyActivated(pid));
        }
        Ok(EventOutcome::focus_changed(None, should_update_notifications))
    }

    pub(super) fn on_window_server_focus_changed(
        &mut self,
        window: WindowId,
        reported_space: SpaceId,
    ) -> anyhow::Result<EventOutcome> {
        if self.layout_manager.layout_engine.focused_window() == Some(window) {
            if let Some(event_tap_tx) = &self.communication_manager.event_tap_tx {
                _ = event_tap_tx.send(crate::input::platform::input_tap::Request::EnforceHidden);
            }
            return Ok(EventOutcome::default());
        }
        if !self.state.windows.contains_window(window) {
            if let Some(app) = self.app_manager.apps.get(&window.pid) {
                let _ = app.handle.send(Request::GetVisibleWindows);
            }
            return Ok(EventOutcome::default());
        }
        if !self.is_space_active(reported_space) {
            return Ok(EventOutcome::default());
        }
        // rini's own raise, coming back as a focus report. Following it moves the layout's selection
        // to a window the user did not ask for, and the strip scrolls to it.
        if self.raise_echo.swallows(window, std::time::Instant::now()) {
            trace!(?window, "ignoring a focus report provoked by rini's own raise");
            return Ok(EventOutcome::default());
        }
        // Whichever window gains or loses focus renders differently now, so both need a fresh
        // picture before the next animation draws them.
        self.refresh_focus_pictures(window);
        // An app activation picked its own window, and it may not be the one the user was in.
        if let Some(redirect) = self.activation_redirect(window, reported_space) {
            return Ok(redirect);
        }
        // Follow focus to the window's own workspace.
        //
        // Auto-switching only happened on APP activation, so cmd-` — which cycles windows inside the
        // already-active app — moved focus to a window sitting in another workspace without the
        // display switching to it. The window is parked off-screen, so focus went somewhere invisible
        // and the keystroke looked like it had done nothing.
        let outcome =
            self.maybe_auto_switch_to_window_workspace(window.pid, window, reported_space);
        // Once per activation: whatever was not used above is not used later either.
        self.main_window_tracker.take_activation_target(window.pid);
        if outcome.arrange.requested {
            return Ok(outcome);
        }
        // A click raises only the window under the cursor, which splits the strip around a floating
        // window. The strip is one group, so `apply_event_outcome` lifts all of it once this focus has
        // landed (`regroup_after_layout`).
        Ok(EventOutcome::default()
            .with_layout_event(LayoutEvent::WindowFocused(reported_space, window)))
    }

    pub(super) fn on_windows_discovered(
        &mut self,
        payload: application_workflow::WindowsDiscoveredPayload,
        raised_window: Option<WindowId>,
    ) -> anyhow::Result<EventOutcome> {
        if self.refreshes_blocked() {
            debug!(
                pid = payload.pid,
                state = ?self.refresh_quarantine_state(),
                "Ignoring windows discovery while refresh quarantine is active"
            );
            self.defer_visible_refresh(true);
            return Ok(EventOutcome::default());
        }
        let mut outcome = application_workflow::handle_windows_discovered(payload)?;
        outcome.focused_window = raised_window;
        Ok(outcome)
    }

    pub(super) fn on_window_created(
        &mut self,
        payload: window_workflow::WindowCreatedPayload,
        raised_window: Option<WindowId>,
    ) -> anyhow::Result<EventOutcome> {
        let mut outcome = window_workflow::handle_window_created(
            &mut self.state,
            &self.transaction_manager,
            payload,
        )?;
        outcome.focused_window = raised_window;
        Ok(outcome)
    }

    pub(super) fn on_window_destroyed(
        &mut self,
        window: WindowId,
        raised_window: Option<WindowId>,
    ) -> anyhow::Result<EventOutcome> {
        // macOS can replace AXUIElements during lifecycle/display churn while the native window
        // remains alive. Recovery already schedules a stable refresh, so preserve topology until then.
        // Outside churn, retain the original AX destruction behaviour and remove the window at once.
        if self.refreshes_blocked() {
            return Ok(EventOutcome::default());
        }
        // A closed window disappears; only its cached picture has to go. Once: the window-server path
        // may have removed the window, and forgotten it, already. See "A closed window disappears" in
        // `src/animation/docs/animation-smoothness.md`.
        if self.state.windows.window(window).is_some()
            && let Some(tx) = &self.communication_manager.workspace_animation_tx
        {
            _ = tx.send(crate::animation::platform::engine::Event::ForgetWindow(window));
        }
        let mut outcome = window_workflow::handle_window_destroyed(
            &mut self.state,
            &self.transaction_manager,
            &mut self.drag_manager,
            window_workflow::WindowDestroyedPayload { window },
        )?;
        outcome.focused_window = raised_window;
        Ok(outcome)
    }

    pub(super) fn on_space_state_changed(
        &mut self,
        space_state: ForwardedSpaceState,
    ) -> anyhow::Result<EventOutcome> {
        let releases_lifecycle = space_state.releases_lifecycle_refresh_quarantine;
        let releases_display_churn = space_state.releases_display_churn_refresh_quarantine;
        let outcome = self.handle_authoritative_space_snapshot(space_state)?;
        if releases_lifecycle {
            self.release_post_instability_quarantine_after_authoritative_snapshot();
        }
        if releases_display_churn {
            self.refresh_quarantine_manager.display_churn_active = false;
            self.request_refresh_when_spaces_actor_stabilizes();
            // A display was plugged in or unplugged. Flush any debounced save now: this is exactly
            // the transition where the arrangement about to be replaced is the one worth remembering,
            // and it is also when the layout file gets consulted to put windows back on the display
            // they came from.
            if self.autosave_pending {
                self.save_layout_now();
            }
        }
        Ok(outcome)
    }

    pub(super) fn on_mouse_up(&mut self) -> anyhow::Result<EventOutcome> {
        let pending_swap = self.get_pending_drag_swap();
        let (visible_spaces, visible_space_centers) = self.visible_spaces_for_layout(true);
        let swap_space = pending_swap
            .and_then(|(dragged, _)| {
                let state = self.state.windows.window(dragged)?;
                self.affinity().best_space_for_window(&state.frame_monotonic, state.info.sys_id)
            })
            .or_else(|| {
                self.drag_manager
                    .drag_swap_manager
                    .origin_frame()
                    .and_then(|frame| self.affinity().best_space_for_frame(&frame))
            })
            .or_else(|| self.space_state.screens.iter().find_map(|screen| screen.space));
        let session = match &self.drag_manager.drag_state {
            DragState::Active { session } | DragState::PendingSwap { session, .. } => {
                Some(session.clone())
            }
            DragState::Inactive => None,
        };
        let final_space = session.as_ref().and_then(|session| {
            session
                .settled_space
                .or_else(|| self.affinity().best_space_for_frame(&session.last_frame))
                .or_else(|| self.affinity().best_space_for_window_id(session.window))
        });
        let focused = self.window_id_under_cursor().and_then(|window| {
            self.affinity().best_space_for_window_id(window).map(|space| (space, window))
        });
        let mut outcome = interaction_workflow::handle_mouse_up(
            &mut self.state,
            &mut self.layout_manager,
            &mut self.drag_manager,
            interaction_workflow::MouseUpPayload {
                pending_swap,
                swap_space,
                final_space,
                visible_spaces,
                visible_space_centers,
            },
        )?;
        if let Some((space, window)) = focused {
            outcome = outcome.with_layout_event(LayoutEvent::WindowFocused(space, window));
        }
        Ok(outcome)
    }
}
