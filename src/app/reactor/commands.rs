//! What a command means before it is carried out.
//!
//! A command names its target loosely — "the window", "that display", "index 3" — and the mutation
//! that follows needs it named exactly. Resolving the two apart is what these methods do: they read
//! the reactor, decide, and hand a settled payload to a reducer in `super::events::command`.
//!
//! Commands whose payload arrives already settled are dispatched straight from
//! `super::Reactor::dispatch_workflow` and are not here.

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::centered_in;
use rini_ipc::protocol::{DisplaySelector, LayoutCommand};
use tracing::{info, warn};

use super::Reactor;
use super::events::{EventOutcome, command};
use crate::displays::domain::screen::ScreenInfo;

impl Reactor {
    pub(super) fn on_toggle_space_activated(&mut self) -> anyhow::Result<EventOutcome> {
        let space = self.active_display_space();
        let display_uuid = space.and_then(|space| {
            self.space_state
                .screen_by_space(space)
                .and_then(|screen| screen.display_uuid_owned())
        });
        let config = self.activation_cfg();
        command::handle_command_reactor_toggle_space_activated(
            &mut self.space_activation_policy,
            command::ToggleSpacePayload { config, space, display_uuid },
        )
    }

    pub(super) fn on_focus_window(
        &mut self,
        window_id: rini_ipc::protocol::WindowId,
        window_server_id: Option<u32>,
    ) -> anyhow::Result<EventOutcome> {
        let window_id = WindowId::new(window_id.pid, window_id.idx);
        let window_server_id = window_server_id.map(WindowServerId::new);
        let resolved_space = self.best_space_for_tracked_window(window_id);
        command::handle_command_reactor_focus_window(
            &self.state,
            &self.apps,
            command::FocusWindowPayload {
                window_id,
                window_server_id,
                resolved_space,
                space_is_active: resolved_space.is_some_and(|space| self.is_space_active(space)),
            },
        )
    }

    /// The space a window is on, preferring what it is assigned to over where its frame sits.
    fn best_space_for_tracked_window(&self, window: WindowId) -> Option<rini_core::ids::SpaceId> {
        self.affinity().best_space_for_window_id(window).or_else(|| {
            let state = self.state.windows.window(window)?;
            self.affinity().best_space_for_window(&state.frame_monotonic, state.info.sys_id)
        })
    }

    /// Which display a selector names, and the window to focus once the cursor is on it.
    ///
    /// `focus-display` and `move-mouse-to-display` differ only in what they do with this.
    fn resolve_display_focus(&self, selector: &DisplaySelector) -> command::DisplayFocusPayload {
        let screen = self.screen_for_selector(selector, None).cloned();
        let focus_window = screen.as_ref().and_then(|screen| {
            let space = screen.space?;
            self.last_focused_window_in_space(space).or_else(|| {
                self.layout_engine
                    .windows_in_active_workspace(&self.state.windows, space)
                    .into_iter()
                    .next()
            })
        });
        let target_is_active = screen
            .as_ref()
            .and_then(|screen| screen.space)
            .is_none_or(|space| self.is_space_active(space));
        command::DisplayFocusPayload {
            screen,
            target_is_active,
            focus_window,
        }
    }

    pub(super) fn on_move_mouse_to_display(
        &mut self,
        selector: DisplaySelector,
    ) -> anyhow::Result<EventOutcome> {
        command::handle_move_mouse_to_display(self.resolve_display_focus(&selector))
    }

    pub(super) fn on_focus_display(
        &mut self,
        selector: DisplaySelector,
    ) -> anyhow::Result<EventOutcome> {
        command::handle_focus_display(self.resolve_display_focus(&selector))
    }

    pub(super) fn on_layout_command(
        &mut self,
        command: LayoutCommand,
    ) -> anyhow::Result<EventOutcome> {
        if !matches!(command, LayoutCommand::ToggleFocusFloating) {
            self.resume_overview_on_strip();
        }
        let command_space = self.command_context_space();
        self.on_layout_command_in(command, command_space)
    }

    /// `command` carried out on `command_space` rather than on whichever display the command came
    /// from.
    ///
    /// Logged by its wire name, `cmd=MoveFocus(Right)`, which is what a press is found by in the log.
    pub(super) fn on_layout_command_in(
        &mut self,
        command: LayoutCommand,
        command_space: Option<rini_core::ids::SpaceId>,
    ) -> anyhow::Result<EventOutcome> {
        info!(cmd = ?command);
        // What a window does after a command answers rini, not a double-click before it.
        self.drag_manager.title_bar_click = None;
        let (visible_spaces, visible_space_centers) = self.visible_spaces_for_layout(false);
        command::handle_command_layout(
            &mut self.state,
            &mut self.layout_engine,
            &mut self.workspace_switch_manager,
            command::LayoutCommandPayload {
                command: command.into(),
                command_space,
                visible_spaces,
                visible_space_centers,
            },
        )
    }

    /// Which window `move-window-to-display` means.
    ///
    /// An explicit index is looked up on the command's own space first, then on any active space, so
    /// the number a status bar shows resolves even when the pointer has wandered. With no index it is
    /// the main window, else the one under the cursor, else the first on the command space.
    fn window_for_move_command(&self, index: Option<u32>) -> Option<WindowId> {
        let command_space = self.workspace_command_space();
        let workspaces = self.layout_engine.virtual_workspace_manager();
        let by_index = |index| {
            command_space
                .and_then(|space| workspaces.find_window_by_idx(&self.state.windows, space, index))
                .or_else(|| {
                    self.iter_active_spaces().find_map(|space| {
                        workspaces.find_window_by_idx(&self.state.windows, space, index)
                    })
                })
        };
        match index {
            Some(index) => by_index(index),
            None => self
                .main_window()
                .or_else(|| self.window_id_under_cursor())
                .or_else(|| by_index(0)),
        }
    }

    /// The active space a window is currently on, by assignment, then by id, then by frame.
    fn source_space_for_move(
        &self,
        window: WindowId,
        frame: &objc2_core_foundation::CGRect,
        window_server_id: Option<WindowServerId>,
    ) -> Option<rini_core::ids::SpaceId> {
        self.affinity()
            .assigned_space_for_window_id(window)
            .or_else(|| self.affinity().best_space_for_window_id(window))
            .or_else(|| self.affinity().best_space_for_window(frame, window_server_id))
            .filter(|space| self.is_space_active(*space))
    }

    pub(super) fn on_move_window_to_display(
        &mut self,
        selector: DisplaySelector,
        window_id: Option<u32>,
    ) -> anyhow::Result<EventOutcome> {
        if self.is_in_drag() {
            warn!("Ignoring move-window-to-display while a drag is active");
            return Ok(EventOutcome::no_change());
        }
        let window = match window_id {
            None if self.overview_covers_the_pointer() => self.overview_press_window(),
            _ => self.window_for_move_command(window_id),
        };
        let Some(window) = window else {
            warn!("Move window to display ignored because no target window was resolved");
            return Ok(EventOutcome::no_change());
        };
        let Some(window_state) = self.state.windows.window(window) else {
            warn!(?window, "Move window to display ignored: unknown window");
            return Ok(EventOutcome::no_change());
        };
        let window_server_id = window_state.info.sys_id;
        let window_frame = window_state.frame_monotonic;
        let Some(source_space) =
            self.source_space_for_move(window, &window_frame, window_server_id)
        else {
            warn!(
                ?window,
                "Move window to display ignored: source space unavailable"
            );
            return Ok(EventOutcome::no_change());
        };
        let Some(target_screen) = self.target_screen_for_move(&selector, source_space) else {
            warn!(
                ?selector,
                "Move window to display ignored: target display not found"
            );
            return Ok(EventOutcome::no_change());
        };
        let Some(target_space) = target_screen.space.filter(|space| self.is_space_active(*space))
        else {
            warn!(
                ?selector,
                "Move window to display ignored: target space unavailable"
            );
            return Ok(EventOutcome::no_change());
        };
        if source_space == target_space {
            return Ok(EventOutcome::no_change());
        }
        command::handle_command_reactor_move_window_to_display(
            &mut self.state,
            &mut self.layout_engine,
            command::MoveWindowToDisplayPayload {
                window,
                window_server_id,
                source_space,
                target_space,
                target_screen: target_screen.frame,
                target_frame: centered_in(window_frame, target_screen.frame),
            },
        )
    }

    /// The display a selector names, measured from the window's current display so that "next" and
    /// "previous" step from where the window is rather than from where the pointer is.
    fn target_screen_for_move(
        &self,
        selector: &DisplaySelector,
        source_space: rini_core::ids::SpaceId,
    ) -> Option<ScreenInfo> {
        let origin = self
            .space_state
            .screen_by_space(source_space)
            .map(|screen| screen.frame.mid())
            .or_else(|| self.current_screen_center());
        self.space_state.screen_for_selector(selector, origin).cloned()
    }
}
