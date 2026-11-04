//! What belongs to a display: each window's home, the strip order a replug rebuilds, and the
//! width each window has on each display.

use rini_core::ids::{DisplayUuid, SpaceId, WindowId};
use tracing::{debug, info};

use crate::layout::{ColumnWidth, LayoutId};
use crate::workspaces::WindowStore;
use crate::workspaces::domain::display_affinity::{Erase, ErasedWidth};
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
    /// paths alone miss windows closed while their display was unplugged. A window the store does
    /// not hold is gone when it has a home in the arrangement in force, or when this session saw
    /// it, as a minimized window it saw and kept only the widths of. One from the file this session
    /// has not seen is left to its app's report: `forget_windows_gone_since_the_save` judged it.
    pub fn forget_affinity_for_dead_windows(
        &mut self,
        window_store: &WindowStore,
        memory: &mut DisplayMemory,
    ) {
        let mut stale: Vec<WindowId> = memory.affinity.homed_windows();
        stale.extend(
            memory
                .affinity
                .remembered_windows()
                .into_iter()
                .filter(|window| self.was_sighted(*window)),
        );
        stale.sort_unstable();
        stale.dedup();
        stale.retain(|window| !window_store.contains_window(*window));
        for window in stale {
            self.log_erased_widths(Erase::Gone, &memory.affinity.forget_window(window));
        }
    }

    /// Forgets, as rini starts, every window the file has records for that is neither a candidate
    /// of the startup restore nor still held by the window server for its process
    /// (`window_exists`, asked of the window server id a window's id carries). A window id is its
    /// process id and that window server id, both reused after a reboot, so a record left under
    /// one would size a different window.
    pub(super) fn forget_windows_gone_since_the_save(
        &self,
        memory: &mut DisplayMemory,
        mut window_exists: impl FnMut(WindowId, u32) -> bool,
    ) {
        let gone: Vec<WindowId> = memory
            .affinity
            .remembered_windows()
            .into_iter()
            .filter(|window| {
                !self.is_restore_candidate(*window) && !window_exists(*window, window.idx.get())
            })
            .collect();
        for window in gone {
            self.log_erased_widths(Erase::GoneSinceTheSave, &memory.affinity.forget_window(window));
        }
    }

    /// Record the width `window` now occupies on the display owning `space`.
    ///
    /// Called after a width command (`remember_given_width`) and before a move to another workspace.
    /// Width is remembered per DISPLAY, so this is the only place the two are tied together; see
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
            None => {
                if let Some(width) = memory.affinity.clear_window_width(&display, window) {
                    self.log_erased_widths(Erase::AtDefault, &[ErasedWidth {
                        window,
                        setup: memory.affinity.current_setup().clone(),
                        display,
                        width,
                    }]);
                }
            }
        }
    }

    /// Record the width `window`'s column has where it is laid out, when it has one: at a drag's end,
    /// and before a removal the window can come back from, so a width only the layout held is the
    /// one read back. A column at the default, or a window in no strip, writes nothing and erases
    /// nothing, since the record may hold a width the layout has not been given yet. Says which
    /// width it recorded, and on which space's display.
    pub(super) fn remember_width_as_laid(
        &mut self,
        window_store: &WindowStore,
        memory: &mut DisplayMemory,
        window: WindowId,
    ) -> Option<(SpaceId, ColumnWidth)> {
        let assignment = window_store.workspace_info_for_window(window)?;
        let (space, workspace_id) = (assignment.space, assignment.workspace_id);
        let layout = self.workspace_layouts.active(space, workspace_id)?;
        let width = self.workspace_tree(workspace_id).carried_width(layout, window)?;
        let display = memory.affinity.display_for_space(space).cloned()?;
        memory.affinity.set_window_width(&display, window, width);
        Some((space, width))
    }

    /// Lay `window`'s column at the width it has recorded on the display it is laid out on, when it
    /// is in a strip and has one: a window given a width after it joined its strip, as the id that
    /// owns a window back from native fullscreen is.
    pub(super) fn apply_remembered_width_where_laid(
        &mut self,
        window_store: &WindowStore,
        memory: &DisplayMemory,
        window: WindowId,
    ) {
        let Some(assignment) = window_store.workspace_info_for_window(window) else {
            return;
        };
        let (space, workspace_id) = (assignment.space, assignment.workspace_id);
        let Some(layout) = self.workspace_layouts.active(space, workspace_id) else {
            return;
        };
        if self.workspace_tree(workspace_id).contains_window(layout, window) {
            self.apply_remembered_column_width(memory, space, workspace_id, layout, window);
        }
    }

    /// Re-apply the width `window` last had on the display owning `space`, or, for a window new to
    /// the session with none of its own there, the one its app last had on the display's shape.
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
        let from_app = self.take_arrival_width(window, &on_display);
        let own = memory.affinity.window_width(&on_display, window);
        if own.is_none()
            && let Some(width) = from_app
        {
            self.note_laid_from_app(window, &on_display, width);
        }
        let found = own.or(from_app);
        debug!(idx = window.idx.get(), %on_display, width = ?found, "applying a remembered width");
        let Some(width) = found else {
            return;
        };
        self.workspace_tree_mut(workspace_id).set_carried_width(layout, window, width);
    }
}

impl LayoutEngine {
    /// Logs each width record erased, with why and the window's app and title as last recorded: a
    /// width that comes back at the default is explained by the last of these for its window
    /// (`src/workspaces/docs/workspaces-and-displays.md`). Called before the window's record is
    /// forgotten, so the line names it.
    pub fn log_erased_widths(&self, why: Erase, erased: &[ErasedWidth]) {
        for record in erased {
            let (app_id, title) = self.recorded_identity(record.window);
            info!(
                pid = record.window.pid,
                idx = record.window.idx.get(),
                app_id,
                title,
                setup = %record.setup,
                display = %record.display,
                width = ?record.width,
                ?why,
                "erasing a window's width record"
            );
        }
    }
}
