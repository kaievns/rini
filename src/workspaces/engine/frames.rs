//! The frames a workspace shows: its strip and its floating windows.

use objc2_core_foundation::CGRect;
use rini_core::ids::{SpaceId, WindowId};
use rustc_hash::FxHashMap as HashMap;

use crate::layout::settings::Gaps;
use crate::workspaces::WindowStore;
use crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId;
use crate::workspaces::engine::LayoutEngine;

impl LayoutEngine {
    pub fn calculate_layout_for_workspace(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        screen: CGRect,
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let mut positions = HashMap::default();

        if let Some(layout) = self.workspace_layouts.active(space, workspace_id) {
            let tiled_positions = self.workspace_tree(workspace_id).calculate_layout(
                layout,
                screen,
                &self.window_layout_constraints,
                &self.borders,
                gaps,
            );
            for (wid, rect) in tiled_positions {
                positions.insert(wid, rect);
            }
        }

        let floating_positions = self.floating_positions.workspace_positions(space, workspace_id);
        for (window_id, stored_position) in floating_positions {
            if self.floating.is_floating(window_id)
                && self.virtual_workspace_manager.workspace_for_window(
                    window_store,
                    space,
                    window_id,
                ) == Some(workspace_id)
            {
                positions.insert(window_id, stored_position);
            }
        }

        positions.into_iter().collect()
    }
}
