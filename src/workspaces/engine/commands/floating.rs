//! A window maximised while off the strip.
//!
//! One arm per command, lifted out of `handle_command` unchanged. `impl LayoutEngine` across modules
//! is the pattern `engine/persistence/` already uses.

use super::CommandTarget;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::engine::{EventResponse, LayoutCommand, LayoutEngine};

impl LayoutEngine {
    pub(in crate::workspaces::engine) fn handle_floating_command(
        &mut self,
        memory: &mut DisplayMemory,
        target: CommandTarget,
        command: LayoutCommand,
    ) -> EventResponse {
        let CommandTarget {
            space,
            workspace: workspace_id,
            layout,
        } = target;
        match command {
            LayoutCommand::ToggleFullscreenWithinGaps => {
                let raise_windows = self
                    .workspace_tree_mut(workspace_id)
                    .toggle_fullscreen_within_gaps_of_selection(layout);
                for window in &raise_windows {
                    self.remember_column_width(memory, space, workspace_id, layout, *window);
                }
                if raise_windows.is_empty() {
                    EventResponse::default()
                } else {
                    EventResponse {
                        changed: true,
                        raise_windows,
                        focus_window: None,
                        boundary_hit: None,
                        edge_hit: None,
                    }
                }
            }
            other => {
                debug_assert!(false, "{other:?} is not a floating command");
                EventResponse::default()
            }
        }
    }
}
