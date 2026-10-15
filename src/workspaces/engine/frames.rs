//! The frames a workspace's strip lays out, with its floating windows: spent by the reactor's
//! passes, previewed by a query.

use objc2_core_foundation::CGRect;
use rini_core::ids::{SpaceId, WindowId};
use rustc_hash::FxHashMap as HashMap;

use crate::layout::settings::Gaps;
use crate::workspaces::WindowStore;
use crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId;
use crate::workspaces::engine::{LayoutEngine, LayoutId};

impl LayoutEngine {
    /// Every frame `workspace_id` shows on `screen`, its strip spending what it held for the pass.
    pub fn calculate_layout_for_workspace(
        &mut self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        screen: CGRect,
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let tiled = match self.workspace_layouts.active(space, workspace_id) {
            Some(layout) => self.lay_out_strip(workspace_id, layout, screen, gaps),
            None => Vec::new(),
        };
        self.with_floating_positions(window_store, space, workspace_id, tiled)
    }

    /// What `calculate_layout_for_workspace` answers, leaving every strip as it is: for a query,
    /// which only reads.
    pub fn preview_layout_for_workspace(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        screen: CGRect,
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let tiled = match self.workspace_layouts.active(space, workspace_id) {
            Some(layout) => self.workspace_tree(workspace_id).preview_layout(
                layout,
                screen,
                &self.window_layout_constraints,
                &self.borders,
                gaps,
            ),
            None => Vec::new(),
        };
        self.with_floating_positions(window_store, space, workspace_id, tiled)
    }

    /// `tiled` with the floating windows of `workspace_id` at their stored positions.
    fn with_floating_positions(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        tiled: Vec<(WindowId, CGRect)>,
    ) -> Vec<(WindowId, CGRect)> {
        let mut positions: HashMap<WindowId, CGRect> = tiled.into_iter().collect();
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

    /// `ws_id`'s strip `layout` laid out on `screen`, spending what it held for the pass.
    pub(in crate::workspaces::engine) fn lay_out_strip(
        &mut self,
        ws_id: VirtualWorkspaceId,
        layout: LayoutId,
        screen: CGRect,
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let constraints = &self.window_layout_constraints;
        let system = &mut self.virtual_workspace_manager.workspaces[ws_id].layout_system;
        system.calculate_layout(layout, screen, constraints, &self.borders, gaps)
    }
}
