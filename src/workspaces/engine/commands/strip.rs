//! Moving the strip itself under the viewport.
//!
//! One arm per command, lifted out of `handle_command` unchanged. `impl LayoutEngine` across modules
//! is the pattern `engine/persistence/` already uses.

use super::CommandTarget;
use crate::workspaces::domain::command::StripCommand;
use crate::workspaces::engine::{EventResponse, LayoutEngine};

impl LayoutEngine {
    pub(in crate::workspaces::engine) fn handle_strip_command(
        &mut self,
        target: CommandTarget,
        command: StripCommand,
    ) -> EventResponse {
        let CommandTarget {
            workspace: workspace_id,
            layout,
            ..
        } = target;
        match command {
            StripCommand::Snap => {
                let system = self.workspace_tree_mut(workspace_id);
                system.snap_to_nearest_column(layout);
                EventResponse::default()
            }
            StripCommand::CenterSelection => {
                let system = self.workspace_tree_mut(workspace_id);
                system.center_selected_column(layout);
                EventResponse::default()
            }
        }
    }
}
