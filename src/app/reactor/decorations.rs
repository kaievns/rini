//! The reactor's half of the decorations: what each managed window wears, and who wears the ring.
//!
//! The ring is decided over rini's own stores (`decorations::domain::style::Rings`): each
//! workspace's active window, the one focus lands on there, on every display. The rings the last
//! batch of events left are kept, so a pass can say whether each window wore the ring before it.

use rini_core::ids::{SpaceId, WindowId};

use crate::decorations::domain::style::{Rings, Worn};
use crate::workspaces::VirtualWorkspaceId;

use super::Reactor;

impl Reactor {
    /// The rings with `focus` holding focus: each workspace's active window on every display rini
    /// manages, `focus` on its own workspace. None while focus rings are off, so nothing is read.
    pub(super) fn rings_with(&self, focus: Option<WindowId>) -> Rings {
        if self.config.settings.decorations.decoration().ring.is_none() {
            return Rings::default();
        }
        let engine = &self.layout_manager.layout_engine;
        let workspaces = engine.virtual_workspace_manager();
        let active: Vec<((SpaceId, VirtualWorkspaceId), Option<WindowId>)> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.space)
            .flat_map(|space| {
                workspaces.existing_workspaces(space).into_iter().map(move |(workspace, _)| {
                    ((space, workspace), engine.active_window(&self.state.windows, space, workspace))
                })
            })
            .collect();
        let focus = focus.and_then(|window| {
            let info = workspaces.workspace_info_for_window_any(&self.state.windows, window)?;
            Some(((info.space, info.workspace_id), window))
        });
        Rings::of(&active, focus)
    }

    /// The rings as rini's layout has them now: its focused window holding focus.
    pub(super) fn layout_rings(&self) -> Rings {
        self.rings_with(self.layout_manager.layout_engine.focused_window())
    }

    /// What `window` wears through a pass ending with `rings`, against the rings the last batch left.
    pub(super) fn worn(&self, window: WindowId, rings: &Rings) -> Worn {
        Worn {
            decoration: self.config.settings.decorations.decoration(),
            wore_ring: self.rings_drawn.ringed(window),
            wears_ring: rings.ringed(window),
        }
    }
}
