//! Moving a window: to another workspace on its display, to another display's space, and between
//! the strip and the floating layer. Each move takes the window out of its layer with
//! `take_window` and puts it into the destination with `place_window`.

use objc2_core_foundation::CGSize;
use rini_core::ids::{SpaceId, WindowId};
use rini_ipc::protocol::WorkspaceSelector;
use tracing::{debug, warn};

use crate::layout::LayoutId;
use crate::layout::domain::boundary::workspace_stack_direction;
use crate::workspaces::WindowStore;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId;
use crate::workspaces::engine::{EventResponse, LayoutEngine};

/// The layer of a workspace a window sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Layer {
    Floating,
    Tiled,
}

/// One workspace as one space shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Slot {
    pub(super) space: SpaceId,
    pub(super) workspace: VirtualWorkspaceId,
}

/// A window out of its layer, with the slot it left so a placement that fails can put it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Detached {
    pub(super) window: WindowId,
    pub(super) layer: Layer,
    pub(super) from: Slot,
}

impl LayoutEngine {
    /// `window` out of the layer it is in: the floating layer of every space, or every strip.
    fn detach(&mut self, window: WindowId) -> Layer {
        if self.floating.is_floating(window) {
            self.floating.remove_active_for_window(window);
            Layer::Floating
        } else {
            self.remove_window_from_all_tiling_trees(window);
            Layer::Tiled
        }
    }

    /// `window` into `layer` on `slot`: the floating layer of its space, or the strip it shows,
    /// after the selection. The layout a tiled window joined, `None` when the strip has none.
    fn attach(&mut self, window: WindowId, layer: Layer, slot: Slot) -> Option<LayoutId> {
        match layer {
            Layer::Floating => {
                self.floating.add_active(slot.space, window.pid, window);
                None
            }
            Layer::Tiled => {
                let layout = self.workspace_layouts.active(slot.space, slot.workspace)?;
                self.workspace_tree_mut(slot.workspace)
                    .add_window_after_selection(layout, window);
                Some(layout)
            }
        }
    }

    pub(super) fn take_window(&mut self, window: WindowId, from: Slot) -> Detached {
        Detached {
            window,
            layer: self.detach(window),
            from,
        }
    }

    /// `detached` assigned to `to`'s workspace and put in the same layer there: a tiled window at
    /// the width it last had on that display, a floating one into the floating layer only when
    /// `to` is the workspace its space shows. When the assignment fails the window goes back to
    /// the slot it left, and the answer is false.
    pub(super) fn place_window(
        &mut self,
        window_store: &mut WindowStore,
        memory: &DisplayMemory,
        detached: Detached,
        to: Slot,
    ) -> bool {
        let Detached { window, layer, from } = detached;
        let assigned = self.virtual_workspace_manager.assign_window_to_workspace(
            window_store,
            to.space,
            window,
            to.workspace,
        );
        if !assigned {
            let _ = self.attach(window, layer, from);
            return false;
        }
        match layer {
            Layer::Floating => {
                if self.virtual_workspace_manager.active_workspace(to.space) == Some(to.workspace) {
                    let _ = self.attach(window, layer, to);
                }
            }
            Layer::Tiled => {
                if let Some(layout) = self.attach(window, layer, to) {
                    self.apply_remembered_column_width(
                        memory,
                        to.space,
                        to.workspace,
                        layout,
                        window,
                    );
                }
            }
        }
        true
    }

    /// Moves the focused window between the strip and the floating layer.
    pub(super) fn toggle_window_floating(
        &mut self,
        window_store: &mut WindowStore,
        space: Option<SpaceId>,
        is_floating: bool,
    ) -> EventResponse {
        let Some(wid) = self.focused_window else {
            return EventResponse::default();
        };
        if is_floating {
            if let Some(space) = space {
                let Some(workspace) = self
                    .virtual_workspace_manager
                    .workspace_for_window(window_store, space, wid)
                    .or_else(|| self.virtual_workspace_manager.active_workspace(space))
                else {
                    warn!(
                        "No workspace on space {:?} to tile floating window {:?} into",
                        space, wid
                    );
                    return EventResponse::default();
                };
                let _ = self.detach(wid);
                if self.attach(wid, Layer::Tiled, Slot { space, workspace }).is_some() {
                    debug!(
                        "Re-added floating window {:?} to tiling tree in workspace {:?}",
                        wid, workspace
                    );
                }
            }
            self.floating.remove_floating(wid);
            self.floating.set_last_focus(None);
        } else {
            if let Some(space) = space {
                let _ = self.detach(wid);
                self.floating.add_active(space, wid.pid, wid);
            }
            self.floating.add_floating(wid);
            self.floating.set_last_focus(Some(wid));
            debug!("Removed window {:?} from tiling tree, now floating", wid);
        }
        EventResponse::default()
    }

    /// Moves a window to another workspace on the same space, following it if asked.
    ///
    /// The window is whichever index was named, else the focused one. Following means the display
    /// switches to the destination, which is why this cannot simply reassign and return.
    pub(super) fn move_window_to_workspace(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        space: SpaceId,
        workspace: &WorkspaceSelector,
        follow: bool,
        maybe_id: &Option<u32>,
    ) -> EventResponse {
        let window = match maybe_id {
            Some(idx) => {
                self.virtual_workspace_manager.find_window_by_idx(window_store, space, *idx)
            }
            None => self.focused_window,
        };
        let Some(window) = window else {
            return EventResponse::default();
        };
        let space = self.space_with_window(window).unwrap_or(space);

        let workspaces = self.virtual_workspace_manager_mut().list_workspaces(space);
        let Some(current) =
            self.virtual_workspace_manager.workspace_for_window(window_store, space, window)
        else {
            return EventResponse::default();
        };
        let target = match workspace {
            WorkspaceSelector::Index(index) => workspaces.get(*index).map(|(id, _)| *id),
            WorkspaceSelector::Name(name) if name == "next" => self
                .virtual_workspace_manager
                .next_workspace(window_store, space, current, None),
            WorkspaceSelector::Name(name) if name == "prev" => self
                .virtual_workspace_manager
                .prev_workspace(window_store, space, current, None),
            WorkspaceSelector::Name(name) => workspaces
                .iter()
                .find_map(|(id, workspace_name)| (workspace_name == name).then_some(*id)),
        };
        let Some(target) = target else {
            // The stack has no workspace that way, so the view bounces and the stop reads as an end
            // rather than a dropped keypress — the same answer a blocked workspace SWITCH gives. A
            // selector that names a workspace outright reports no edge: not finding workspace 7 is a
            // request that cannot be honoured, not a push against the bottom of the stack.
            return EventResponse {
                edge_hit: workspace_stack_direction(workspace),
                ..EventResponse::default()
            };
        };
        if current == target {
            return EventResponse::default();
        }
        let (from, to) = (Slot { space, workspace: current }, Slot {
            space,
            workspace: target,
        });

        // The width is read back from per-display affinity once the window lands: this move keeps
        // it on the same display, so what it had here is what it gets there.
        if !self.floating.is_floating(window)
            && let Some(layout) = self.workspace_layouts.active(space, current)
        {
            self.remember_column_width(memory, space, current, layout, window);
        }

        let detached = self.take_window(window, from);
        if !self.place_window(window_store, memory, detached, to) {
            return EventResponse::default();
        }
        if follow {
            return self.activate_workspace(window_store, space, target, Some(window));
        }
        self.focus_after_workspace_move(window_store, window, from, to)
    }

    /// Where focus goes once `window` moved from `from` to `to` on one space and the view stayed:
    /// with the window when it landed on the workspace showing, else to the first window left
    /// behind on the one it left.
    fn focus_after_workspace_move(
        &mut self,
        window_store: &WindowStore,
        window: WindowId,
        from: Slot,
        to: Slot,
    ) -> EventResponse {
        let space = from.space;
        let active_workspace = self.virtual_workspace_manager.active_workspace(space);
        if Some(to.workspace) == active_workspace {
            return EventResponse {
                changed: true,
                focus_window: Some(window),
                ..EventResponse::default()
            };
        }
        if Some(from.workspace) == active_workspace {
            self.focused_window = None;
            self.virtual_workspace_manager
                .set_last_focused_window(space, from.workspace, None);
            let remaining_windows =
                self.virtual_workspace_manager.windows_in_active_workspace(window_store, space);
            if let Some(&new_focus) = remaining_windows.first() {
                return EventResponse {
                    changed: true,
                    focus_window: Some(new_focus),
                    ..EventResponse::default()
                };
            }
        }
        self.virtual_workspace_manager
            .set_last_focused_window(space, to.workspace, Some(window));
        EventResponse {
            changed: true,
            ..EventResponse::default()
        }
    }

    /// Moves `window_id` onto the workspace `target_space` shows, which is its first when it shows
    /// none, and focuses it there.
    pub fn move_window_to_space(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        source_space: SpaceId,
        target_space: SpaceId,
        target_screen_size: CGSize,
        window_id: WindowId,
    ) -> EventResponse {
        let moved = EventResponse {
            changed: true,
            raise_windows: vec![window_id],
            focus_window: Some(window_id),
            edge_hit: None,
            moved: None,
        };
        if source_space == target_space {
            return moved;
        }

        let _ = self.virtual_workspace_manager.list_workspaces(source_space);
        let _ = self.virtual_workspace_manager.list_workspaces(target_space);

        let Some(source_workspace) = self
            .virtual_workspace_manager
            .workspace_for_window(window_store, source_space, window_id)
            .or_else(|| self.virtual_workspace_manager.active_workspace(source_space))
        else {
            return EventResponse::default();
        };
        let Some((target_workspace, _)) = self.ensure_active_workspace_info(target_space) else {
            return EventResponse::default();
        };
        let (from, to) = (
            Slot {
                space: source_space,
                workspace: source_workspace,
            },
            Slot {
                space: target_space,
                workspace: target_workspace,
            },
        );

        let detached = self.take_window(window_id, from);
        self.expose_layouts(target_space, target_screen_size);
        if !self.place_window(window_store, memory, detached, to) {
            return EventResponse::default();
        }
        // A floating frame saved on the source display means nothing on this one. A tiled window
        // took the width it last had on the DESTINATION display in `place_window`: half width is
        // roomy on a 2338pt monitor and cramped on a 1728pt panel, so the source's would be wrong.
        if detached.layer == Layer::Floating {
            self.floating_positions.remove_window(window_id);
            self.floating.set_last_focus(Some(window_id));
        }

        if self.virtual_workspace_manager.active_workspace(source_space) == Some(source_workspace) {
            self.virtual_workspace_manager.set_last_focused_window(
                source_space,
                source_workspace,
                None,
            );
        }
        self.virtual_workspace_manager.set_last_focused_window(
            target_space,
            target_workspace,
            Some(window_id),
        );
        self.focused_window = Some(window_id);
        moved
    }
}
