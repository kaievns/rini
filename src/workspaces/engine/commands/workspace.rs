//! Which workspace a display shows, and moving a window to another one.
//!
//! `impl LayoutEngine` across modules is the pattern `engine/persistence/` already uses.

use rini_core::ids::SpaceId;

use crate::workspaces::Direction;
use crate::workspaces::domain::command::WorkspaceCommand;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::engine::{EventResponse, LayoutEngine, WindowStore};

impl LayoutEngine {
    pub(in crate::workspaces::engine) fn handle_workspace_command(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        space: SpaceId,
        command: WorkspaceCommand,
    ) -> EventResponse {
        match command {
            // Workspaces stack downward: the next one is below, the previous above. No step
            // (the end of the stack, or nothing left in that direction to skip to)
            // reports the edge instead of silence.
            WorkspaceCommand::Next(skip_empty) => {
                if let Some(current_workspace) =
                    self.virtual_workspace_manager.active_workspace(space)
                    && let Some(next_workspace) = self.virtual_workspace_manager.next_workspace(
                        window_store,
                        space,
                        current_workspace,
                        skip_empty,
                    )
                {
                    return self.activate_workspace(window_store, space, next_workspace, None);
                }
                EventResponse {
                    edge_hit: Some(Direction::Down),
                    ..EventResponse::default()
                }
            }
            WorkspaceCommand::Prev(skip_empty) => {
                if let Some(current_workspace) =
                    self.virtual_workspace_manager.active_workspace(space)
                    && let Some(prev_workspace) = self.virtual_workspace_manager.prev_workspace(
                        window_store,
                        space,
                        current_workspace,
                        skip_empty,
                    )
                {
                    return self.activate_workspace(window_store, space, prev_workspace, None);
                }
                EventResponse {
                    edge_hit: Some(Direction::Up),
                    ..EventResponse::default()
                }
            }
            WorkspaceCommand::SwitchTo(workspace_index) => {
                self.switch_to_workspace(window_store, space, workspace_index, None)
            }
            WorkspaceCommand::MoveWindow { workspace, follow, window_id } => self
                .move_window_to_workspace(
                    window_store,
                    memory,
                    space,
                    &workspace,
                    follow,
                    &window_id,
                ),
            WorkspaceCommand::SwitchToLast => {
                if let Some(last_workspace) = self.virtual_workspace_manager.last_workspace(space) {
                    return self.activate_workspace(window_store, space, last_workspace, None);
                }
                EventResponse::default()
            }
        }
    }
}
