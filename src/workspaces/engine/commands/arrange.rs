//! Moving windows within the strip: swaps, node moves, folding and stacking.
//!
//! One arm per command, lifted out of `handle_command` unchanged. `impl LayoutEngine` across modules
//! is the pattern `engine/persistence/` already uses.

use rustc_hash::FxHashMap as HashMap;

use objc2_core_foundation::CGPoint;
use rini_core::ids::SpaceId;

use super::CommandTarget;
use crate::workspaces::engine::{EventResponse, LayoutCommand, LayoutEngine, WindowStore};
use tracing::debug;

impl LayoutEngine {
    pub(in crate::workspaces::engine) fn handle_arrange_command(
        &mut self,
        window_store: &mut WindowStore,
        visible_spaces: &[SpaceId],
        visible_space_centers: &HashMap<SpaceId, CGPoint>,
        target: CommandTarget,
        command: LayoutCommand,
    ) -> EventResponse {
        let CommandTarget {
            space,
            workspace: workspace_id,
            layout,
        } = target;
        match command {
            LayoutCommand::SwapWindows(a, b) => {
                let a = rini_core::ids::WindowId::new(a.pid, a.idx);
                let b = rini_core::ids::WindowId::new(b.pid, b.idx);
                let _ = self.workspace_tree_mut(workspace_id).swap_windows(layout, a, b);

                EventResponse::default()
            }
            LayoutCommand::MoveNode(direction) => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                if !self.workspace_tree_mut(workspace_id).move_selection(layout, direction) {
                    if let Some(new_space) = self.next_space_for_direction(
                        space,
                        direction,
                        visible_spaces,
                        visible_space_centers,
                    ) {
                        let Some((new_ws_id, new_layout)) = self.workspace_and_layout(new_space)
                        else {
                            debug!(
                                "No active workspace/layout for adjacent space {:?}; skipping cross-space move",
                                new_space
                            );
                            return EventResponse::default();
                        };
                        let windows = self
                            .workspace_tree(workspace_id)
                            .visible_windows_under_selection(layout);
                        for wid in windows {
                            self.workspace_tree_mut(workspace_id).remove_window(wid);
                            self.workspace_tree_mut(new_ws_id)
                                .add_window_after_selection(new_layout, wid);
                            self.virtual_workspace_manager.assign_window_to_workspace(
                                window_store,
                                new_space,
                                wid,
                                new_ws_id,
                            );
                        }
                    }
                }
                EventResponse::default()
            }
            LayoutCommand::ToggleFold(side) => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                let raise_windows =
                    self.workspace_tree_mut(workspace_id).toggle_fold_of_selection(layout, side);
                Self::response_for_raised_windows(raise_windows)
            }
            LayoutCommand::ToggleStack => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                self.toggle_stack_for_workspace(workspace_id, layout)
            }
            other => {
                debug_assert!(false, "{other:?} is not a arrange command");
                EventResponse::default()
            }
        }
    }
}
