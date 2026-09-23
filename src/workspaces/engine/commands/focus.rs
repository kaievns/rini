//! Moving focus around the strip, and across to the next display when it runs out.
//!
//! One arm per command, lifted out of `handle_command` unchanged. `impl LayoutEngine` across modules
//! is the pattern `engine/persistence/` already uses.

use rustc_hash::FxHashMap as HashMap;

use objc2_core_foundation::CGPoint;
use rini_core::ids::SpaceId;

use super::CommandTarget;
use crate::workspaces::domain::workspace_focus;
use crate::workspaces::engine::{EventResponse, LayoutCommand, LayoutEngine, WindowStore};
use tracing::debug;

impl LayoutEngine {
    pub(in crate::workspaces::engine) fn handle_focus_command(
        &mut self,
        window_store: &mut WindowStore,
        visible_spaces: &[SpaceId],
        visible_space_centers: &HashMap<SpaceId, CGPoint>,
        target: CommandTarget,
        is_floating: bool,
        command: LayoutCommand,
    ) -> EventResponse {
        let CommandTarget {
            space,
            workspace: workspace_id,
            layout,
        } = target;
        match command {
            LayoutCommand::NextWindow | LayoutCommand::PrevWindow => {
                let forward = matches!(command, LayoutCommand::NextWindow);
                let windows = if is_floating {
                    self.active_floating_windows_in_workspace(window_store, space)
                } else {
                    self.filter_active_workspace_windows(
                        window_store,
                        space,
                        self.workspace_tree(workspace_id).visible_windows_in_layout(layout),
                    )
                };
                let step =
                    windows.iter().position(|&w| Some(w) == self.focused_window).and_then(|idx| {
                        workspace_focus::cycle_step(
                            idx,
                            windows.len(),
                            if forward {
                                workspace_focus::Cycle::Forward
                            } else {
                                workspace_focus::Cycle::Backward
                            },
                        )
                    });
                if let Some(next) = step {
                    let response = EventResponse {
                        changed: true,
                        focus_window: Some(windows[next]),
                        raise_windows: vec![windows[next]],
                        boundary_hit: None,
                        edge_hit: None,
                    };
                    self.apply_focus_response(window_store, space, workspace_id, layout, &response);
                    return response;
                } else {
                    let focus_window = self
                        .workspace_tree(workspace_id)
                        .selected_window(layout)
                        .filter(|wid| windows.contains(wid))
                        .or_else(|| windows.first().copied());
                    let raise_windows = focus_window.into_iter().collect();
                    let response = EventResponse {
                        changed: true,
                        focus_window,
                        raise_windows,
                        boundary_hit: None,
                        edge_hit: None,
                    };
                    self.apply_focus_response(window_store, space, workspace_id, layout, &response);
                    return response;
                }
            }
            LayoutCommand::MoveFocus(direction) => {
                debug!(
                    "MoveFocus command received, direction: {:?}, is_floating: {}",
                    direction, is_floating
                );
                return self.move_focus_internal(
                    window_store,
                    space,
                    visible_spaces,
                    visible_space_centers,
                    direction,
                    is_floating,
                );
            }
            other => {
                debug_assert!(false, "{other:?} is not a focus command");
                EventResponse::default()
            }
        }
    }
}
