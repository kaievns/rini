use objc2_core_foundation::CGPoint;
use tracing::{trace, warn};

use crate::actor::app::WindowId;
use crate::actor::reactor::events::EventOutcome;
use crate::actor::reactor::managers::{DragManager, LayoutManager};
use crate::actor::reactor::{DragState, LayoutEvent};
use crate::common::collections::HashMap;
use crate::layout_engine::LayoutCommand;
use crate::model::RiftState;
use crate::sys::screen::SpaceId;

#[derive(Debug, Clone)]
pub struct MouseUpPayload {
    pub pending_swap: Option<(WindowId, WindowId)>,
    pub swap_space: Option<SpaceId>,
    pub final_space: Option<SpaceId>,
    pub visible_spaces: Vec<SpaceId>,
    pub visible_space_centers: HashMap<SpaceId, CGPoint>,
}

pub fn handle_mouse_up(
    state: &mut RiftState,
    layout: &mut LayoutManager,
    drag: &mut DragManager,
    payload: MouseUpPayload,
) -> anyhow::Result<EventOutcome> {
    let mut outcome = EventOutcome::layout_changed(false);
    let mut needs_layout = false;

    if let Some((dragged, target)) = payload.pending_swap {
        trace!(?dragged, ?target, "performing deferred drag swap");
        drag.skip_layout_for_window = Some(dragged);
        if state.windows.contains_window(dragged) && state.windows.contains_window(target) {
            let response = layout.layout_engine.handle_command(
                &mut state.windows,
                payload.swap_space,
                &payload.visible_spaces,
                &payload.visible_space_centers,
                LayoutCommand::SwapWindows(dragged.into(), target.into()),
            );
            outcome = outcome.with_layout_response(response, None);
        }
        needs_layout = true;
    }

    let session = match std::mem::replace(&mut drag.drag_state, DragState::Inactive) {
        DragState::Active { session } | DragState::PendingSwap { session, .. } => Some(session),
        DragState::Inactive => None,
    };
    if let Some(session) = session {
        let window = session.window;
        if session.origin_space != payload.final_space {
            if session.origin_space.is_some() {
                outcome = outcome.with_layout_event(LayoutEvent::WindowRemoved(window));
            }
            if let Some(space) = payload.final_space {
                if let Some(server_id) =
                    state.windows.window(window).and_then(|window| window.info.sys_id)
                {
                    state.windows.set_window_server_space(server_id, Some(space));
                    state.windows.mark_window_visible(server_id);
                }
                // Keep the window in ITS OWN workspace, changing only which display's strip
                // holds it.
                //
                // This used to assign it to the destination display's ACTIVE workspace. If
                // that display was showing something else, the window silently changed
                // workspace and vanished from view — reported when tearing a Chrome tab into
                // its own window, and when dragging a window across displays. A drag moves a
                // window between strips; only an explicit move-to-workspace command may move
                // it between workspaces.
                let workspace = layout
                    .layout_engine
                    .virtual_workspace_manager()
                    .workspace_info_for_window_any(&state.windows, window)
                    .map(|assignment| assignment.workspace_id)
                    .or_else(|| layout.layout_engine.active_workspace(space));
                if let Some(workspace) = workspace
                    && !layout
                        .layout_engine
                        .virtual_workspace_manager_mut()
                        .assign_window_to_workspace(&mut state.windows, space, window, workspace)
                {
                    warn!(?window, ?workspace, "failed to assign dragged window");
                }
                // Dragging a window to another display is the user choosing where it
                // lives, so it becomes the window's home. Without this a later replug
                // would repatriate it back to the display it was dragged off.
                layout.layout_engine.set_window_display_home(window, space);
                outcome = outcome.with_layout_event(LayoutEvent::WindowAdded(space, window));
            }
            drag.skip_layout_for_window = Some(window);
            needs_layout = true;
        } else if session.layout_dirty {
            drag.skip_layout_for_window = Some(window);
            needs_layout = true;
        }

        if let Some(space) = payload.final_space
            && layout.layout_engine.is_window_floating(window)
        {
            if session.origin_space != payload.final_space {
                layout.layout_engine.remove_floating_position(window);
            }
            if let Some(workspace) = layout
                .layout_engine
                .virtual_workspace_manager()
                .workspace_for_window(&state.windows, space, window)
                .or_else(|| layout.layout_engine.active_workspace(space))
            {
                layout.layout_engine.store_floating_position(
                    space,
                    workspace,
                    window,
                    session.last_frame,
                );
            }
        }
    }

    drag.reset();
    drag.drag_state = DragState::Inactive;
    let skipped = drag.skip_layout_for_window.is_some();
    drag.skip_layout_for_window = None;

    let passes = if needs_layout {
        if skipped { 3 } else { 2 }
    } else {
        0
    };
    Ok(outcome.with_arrange_passes(passes))
}
