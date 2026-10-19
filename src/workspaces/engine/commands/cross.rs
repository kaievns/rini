//! A press up or down that goes on through the workspace stack where its column ends: the room its
//! column leaves it, which decides the command it is.

use rini_core::ids::SpaceId;

use crate::workspaces::domain::command::{ColumnRoom, CrossCommand};
use crate::workspaces::engine::{LayoutEngine, WindowStore};

impl LayoutEngine {
    /// Whether the window selected on `space`'s strip has a window the way `press` goes in its
    /// column. A focused floating window is in no column: its focus press stays within, which takes
    /// focus back to the strip, and its move goes on to the workspace, so the floating window moves
    /// and not the strip's selection. No space, no layout and no selection leave nothing to step to.
    pub(in crate::workspaces::engine) fn column_room(
        &self,
        window_store: &WindowStore,
        space: Option<SpaceId>,
        press: CrossCommand,
    ) -> ColumnRoom {
        if self.focused_window.is_some_and(|focus| self.floating.is_floating(focus)) {
            return match press {
                CrossCommand::Focus(_) => ColumnRoom::Within,
                CrossCommand::Move(_) => ColumnRoom::Ended,
            };
        }
        let onward = space.and_then(|space| {
            let (workspace, layout) = self.workspace_and_layout(space)?;
            let window = self
                .workspace_tree(workspace)
                .window_in_direction(layout, press.way().direction());
            self.filter_active_workspace_window(window_store, space, window)
        });
        match onward {
            Some(_) => ColumnRoom::Within,
            None => ColumnRoom::Ended,
        }
    }
}
