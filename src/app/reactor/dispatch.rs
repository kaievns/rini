//! Each event handed to its reducer: the focus report an event carries taken in first, then the
//! event routed by its family to the reducer that owns it.

use rini_core::ids::{WindowId, WindowServerId};
use rini_ipc::protocol::Command;

use super::events::{EventOutcome, app, command, space, system, window};
use super::{Event, Reactor, ReactorCommand};

/// What the reducers may be handed besides their event: the window the event's focus report
/// raised, whether the event changes which windows have notifications, and whether it repeats the
/// global activation already in force.
struct Dispatch {
    raised_window: Option<WindowId>,
    update_notifications: bool,
    global_activation: GlobalActivation,
}

/// Whether an event is a global activation of the app already globally frontmost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GlobalActivation {
    New,
    Repeated,
}

impl Dispatch {
    /// What an event with nothing of its own to carry out leaves: its focus report's raise.
    fn focus_changed(&self) -> EventOutcome {
        EventOutcome::focus_changed(self.raised_window, self.update_notifications)
    }

    /// What a family's dispatcher leaves for a variant `dispatch_workflow` routed to it that it
    /// has no arm for: the focus report's raise, and a failed debug assertion.
    fn misrouted(&self, event: &Event) -> EventOutcome {
        debug_assert!(
            false,
            "{} routed to the wrong family",
            <&'static str>::from(event)
        );
        self.focus_changed()
    }
}

impl Reactor {
    pub(super) fn dispatch_workflow(&mut self, event: Event) -> anyhow::Result<EventOutcome> {
        self.live_spaces.clear();
        self.log_event(&event);
        let dispatch = self.take_focus_report(&event);
        match event {
            Event::Command(command) => self.dispatch_command(command),
            event @ (Event::ApplicationLaunched { .. }
            | Event::ApplicationTerminated(_)
            | Event::ApplicationThreadTerminated(_)
            | Event::ApplicationActivated(..)
            | Event::ApplicationDeactivated(_)
            | Event::ApplicationGloballyDeactivated(_)
            | Event::ApplicationGloballyActivated(_)
            | Event::ApplicationMainWindowChanged(..)) => {
                self.dispatch_application_event(event, &dispatch)
            }
            event @ (Event::WindowServerFocusChanged(..)
            | Event::WindowsDiscovered { .. }
            | Event::WindowCreated { .. }
            | Event::WindowDestroyed(_)
            | Event::WindowServerDestroyed(..)
            | Event::WindowServerAppeared(..)
            | Event::WindowMinimized(_)
            | Event::WindowDeminiaturized(_)
            | Event::WindowFrameChanged { .. }
            | Event::WindowTitleChanged(..)
            | Event::ApplyOverlayFrames(_)) => self.dispatch_window_event(event, &dispatch),
            event @ (Event::SpaceCreated(_)
            | Event::SpaceDestroyed(_)
            | Event::SpaceStateChanged(_)
            | Event::ActiveDisplayChanged { .. }
            | Event::MissionControlNativeEntered
            | Event::MissionControlNativeExited
            | Event::SystemWillSleep
            | Event::SystemWoke
            | Event::SessionDidResignActive
            | Event::SessionDidBecomeActive
            | Event::DisplayChurnBegin
            | Event::DisplayChurnEnd
            | Event::RegisterWmSender(_)) => self.dispatch_system_event(event, &dispatch),
            event @ (Event::MouseUp
            | Event::LeftMouseDown(_)
            | Event::MouseMoved(_)
            | Event::MenuOpened(_)
            | Event::MenuClosed(_)
            | Event::RaiseCompleted { .. }
            | Event::RaiseTimeout { .. }
            | Event::RaisesIdle { .. }
            | Event::AttentionChanged(_)
            | Event::ConfigUpdated(_)
            | Event::ConfigFileRead { .. }
            | Event::Switch(_)
            | Event::SwitchPicked(_)
            | Event::BarAction(_)
            | Event::ClockChanged) => self.dispatch_input_event(event, &dispatch),
            // Answered by `handle_loop_event` before the event path.
            Event::Query(_) => Ok(dispatch.focus_changed()),
        }
    }

    /// The main-window tracker told of the focus report `event` carries, the flight engine told of
    /// the faces it turned, and what the reducers are handed besides the event.
    fn take_focus_report(&mut self, event: &Event) -> Dispatch {
        let update_notifications = Self::should_update_notifications(event);
        let global_activation = match event {
            Event::ApplicationGloballyActivated(pid)
                if self.main_window_tracker.is_globally_frontmost(*pid) =>
            {
                GlobalActivation::Repeated
            }
            _ => GlobalActivation::New,
        };
        let key_before = self.main_window_tracker.key_window();
        let raised_window = event
            .focus_event()
            .map(|focus| self.recognise_panel(focus))
            .and_then(|focus| self.main_window_tracker.handle_event(focus));
        self.tell_faces_turned(key_before);
        Dispatch {
            raised_window,
            update_notifications,
            global_activation,
        }
    }

    /// An application launching, going, losing its thread, activating, and its main window
    /// changing, which needs no more than the focus tracking and leaves the focus report's raise.
    fn dispatch_application_event(
        &mut self,
        event: Event,
        dispatch: &Dispatch,
    ) -> anyhow::Result<EventOutcome> {
        let raised_window = dispatch.raised_window;
        match event {
            Event::ApplicationLaunched {
                pid,
                info,
                handle,
                visible_windows,
                window_server_info,
                is_frontmost: _,
                main_window: _,
            } => self.on_application_launched(
                app::ApplicationLaunchedPayload {
                    pid,
                    info,
                    handle,
                    visible_windows,
                    window_server_info,
                },
                raised_window,
            ),
            Event::ApplicationTerminated(pid) => {
                app::handle_application_terminated(&self.apps, &mut self.state, pid)
            }
            Event::ApplicationThreadTerminated(pid) => {
                self.clear_menu_state_for_pid(pid);
                app::handle_application_thread_terminated(
                    &mut self.apps,
                    &mut self.state,
                    &self.transactions,
                    &mut self.drag_manager,
                    pid,
                )
            }
            Event::ApplicationActivated(pid, quiet) => {
                self.on_application_activated(pid, quiet, raised_window)
            }
            Event::ApplicationDeactivated(pid) | Event::ApplicationGloballyDeactivated(pid) => {
                self.clear_menu_state_for_pid(pid);
                Ok(dispatch.focus_changed())
            }
            Event::ApplicationGloballyActivated(pid) => self.on_application_globally_activated(
                pid,
                dispatch.global_activation,
                dispatch.update_notifications,
            ),
            Event::ApplicationMainWindowChanged(..) => Ok(dispatch.focus_changed()),
            other => Ok(dispatch.misrouted(&other)),
        }
    }

    /// One window's events, and the windows an application or the window server reports.
    fn dispatch_window_event(
        &mut self,
        event: Event,
        dispatch: &Dispatch,
    ) -> anyhow::Result<EventOutcome> {
        let raised_window = dispatch.raised_window;
        match event {
            Event::WindowServerFocusChanged(window, reported_space) => {
                self.on_window_server_focus_changed(window, reported_space)
            }
            Event::WindowsDiscovered { pid, new, known_visible } => self.on_windows_discovered(
                app::WindowsDiscoveredPayload { pid, new, known_visible },
                raised_window,
            ),
            Event::WindowCreated { window, info, server_info, .. } => self.on_window_created(
                window::WindowCreatedPayload {
                    window_id: window,
                    window: info,
                    window_server_info: server_info,
                },
                raised_window,
            ),
            Event::WindowDestroyed(wid) => self.on_window_destroyed(wid, raised_window),
            Event::WindowServerDestroyed(wsid, sid, kind) => {
                self.on_window_server_destroyed(wsid, sid, kind)
            }
            Event::WindowServerAppeared(wsid, sid, kind) => {
                self.on_window_server_appeared(wsid, sid, kind)
            }
            Event::WindowMinimized(wid) => window::handle_window_minimized(&mut self.state, wid),
            Event::WindowDeminiaturized(wid) => self.on_window_deminiaturized(wid),
            Event::WindowFrameChanged {
                window,
                frame,
                last_seen,
                requested,
                mouse_state,
            } => self.on_window_frame_changed(
                window,
                frame,
                last_seen,
                requested,
                mouse_state,
                raised_window,
            ),
            Event::WindowTitleChanged(wid, new_title) => {
                let mut outcome = window::handle_window_title_changed(
                    &mut self.state,
                    window::WindowTitleChangedPayload { window: wid, title: new_title },
                )?;
                outcome.focused_window = raised_window;
                Ok(outcome)
            }
            Event::ApplyOverlayFrames(frames) => {
                self.apply_overlay_frames(frames);
                Ok(EventOutcome::no_change())
            }
            other => Ok(dispatch.misrouted(&other)),
        }
    }

    /// Spaces created, gone and changed, Mission Control, sleep, the session and display churn,
    /// and the controller's sender.
    fn dispatch_system_event(
        &mut self,
        event: Event,
        dispatch: &Dispatch,
    ) -> anyhow::Result<EventOutcome> {
        match event {
            Event::SpaceCreated(space) => space::handle_space_lifecycle(
                &mut self.space_activation_policy,
                space::SpaceLifecyclePayload { space, created: true },
            ),
            Event::SpaceDestroyed(space) => space::handle_space_lifecycle(
                &mut self.space_activation_policy,
                space::SpaceLifecyclePayload { space, created: false },
            ),
            Event::SpaceStateChanged(space_state) => self.on_space_state_changed(space_state),
            Event::ActiveDisplayChanged { menu_bar_space, command_space } => {
                self.space_state.menu_bar_space = menu_bar_space;
                self.space_state.command_space = command_space;
                Ok(EventOutcome::default())
            }
            Event::MissionControlNativeEntered => space::handle_mission_control_native_entered(
                &mut self.mission_control_manager,
                &mut self.drag_manager,
            ),
            Event::MissionControlNativeExited => {
                space::handle_mission_control_native_exited(&mut self.mission_control_manager)
            }
            Event::SystemWillSleep => Ok(self.on_system_will_sleep()),
            Event::SystemWoke => Ok(self.on_system_woke()),
            Event::SessionDidResignActive => Ok(self.on_session_did_resign_active()),
            Event::SessionDidBecomeActive => Ok(self.on_session_did_become_active()),
            Event::DisplayChurnBegin => Ok(self.on_display_churn_begin()),
            Event::DisplayChurnEnd => Ok(self.on_display_churn_end()),
            Event::RegisterWmSender(sender) => Ok(system::handle_register_wm_sender(
                &mut self.communication_manager,
                sender,
            )),
            other => Ok(dispatch.misrouted(&other)),
        }
    }

    /// The mouse, menus, raises completing, attention, the config, the switcher and the bar.
    fn dispatch_input_event(
        &mut self,
        event: Event,
        dispatch: &Dispatch,
    ) -> anyhow::Result<EventOutcome> {
        match event {
            Event::MouseUp => self.on_mouse_up(),
            Event::LeftMouseDown(press) => {
                self.on_left_mouse_down(press);
                Ok(EventOutcome::no_change())
            }
            Event::MouseMoved(wsid) => self.on_mouse_moved(wsid),
            Event::MenuOpened(pid) => Ok(system::handle_menu_opened(&mut self.menu_state, pid)),
            Event::MenuClosed(pid) => Ok(system::handle_menu_closed(&mut self.menu_state, pid)),
            Event::RaiseCompleted { window_id, sequence_id } => {
                Ok(system::handle_raise_completed(system::RaiseCompletedPayload {
                    window: window_id,
                    sequence: sequence_id,
                }))
            }
            Event::RaiseTimeout { sequence_id } => Ok(system::handle_raise_timeout(sequence_id)),
            Event::RaisesIdle { requests_seen } => {
                self.raise_echo.idle(requests_seen, std::time::Instant::now());
                Ok(EventOutcome::no_change())
            }
            Event::AttentionChanged(attention) => {
                self.attention = attention;
                Ok(EventOutcome::no_change())
            }
            Event::ConfigUpdated(new_cfg) => command::handle_config_updated(
                &mut self.config,
                &mut self.layout_engine,
                &mut self.drag_manager,
                *new_cfg,
            ),
            Event::ConfigFileRead { usable } => {
                self.bar.config_failed = !usable;
                Ok(EventOutcome::no_change())
            }
            Event::Switch(signal) => Ok(self.handle_switch_signal(signal)),
            Event::SwitchPicked(window) => Ok(self.pick_switch(window)),
            Event::BarAction(action) => self.on_bar_action(action),
            Event::ClockChanged => {
                self.bar_clock_changed();
                Ok(EventOutcome::no_change())
            }
            other => Ok(dispatch.misrouted(&other)),
        }
    }

    fn dispatch_command(&mut self, command: Command) -> anyhow::Result<EventOutcome> {
        let command = match command {
            Command::Layout(command) => return self.on_layout_command(command),
            Command::Reactor(command) => command,
        };
        match command {
            ReactorCommand::SaveAndExit => {
                let saved = self.save_layout_before_exit(rini_core::paths::restore_file());
                command::handle_command_reactor_save_and_exit(saved)
            }
            ReactorCommand::RedistributeWindows => Ok(self.redistribute_windows()),
            ReactorCommand::SwitchWindow { backward, scope } => {
                Ok(self.switch_window(backward, scope))
            }
            ReactorCommand::ToggleSpaceActivated => self.on_toggle_space_activated(),
            ReactorCommand::CloseWindow { window_server_id } => {
                command::handle_close_window(window_server_id.map(WindowServerId::new))
            }
            ReactorCommand::FocusWindow { window_id, window_server_id } => {
                self.on_focus_window(window_id, window_server_id)
            }
            ReactorCommand::MoveMouseToDisplay(selector) => self.on_move_mouse_to_display(selector),
            ReactorCommand::FocusDisplay(selector) => self.on_focus_display(selector),
            ReactorCommand::MoveWindowToDisplay { selector, window_id } => {
                self.on_move_window_to_display(selector, window_id)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::reactor::testing::test_reactor;
    use crate::windows::domain::request::Quiet;

    fn focus_report(raised_window: Option<WindowId>) -> Dispatch {
        Dispatch {
            raised_window,
            update_notifications: true,
            global_activation: GlobalActivation::New,
        }
    }

    #[test]
    fn a_main_window_change_leaves_only_the_focus_report() {
        let mut reactor = test_reactor();
        let raised = WindowId::new(1, 1);
        let outcome = reactor
            .dispatch_application_event(
                Event::ApplicationMainWindowChanged(1, Some(raised), Quiet::No),
                &focus_report(Some(raised)),
            )
            .unwrap();
        assert_eq!(outcome.focused_window, Some(raised));
        assert!(outcome.refresh_window_notifications);
        assert!(outcome.layout_events.is_empty() && outcome.app_requests.is_empty());
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "system_woke routed to the wrong family")]
    fn a_variant_routed_to_the_wrong_family_fails_the_suite() {
        let mut reactor = test_reactor();
        let _ = reactor.dispatch_input_event(Event::SystemWoke, &focus_report(None));
    }
}
