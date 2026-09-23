//! Moving the strip itself under the viewport.
//!
//! One arm per command, lifted out of `handle_command` unchanged. `impl LayoutEngine` across modules
//! is the pattern `engine/persistence/` already uses.

use super::CommandTarget;
use crate::workspaces::engine::{EventResponse, LayoutCommand, LayoutEngine};

impl LayoutEngine {
    pub(in crate::workspaces::engine) fn handle_strip_command(
        &mut self,
        target: CommandTarget,
        command: LayoutCommand,
    ) -> EventResponse {
        let CommandTarget {
            workspace: workspace_id,
            layout,
            ..
        } = target;
        match command {
            LayoutCommand::ScrollStrip { delta } => {
                let mut resp = EventResponse::default();
                let system = self.workspace_tree_mut(workspace_id);
                resp.boundary_hit = system.scroll_by_delta(layout, delta);
                resp
            }
            LayoutCommand::SnapStrip => {
                let system = self.workspace_tree_mut(workspace_id);
                system.snap_to_nearest_column(layout);
                EventResponse::default()
            }
            LayoutCommand::CenterSelection => {
                let system = self.workspace_tree_mut(workspace_id);
                system.center_selected_column(layout);
                EventResponse::default()
            }
            other => {
                debug_assert!(false, "{other:?} is not a strip command");
                EventResponse::default()
            }
        }
    }
}
