//! Changing a column's width or a window's height.
//!
//! One arm per command, lifted out of `handle_command` unchanged. `impl LayoutEngine` across modules
//! is the pattern `engine/persistence/` already uses.

use super::CommandTarget;
use crate::layout::ResizeOrientation;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::engine::{EventResponse, LayoutCommand, LayoutEngine};

impl LayoutEngine {
    pub(in crate::workspaces::engine) fn handle_resize_command(
        &mut self,
        memory: &mut DisplayMemory,
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
            LayoutCommand::ResizeWindowGrow(orientation) => {
                if is_floating {
                    return EventResponse::default();
                }

                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                let resize_amount = 0.05;
                self.workspace_tree_mut(workspace_id).resize_selection_by(
                    layout,
                    resize_amount,
                    orientation,
                );
                self.remember_selected_column_width(memory, space, workspace_id, layout);
                EventResponse::default()
            }
            LayoutCommand::ResizeWindowShrink(orientation) => {
                if is_floating {
                    return EventResponse::default();
                }

                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                let resize_amount = -0.05;
                self.workspace_tree_mut(workspace_id).resize_selection_by(
                    layout,
                    resize_amount,
                    orientation,
                );
                self.remember_selected_column_width(memory, space, workspace_id, layout);
                EventResponse::default()
            }
            LayoutCommand::ResizeWindowBy { amount } => {
                if is_floating {
                    return EventResponse::default();
                }

                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                self.workspace_tree_mut(workspace_id).resize_selection_by(
                    layout,
                    amount,
                    ResizeOrientation::Horizontal,
                );
                self.remember_selected_column_width(memory, space, workspace_id, layout);
                EventResponse::default()
            }
            LayoutCommand::CyclePresetColumnWidth => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                let raised =
                    self.workspace_tree_mut(workspace_id).cycle_preset_column_width(layout);
                for window in &raised {
                    self.remember_column_width(memory, space, workspace_id, layout, *window);
                }
                Self::response_for_raised_windows(raised)
            }
            other => {
                debug_assert!(false, "{other:?} is not a resize command");
                EventResponse::default()
            }
        }
    }
}
