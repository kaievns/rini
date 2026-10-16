//! What belongs to a display: each window's home, the strip order a replug rebuilds, and the
//! width each window has on each display.

use rini_core::ids::{DisplayUuid, SpaceId, WindowId};
use tracing::debug;

use crate::layout::LayoutId;
use crate::workspaces::WindowStore;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId;
use crate::workspaces::engine::LayoutEngine;

impl LayoutEngine {
    pub fn update_space_display(
        &mut self,
        memory: &mut DisplayMemory,
        space: SpaceId,
        display_uuid: Option<DisplayUuid>,
    ) {
        if let Some(uuid) = display_uuid {
            memory.affinity.set_display_space(&uuid, space);
        }
    }

    /// Record that `window` belongs to the display currently owning `space`.
    ///
    /// Only call this from paths that express intent — an explicit move, or the first
    /// time a window is seen. The forced reassignment that follows a display change must
    /// NOT call it: an unplug evacuates windows onto the remaining display, and recording
    /// that as their home destroys the record needed to bring them back on replug.
    pub fn set_window_display_home(
        &mut self,
        memory: &mut DisplayMemory,
        window: WindowId,
        space: SpaceId,
    ) {
        if let Some(display) = memory.affinity.display_for_space(space) {
            let display = display.to_owned();
            memory.affinity.set_window_home(window, &display);
        }
    }

    /// Record a home for a newly seen window, leaving an existing home untouched.
    pub fn note_window_display_home(
        &mut self,
        memory: &mut DisplayMemory,
        window: WindowId,
        space: SpaceId,
    ) {
        if let Some(display) = memory.affinity.display_for_space(space) {
            let display = display.to_owned();
            memory.affinity.set_window_home_if_absent(window, &display);
        }
    }

    /// Record what is on one display for a settled topology: the strip order, and a home for any
    /// window that does not have one yet. `live_windows` is the display's strip in visual order.
    /// Windows homed to a detached display keep that home (evacuation). See
    /// `src/workspaces/docs/workspaces-and-displays.md`.
    ///
    /// It does NOT re-home a window that already has one, and that is the whole point. Every real
    /// way a window changes display already writes the home at the moment the user asks: a drag
    /// (`events::drag`), an explicit move command (`events::command`), a first sighting
    /// (`note_window_display_home`), a restore. This pass runs afterwards and sees the RESULT of
    /// rini's own layout, which already follows from the home — so re-homing from it was circular,
    /// and any reason a window was temporarily somewhere else became its new address.
    ///
    /// That is the "windows teleport between displays" report: a window rini parked, evacuated or
    /// laid out on a neighbour was re-homed there by the next settled topology and never came back.
    /// Windows homed to a DETACHED display are the evacuation case and were already spared; this
    /// spares the attached case too, which is the one that was moving.
    pub fn sync_display_affinity(
        &mut self,
        memory: &mut DisplayMemory,
        display_uuid: &DisplayUuid,
        live_windows: &[WindowId],
    ) {
        for window in live_windows {
            memory.affinity.set_window_home_if_absent(*window, display_uuid);
        }

        // The strip is the arrangement to rebuild on replug, so it must reflect only the
        // windows that genuinely live here. A window parked here while its own display is
        // unplugged would otherwise be recorded as part of this display's layout.
        let strip: Vec<WindowId> = live_windows
            .iter()
            .copied()
            .filter(|window| memory.affinity.window_home(*window) == Some(display_uuid))
            .collect();
        memory.affinity.set_display_strip(display_uuid, strip);
    }

    /// Windows that belong on `display` but are currently assigned somewhere else.
    ///
    /// This is what a replug consults. It deliberately reports only windows whose current
    /// assignment disagrees with their home, so a display that came back to find its own
    /// windows already in place produces no moves at all.
    pub fn windows_to_repatriate(
        &self,
        window_store: &WindowStore,
        memory: &DisplayMemory,
        display_uuid: &DisplayUuid,
        target_space: SpaceId,
    ) -> Vec<WindowId> {
        memory
            .affinity
            .windows_homed_to(display_uuid)
            .into_iter()
            .filter(|window| {
                window_store
                    .workspace_info_for_window(*window)
                    .is_some_and(|assignment| assignment.space != target_space)
            })
            .collect()
    }

    /// Drop affinity for windows that have gone. Called on every settled topology: the removal
    /// paths alone miss windows closed while their display was unplugged.
    pub fn forget_affinity_for_dead_windows(
        &mut self,
        window_store: &WindowStore,
        memory: &mut DisplayMemory,
    ) {
        let stale: Vec<WindowId> = memory
            .affinity
            .homed_windows()
            .into_iter()
            .filter(|window| !window_store.contains_window(*window))
            .collect();
        for window in stale {
            memory.affinity.forget_window(window);
        }
    }

    /// Record the width `window` now occupies on the display owning `space`.
    ///
    /// Called after any command that deliberately sets a width. Width is remembered per
    /// DISPLAY, so this is the only place the two are tied together; see
    /// `DisplayAffinity::window_width` for why the layout tree is the wrong home for it.
    pub(super) fn remember_column_width(
        &mut self,
        memory: &mut DisplayMemory,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        layout: LayoutId,
        window: WindowId,
    ) {
        let Some(display) = memory.affinity.display_for_space(space).cloned() else {
            return;
        };
        match self.workspace_tree(workspace_id).carried_width(layout, window) {
            Some(width) => memory.affinity.set_window_width(&display, window, width),
            // Toggled back to the default. Forget rather than pin, so the window follows
            // each display's configured ratio again.
            None => memory.affinity.clear_window_width(&display, window),
        }
    }

    /// Record the width of whichever column the resize commands just acted on.
    ///
    /// Those commands operate on the selection and report nothing back, so the window has to
    /// be looked up rather than taken from a return value.
    pub(super) fn remember_selected_column_width(
        &mut self,
        memory: &mut DisplayMemory,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        layout: LayoutId,
    ) {
        let Some(window) = self.workspace_tree(workspace_id).selected_window(layout) else {
            return;
        };
        self.remember_column_width(memory, space, workspace_id, layout, window);
    }

    /// Re-apply the width `window` last had on the display owning `space`.
    ///
    /// Called when a window ARRIVES in a strip — a new workspace, or a new display. A fresh
    /// column starts at the configured default, so without this a window sized to fill the
    /// built-in lost that size the moment it changed workspace.
    pub(super) fn apply_remembered_column_width(
        &mut self,
        memory: &DisplayMemory,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        layout: LayoutId,
        window: WindowId,
    ) {
        let Some(on_display) = memory.affinity.display_for_space(space).cloned() else {
            debug!(
                idx = window.idx.get(),
                ?space,
                "no display known for this space, so no remembered width can be applied"
            );
            return;
        };
        let found = memory.affinity.window_width(&on_display, window);
        debug!(idx = window.idx.get(), %on_display, width = ?found, "applying a remembered width");
        let Some(width) = found else {
            return;
        };
        self.workspace_tree_mut(workspace_id).set_carried_width(layout, window, width);
    }
}
