//! Moving windows within the strip: swaps, node moves, moves to an end, folding and stacking.
//!
//! One arm per command. `impl LayoutEngine` across modules is the pattern `engine/persistence/`
//! already uses.

use objc2_core_foundation::CGPoint;
use rini_core::ids::SpaceId;
use rustc_hash::FxHashMap as HashMap;
use tracing::debug;

use super::CommandTarget;
use crate::layout::domain::boundary::strip_edge;
use crate::layout::{LayoutId, StripEnd};
use crate::workspaces::domain::command::ArrangeCommand;
use crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId;
use crate::workspaces::engine::{EventResponse, LayoutEngine, StripMove, WindowStore};

impl LayoutEngine {
    pub(in crate::workspaces::engine) fn handle_arrange_command(
        &mut self,
        window_store: &mut WindowStore,
        visible_spaces: &[SpaceId],
        visible_space_centers: &HashMap<SpaceId, CGPoint>,
        target: CommandTarget,
        command: ArrangeCommand,
    ) -> EventResponse {
        let CommandTarget {
            space,
            workspace: workspace_id,
            layout,
        } = target;
        match command {
            ArrangeCommand::Swap(a, b) => {
                let _ = self.workspace_tree_mut(workspace_id).swap_windows(layout, a, b);

                EventResponse::default()
            }
            ArrangeCommand::MoveNode(direction) => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                let tree = self.workspace_tree(workspace_id);
                let swapping =
                    strip_edge(direction).is_some() && !tree.parent_of_selection_is_stacked(layout);
                let selected = tree.selected_window(layout);
                if self.workspace_tree_mut(workspace_id).move_selection(layout, direction) {
                    return EventResponse {
                        moved: selected.filter(|_| swapping).map(|window| StripMove {
                            window,
                            direction,
                            columns_passed: 1,
                        }),
                        ..EventResponse::default()
                    };
                }

                // The strip has no room that way. Whether the window may leave for the next display is
                // the same question focus asks, and gets the same answer from the same place: a window
                // pushed past the last column must not leave for the other monitor where focus in
                // that direction stops.
                let adjacent_space = match strip_edge(direction) {
                    Some(_) => None,
                    None => self.next_space_for_direction(
                        space,
                        direction,
                        visible_spaces,
                        visible_space_centers,
                    ),
                };

                let Some(new_space) = adjacent_space else {
                    // Nowhere to go. The view bounces, so the stop reads as an end of the strip
                    // rather than a dropped keypress.
                    return EventResponse {
                        edge_hit: strip_edge(direction),
                        pushed: selected.filter(|_| swapping),
                        ..EventResponse::default()
                    };
                };

                let Some((new_ws_id, new_layout)) = self.workspace_and_layout(new_space) else {
                    debug!(
                        "No active workspace/layout for adjacent space {:?}; skipping cross-space move",
                        new_space
                    );
                    return EventResponse::default();
                };
                let windows =
                    self.workspace_tree(workspace_id).visible_windows_under_selection(layout);
                for wid in windows {
                    self.workspace_tree_mut(workspace_id).remove_window(wid);
                    self.workspace_tree_mut(new_ws_id).add_window_after_selection(new_layout, wid);
                    self.virtual_workspace_manager.assign_window_to_workspace(
                        window_store,
                        new_space,
                        wid,
                        new_ws_id,
                    );
                }
                EventResponse::default()
            }
            ArrangeCommand::MoveNodeToEnd(end) => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                self.move_node_to_end(workspace_id, layout, end)
            }
            ArrangeCommand::ToggleFold(side) => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                let raise_windows =
                    self.workspace_tree_mut(workspace_id).toggle_fold_of_selection(layout, side);
                Self::response_for_raised_windows(raise_windows)
            }
            ArrangeCommand::ExplodeColumn => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                let raise_windows =
                    self.workspace_tree_mut(workspace_id).explode_column_of_selection(layout);
                Self::response_for_raised_windows(raise_windows)
            }
        }
    }

    /// The selection taken to `end` of the strip, as a move of one place takes it: a lone window's
    /// column named with how many columns it passed, a stacked window leaving as a new column named
    /// with nothing, and a lone window already there pushing against the end, which bounces.
    fn move_node_to_end(
        &mut self,
        workspace_id: VirtualWorkspaceId,
        layout: LayoutId,
        end: StripEnd,
    ) -> EventResponse {
        let direction = end.direction();
        let tree = self.workspace_tree(workspace_id);
        let swapping = !tree.parent_of_selection_is_stacked(layout);
        let columns_passed = tree.columns_toward(layout, end);
        let selected = tree.selected_window(layout);
        if self.workspace_tree_mut(workspace_id).move_selection_to_end(layout, end) {
            return EventResponse {
                moved: selected.filter(|_| swapping).map(|window| StripMove {
                    window,
                    direction,
                    columns_passed,
                }),
                ..EventResponse::default()
            };
        }
        EventResponse {
            edge_hit: strip_edge(direction),
            pushed: selected,
            ..EventResponse::default()
        }
    }
}
