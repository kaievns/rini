//! Moving focus around the strip, and across to the next display when it runs out.
//!
//! One arm per command. `impl LayoutEngine` across modules is the pattern `engine/persistence/`
//! already uses.

use objc2_core_foundation::CGPoint;
use rini_core::ids::SpaceId;
use rustc_hash::FxHashMap as HashMap;
use tracing::debug;

use super::CommandTarget;
use crate::workspaces::domain::command::FocusCommand;
use crate::workspaces::domain::workspace_focus;
use crate::workspaces::engine::navigation::FocusStep;
use crate::workspaces::engine::{EventResponse, LayoutEngine, WindowStore};

impl LayoutEngine {
    pub(in crate::workspaces::engine) fn handle_focus_command(
        &mut self,
        window_store: &mut WindowStore,
        visible_spaces: &[SpaceId],
        visible_space_centers: &HashMap<SpaceId, CGPoint>,
        target: CommandTarget,
        is_floating: bool,
        command: FocusCommand,
    ) -> EventResponse {
        let CommandTarget {
            space,
            workspace: workspace_id,
            layout,
        } = target;
        match command {
            FocusCommand::Cycle(cycle) => {
                let windows = if is_floating {
                    self.active_floating_windows_in_workspace(window_store, space)
                } else {
                    self.filter_active_workspace_windows(
                        window_store,
                        space,
                        self.workspace_tree(workspace_id).visible_windows_in_layout(layout),
                    )
                };
                let step = windows
                    .iter()
                    .position(|&w| Some(w) == self.focused_window)
                    .and_then(|idx| workspace_focus::cycle_step(idx, windows.len(), cycle));
                if let Some(next) = step {
                    let response = EventResponse {
                        changed: true,
                        focus_window: Some(windows[next]),
                        raise_windows: vec![windows[next]],
                        edge_hit: None,
                        moved: None,
                        stepped: None,
                        pushed: None,
                    };
                    self.apply_focus_response(space, workspace_id, layout, &response);
                    response
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
                        edge_hit: None,
                        moved: None,
                        stepped: None,
                        pushed: None,
                    };
                    self.apply_focus_response(space, workspace_id, layout, &response);
                    response
                }
            }
            FocusCommand::Move(direction) => {
                debug!(
                    "MoveFocus command received, direction: {:?}, is_floating: {}",
                    direction, is_floating
                );
                self.move_focus_internal(
                    window_store,
                    space,
                    visible_spaces,
                    visible_space_centers,
                    FocusStep::Toward(direction),
                    is_floating,
                )
            }
            FocusCommand::ToEnd(end) => self.move_focus_internal(
                window_store,
                space,
                visible_spaces,
                visible_space_centers,
                FocusStep::ToEnd(end),
                is_floating,
            ),
        }
    }
}
