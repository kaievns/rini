use tracing::{error, info, warn};

use super::super::ScreenInfo;
use crate::windows::domain::request::{AppThreadHandle, Quiet};
use rini_core::ids::WindowId;
use crate::windows::domain::raise as raise_manager;
use crate::app::reactor::WorkspaceSwitchOrigin;
use crate::app::reactor::events::EventOutcome;
use crate::app::reactor::managers::{
    AppManager, DragManager, LayoutManager, WorkspaceSwitchManager,
};
use crate::displays::domain::topology::ForwardedSpaceState;
use rustc_hash::FxHashMap as HashMap;
use crate::app::config::Config;
use crate::app::logging::{MetricsCommand, handle_command as handle_metrics_command};
use crate::workspaces::{EventResponse, LayoutCommand, LayoutEvent};
use crate::app::reactor::state::RiniState;
use crate::displays::domain::space_activation::{
    SpaceActivationConfig, SpaceActivationPolicy, ToggleSpaceContext,
};
use rini_core::ids::SpaceId;
use rini_core::ids::WindowServerId;

#[derive(Debug, Clone)]
pub struct LayoutCommandPayload {
    pub command: LayoutCommand,
    pub command_space: Option<SpaceId>,
    pub visible_spaces: Vec<SpaceId>,
    pub visible_space_centers: HashMap<SpaceId, objc2_core_foundation::CGPoint>,
}

pub fn handle_command_layout(
    state: &mut RiniState,
    layout: &mut LayoutManager,
    workspace_switch: &mut WorkspaceSwitchManager,
    payload: LayoutCommandPayload,
) -> anyhow::Result<EventOutcome> {
    let LayoutCommandPayload {
        command: cmd,
        command_space,
        visible_spaces,
        visible_space_centers,
    } = payload;
    info!(?cmd);
    let is_workspace_switch = matches!(
        cmd,
        LayoutCommand::NextWorkspace(_)
            | LayoutCommand::PrevWorkspace(_)
            | LayoutCommand::SwitchToWorkspace(_)
            | LayoutCommand::MoveWindowToWorkspace { follow: true, .. }
            | LayoutCommand::SwitchToLastWorkspace
    );
    let requires_workspace_space = matches!(
        cmd,
        LayoutCommand::NextWorkspace(_)
            | LayoutCommand::PrevWorkspace(_)
            | LayoutCommand::SwitchToWorkspace(_)
            | LayoutCommand::MoveWindowToWorkspace { follow: true, .. }
            | LayoutCommand::CreateWorkspace
            | LayoutCommand::SwitchToLastWorkspace
    );
    let is_virtual_workspace_command = matches!(
        cmd,
        LayoutCommand::NextWorkspace(_)
            | LayoutCommand::PrevWorkspace(_)
            | LayoutCommand::SwitchToWorkspace(_)
            | LayoutCommand::MoveWindowToWorkspace { .. }
            | LayoutCommand::CreateWorkspace
            | LayoutCommand::SwitchToLastWorkspace
    );
    let workspace_space = if requires_workspace_space {
        if let Some(space) = command_space {
            store_current_floating_positions(state, layout, space);
        }
        command_space
    } else {
        None
    };
    if is_workspace_switch {
        workspace_switch.start_workspace_switch(WorkspaceSwitchOrigin::Manual);
    } else {
        workspace_switch.mark_workspace_switch_inactive();
    }

    let response = match &cmd {
        LayoutCommand::NextWorkspace(_)
        | LayoutCommand::PrevWorkspace(_)
        | LayoutCommand::SwitchToWorkspace(_)
        | LayoutCommand::CreateWorkspace
        | LayoutCommand::SwitchToLastWorkspace => {
            if let Some(space) = workspace_space {
                layout.layout_engine.handle_virtual_workspace_command(
                    &mut state.windows,
                    space,
                    &cmd,
                )
            } else {
                EventResponse::default()
            }
        }
        LayoutCommand::MoveWindowToWorkspace { .. } => {
            if let Some(space) = command_space {
                layout.layout_engine.handle_virtual_workspace_command(
                    &mut state.windows,
                    space,
                    &cmd,
                )
            } else {
                EventResponse::default()
            }
        }
        _ => {
            if visible_spaces.is_empty() {
                warn!("Layout command ignored: no active spaces");
                return Ok(EventOutcome::no_change());
            }
            layout.layout_engine.handle_command(
                &mut state.windows,
                command_space,
                &visible_spaces,
                &visible_space_centers,
                cmd,
            )
        }
    };

    // A blocked workspace step still reaches `handle_layout_response` when it hit an end, so the
    // stack can bounce; nothing else about it is worth an arrange.
    if is_virtual_workspace_command && !response.changed {
        return Ok(if response.edge_hit.is_some() {
            EventOutcome::no_change().with_layout_response(response, workspace_space)
        } else {
            EventOutcome::no_change()
        });
    }

    let arrange_space_scope = is_workspace_switch.then_some(workspace_space).flatten();
    Ok(EventOutcome::layout_changed(false)
        .with_layout_response(response, workspace_space)
        .with_arrange_space_scope(arrange_space_scope))
}

fn current_floating_positions(
    state: &RiniState,
    layout: &LayoutManager,
    space: SpaceId,
) -> Vec<(SpaceId, WindowId, objc2_core_foundation::CGRect)> {
    layout
        .layout_engine
        .windows_in_active_workspace(&state.windows, space)
        .into_iter()
        .filter(|window| layout.layout_engine.is_window_floating(*window))
        .filter_map(|window| {
            state.windows.window(window).map(|state| (space, window, state.frame_monotonic))
        })
        .collect()
}

fn store_current_floating_positions(state: &RiniState, layout: &mut LayoutManager, space: SpaceId) {
    let positions = current_floating_positions(state, layout, space)
        .into_iter()
        .map(|(_, window, frame)| (window, frame))
        .collect::<Vec<_>>();
    if !positions.is_empty() {
        layout.layout_engine.store_floating_window_positions(space, &positions);
    }
}

pub fn handle_command_metrics(cmd: MetricsCommand) -> anyhow::Result<EventOutcome> {
    handle_metrics_command(cmd);
    Ok(EventOutcome::no_change())
}

pub fn handle_switch_native_space(
    direction: crate::workspaces::Direction,
) -> anyhow::Result<EventOutcome> {
    Ok(EventOutcome::no_change().with_native_space_switch(direction))
}


pub fn handle_close_window(
    window_server_id: Option<WindowServerId>,
) -> anyhow::Result<EventOutcome> {
    Ok(EventOutcome::no_change().with_close_window(window_server_id))
}

pub fn handle_config_updated(
    config: &mut Config,
    layout: &mut LayoutManager,
    drag: &mut DragManager,
    new_config: Config,
) -> anyhow::Result<EventOutcome> {
    *config = new_config;
    layout.layout_engine.set_layout_settings(&config.settings.layout);

    layout
        .layout_engine
        .update_virtual_workspace_settings(&config.virtual_workspaces);

    drag.update_config(config.settings.window_snapping);

    Ok(EventOutcome::layout_changed(false).with_service_config_update(config.clone()))
}

pub fn handle_command_reactor_debug(
    layout: &LayoutManager,
    topology: &ForwardedSpaceState,
) -> anyhow::Result<EventOutcome> {
    for screen in &topology.screens {
        if let Some(space) = screen.space {
            layout.layout_engine.debug_tree_desc(space, "", true);
        }
    }
    Ok(EventOutcome::no_change())
}

pub fn handle_command_reactor_serialize(
    serialized: Result<String, serde_json::Error>,
) -> anyhow::Result<EventOutcome> {
    Ok(EventOutcome::no_change().with_stdout_line(serialized?))
}

pub fn handle_command_reactor_save_and_exit(
    state: &RiniState,
    layout: &mut LayoutManager,
    active_space: Option<SpaceId>,
) -> anyhow::Result<EventOutcome> {
    if let Err(e) = save_layout(state, layout, rini_core::paths::restore_file(), active_space) {
        error!("Could not save the layout file: {e}");
        // A quit request is conditional on a durable canonical save. Keep Rini running when the
        // snapshot cannot be committed so the user can fix the filesystem problem or retry
        // without losing the only complete in-memory layout.
        return Ok(EventOutcome::no_change()
            .with_stdout_line(format!(
            "Could not save the layout file; Rini is still running: {e}"
        )));
    }
    std::process::exit(0);
}

fn save_layout(
    state: &RiniState,
    layout: &mut LayoutManager,
    path: std::path::PathBuf,
    active_space: Option<SpaceId>,
) -> std::io::Result<()> {
    layout.layout_engine.save_current_layout(path, &state.windows, active_space)
}

pub fn handle_command_reactor_save_layout(
    state: &RiniState,
    layout: &mut LayoutManager,
    path: std::path::PathBuf,
    active_space: Option<SpaceId>,
) -> anyhow::Result<EventOutcome> {
    save_layout(state, layout, path.clone(), active_space)?;
    info!(path = %path.display(), "Saved layout");
    Ok(EventOutcome::no_change().with_stdout_line(format!("Saved layout to {}", path.display())))
}

#[derive(Debug, Clone)]
pub struct ToggleSpacePayload {
    pub config: SpaceActivationConfig,
    pub space: Option<SpaceId>,
    pub display_uuid: Option<String>,
}

pub fn handle_command_reactor_toggle_space_activated(
    policy: &mut SpaceActivationPolicy,
    payload: ToggleSpacePayload,
) -> anyhow::Result<EventOutcome> {
    let Some(space) = payload.space else {
        return Ok(EventOutcome::no_change());
    };
    policy.toggle_space_activated(
        payload.config,
        ToggleSpaceContext {
            space,
            display_uuid: payload.display_uuid,
        },
    );
    Ok(EventOutcome::layout_changed(false).with_active_space_recompute())
}

#[derive(Debug, Clone, Copy)]
pub struct FocusWindowPayload {
    pub window_id: WindowId,
    pub window_server_id: Option<WindowServerId>,
    pub resolved_space: Option<SpaceId>,
    pub space_is_active: bool,
}

#[derive(Debug, Clone)]
pub struct DisplayFocusPayload {
    pub screen: Option<ScreenInfo>,
    pub target_is_active: bool,
    pub focus_window: Option<WindowId>,
}

pub fn handle_move_mouse_to_display(payload: DisplayFocusPayload) -> anyhow::Result<EventOutcome> {
    let Some(screen) = payload.screen else {
        return Ok(EventOutcome::no_change());
    };
    if !payload.target_is_active {
        warn!(?screen.space, "Move mouse ignored: target display space is inactive");
        return Ok(EventOutcome::no_change());
    }
    let mut outcome = EventOutcome::focus_changed(None, false).with_mouse_warp(screen.frame.mid());
    if let (Some(space), Some(window)) = (screen.space, payload.focus_window) {
        outcome = outcome.with_layout_event(LayoutEvent::WindowFocused(space, window));
    }
    Ok(outcome)
}

pub fn handle_focus_display(payload: DisplayFocusPayload) -> anyhow::Result<EventOutcome> {
    let Some(screen) = payload.screen else {
        return Ok(EventOutcome::no_change());
    };
    if !payload.target_is_active {
        warn!(?screen.space, "Focus display ignored: target display space is inactive");
        return Ok(EventOutcome::no_change());
    }
    if let (Some(space), Some(window)) = (screen.space, payload.focus_window) {
        return Ok(EventOutcome::focus_changed(None, false)
            .with_layout_event(LayoutEvent::WindowFocused(space, window)));
    }
    Ok(EventOutcome::focus_changed(None, false).with_mouse_warp(screen.frame.mid()))
}

pub fn handle_command_reactor_focus_window(
    state: &RiniState,
    apps: &AppManager,
    payload: FocusWindowPayload,
) -> anyhow::Result<EventOutcome> {
    let FocusWindowPayload {
        window_id,
        window_server_id,
        resolved_space,
        space_is_active,
    } = payload;
    let mut outcome = EventOutcome::focus_changed(None, false);
    if state.windows.window(window_id).is_some() {
        let Some(space) = resolved_space else {
            warn!(?window_id, "Focus window ignored: space unknown");
            return Ok(outcome);
        };
        if !space_is_active {
            warn!(?window_id, ?space, "Focus window ignored: space is inactive");
            return Ok(outcome);
        }
        outcome = outcome.with_layout_event(LayoutEvent::WindowFocused(space, window_id));

        let mut app_handles: HashMap<i32, AppThreadHandle> = HashMap::default();
        if let Some(app) = apps.apps.get(&window_id.pid) {
            app_handles.insert(window_id.pid, app.handle.clone());
        }
        let request = raise_manager::Event::RaiseRequest(raise_manager::RaiseRequest {
            raise_windows: Vec::new(),
            focus_window: Some((window_id, None)),
            app_handles,
            focus_quiet: Quiet::No,
        });
        outcome = outcome.with_raise_request(request);
    } else if let Some(wsid) = window_server_id {
        outcome = outcome.with_make_key_window(window_id.pid, wsid);
    }
    Ok(outcome)
}

#[derive(Debug, Clone, Copy)]
pub struct MoveWindowToDisplayPayload {
    pub window: WindowId,
    pub window_server_id: Option<WindowServerId>,
    pub source_space: SpaceId,
    pub target_space: SpaceId,
    pub target_screen: objc2_core_foundation::CGRect,
    pub target_frame: objc2_core_foundation::CGRect,
}

pub fn handle_command_reactor_move_window_to_display(
    state: &mut RiniState,
    layout: &mut LayoutManager,
    payload: MoveWindowToDisplayPayload,
) -> anyhow::Result<EventOutcome> {
    if let Some(window) = state.windows.window_mut(payload.window) {
        window.frame_monotonic = payload.target_frame;
    } else {
        warn!(window = ?payload.window, "Move window to display ignored: unknown window");
        return Ok(EventOutcome::no_change());
    }

    let response = layout.layout_engine.move_window_to_space(
        &mut state.windows,
        payload.source_space,
        payload.target_space,
        payload.target_screen.size,
        payload.window,
    );

    // The user asked for this display, so it becomes the window's home. A later unplug
    // will evacuate the window elsewhere, and this is the record that brings it back.
    layout
        .layout_engine
        .set_window_display_home(payload.window, payload.target_space);

    if state
        .windows
        .workspace_for_window(payload.target_space, payload.window)
        .is_some()
        && let Some(window_server_id) = payload.window_server_id
    {
        state
            .windows
            .set_window_server_space(window_server_id, Some(payload.target_space));
        state.windows.mark_window_visible(window_server_id);
    }

    Ok(EventOutcome::layout_changed(false)
        .with_layout_response(response, None)
        .with_pre_layout_window_frame_write(payload.window, payload.target_frame, true))
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};

    use crate::app::reactor::managers::AppManager;
    use crate::app::reactor::state::RiniState;
    use crate::app::reactor::testing::make_window_info;
    use crate::windows::domain::state::WindowState;
    use rini_core::ids::ScreenId;

    use super::*;

    fn wid() -> WindowId {
        WindowId::new(1, 1)
    }

    fn screen(space: Option<SpaceId>) -> ScreenInfo {
        ScreenInfo {
            id: ScreenId::new(1),
            frame: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 800.0)),
            display_uuid: "uuid-1".into(),
            name: None,
            space,
        }
    }

    fn display_payload(space: Option<SpaceId>, active: bool, focus: Option<WindowId>) -> DisplayFocusPayload {
        DisplayFocusPayload { screen: Some(screen(space)), target_is_active: active, focus_window: focus }
    }

    fn focused_window(outcome: &EventOutcome) -> Option<(SpaceId, WindowId)> {
        outcome.layout_events.iter().find_map(|event| match event {
            LayoutEvent::WindowFocused(space, window) => Some((*space, *window)),
            _ => None,
        })
    }

    #[test]
    fn a_selector_naming_no_display_does_nothing() {
        let none = DisplayFocusPayload { screen: None, target_is_active: true, focus_window: None };
        for outcome in [
            handle_move_mouse_to_display(none.clone()).unwrap(),
            handle_focus_display(none).unwrap(),
        ] {
            assert!(outcome.layout_events.is_empty());
            assert_eq!(outcome.mouse_warps, Vec::new());
        }
    }

    // A display whose space is not active is one rini is not managing right now. Warping onto it
    // would put the cursor somewhere no layout answers for.
    #[test]
    fn an_inactive_target_display_is_refused_by_both_commands() {
        let payload = display_payload(Some(SpaceId::new(2)), false, Some(wid()));
        for outcome in [
            handle_move_mouse_to_display(payload.clone()).unwrap(),
            handle_focus_display(payload).unwrap(),
        ] {
            assert_eq!(outcome.mouse_warps, Vec::new());
            assert_eq!(focused_window(&outcome), None);
        }
    }

    // The two commands differ only here: moving the mouse always warps, and focusing warps only when
    // there is no window to focus instead.
    #[test]
    fn moving_the_mouse_warps_and_focusing_prefers_a_window() {
        let payload = display_payload(Some(SpaceId::new(2)), true, Some(wid()));

        let moved = handle_move_mouse_to_display(payload.clone()).unwrap();
        assert_eq!(moved.mouse_warps, vec![screen(None).frame.mid()]);
        assert_eq!(focused_window(&moved), Some((SpaceId::new(2), wid())));

        let focused = handle_focus_display(payload).unwrap();
        assert_eq!(focused.mouse_warps, Vec::new(), "the window takes focus, the cursor stays put");
        assert_eq!(focused_window(&focused), Some((SpaceId::new(2), wid())));
    }

    #[test]
    fn focusing_a_display_with_no_window_on_it_warps_the_cursor_instead() {
        let outcome = handle_focus_display(display_payload(Some(SpaceId::new(2)), true, None)).unwrap();
        assert_eq!(outcome.mouse_warps, vec![screen(None).frame.mid()]);
        assert_eq!(focused_window(&outcome), None);
    }

    fn state_with_window() -> RiniState {
        let mut state = RiniState::default();
        let info = make_window_info(screen(None).frame, Some(WindowServerId::new(7)), "w", None);
        state.windows.insert_window(wid(), WindowState::from(info));
        state
    }

    #[test]
    fn focusing_a_tracked_window_on_an_active_space_raises_it() {
        let outcome = handle_command_reactor_focus_window(
            &state_with_window(),
            &AppManager::new(),
            FocusWindowPayload {
                window_id: wid(),
                window_server_id: None,
                resolved_space: Some(SpaceId::new(2)),
                space_is_active: true,
            },
        )
        .unwrap();
        assert_eq!(focused_window(&outcome), Some((SpaceId::new(2), wid())));
        assert!(!outcome.raise_requests.is_empty());
    }

    #[test]
    fn focusing_a_tracked_window_is_refused_without_an_active_space() {
        for (space, active) in [(None, true), (Some(SpaceId::new(2)), false)] {
            let outcome = handle_command_reactor_focus_window(
                &state_with_window(),
                &AppManager::new(),
                FocusWindowPayload {
                    window_id: wid(),
                    window_server_id: None,
                    resolved_space: space,
                    space_is_active: active,
                },
            )
            .unwrap();
            assert_eq!(focused_window(&outcome), None, "space {space:?} active {active}");
            assert!(outcome.raise_requests.is_empty());
        }
    }

    // A window rini does not track cannot be raised through the layout, but the window server can
    // still be told to make it key. That is how focusing something unmanaged works at all.
    #[test]
    fn an_untracked_window_is_made_key_through_the_window_server() {
        let outcome = handle_command_reactor_focus_window(
            &RiniState::default(),
            &AppManager::new(),
            FocusWindowPayload {
                window_id: wid(),
                window_server_id: Some(WindowServerId::new(7)),
                resolved_space: None,
                space_is_active: false,
            },
        )
        .unwrap();
        assert_eq!(focused_window(&outcome), None);
        assert_eq!(outcome.make_key_windows, vec![(1, WindowServerId::new(7))]);
    }

    #[test]
    fn an_untracked_window_with_no_server_id_cannot_be_focused_at_all() {
        let outcome = handle_command_reactor_focus_window(
            &RiniState::default(),
            &AppManager::new(),
            FocusWindowPayload {
                window_id: wid(),
                window_server_id: None,
                resolved_space: None,
                space_is_active: false,
            },
        )
        .unwrap();
        assert_eq!(outcome.make_key_windows, Vec::new());
        assert_eq!(focused_window(&outcome), None);
    }
}
