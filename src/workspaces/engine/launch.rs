//! Launch memory, the projection and the lookup: where each application's windows are now,
//! recorded under the attached displays, and the slot a relaunched window takes from it. See
//! `src/workspaces/docs/launch-memory.md`.

use rini_core::ids::{SpaceId, WindowId};
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, warn};

use crate::layout::ColumnWidth;
use crate::workspaces::WindowStore;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId;
use crate::workspaces::engine::LayoutEngine;

impl LayoutEngine {
    /// The displays attached right now, which is the topology the launch memory is keyed by.
    pub fn set_connected_displays(&mut self, uuids: Vec<String>) {
        self.connected_displays = uuids;
    }

    /// Projects live windows into the launch memory, so their next launch can find them.
    ///
    /// A projection computed before each save rather than hooks on every move and resize: one write
    /// path, and no new work in the paths that place windows. See `src/workspaces/docs/launch-memory.md` (in this crate).
    ///
    /// `connected` is the display UUIDs currently attached, which is the topology the answer is filed
    /// under. Nothing is recorded without it, since an answer with no topology cannot be looked up.
    pub fn remember_launch_slots(
        &mut self,
        window_store: &WindowStore,
        memory: &mut DisplayMemory,
        connected: &[String],
    ) {
        use crate::workspaces::domain::launch_memory::{
            ProjectedSlot, ProjectedWidth, topology_key,
        };

        if connected.is_empty() {
            return;
        }
        let topology = topology_key(connected);

        // Grouped by bundle identifier, ordered by window server id, which is creation order. A
        // relaunched application creates its windows in the same order, so the Nth window can find the
        // Nth slot when its title is no help.
        let mut by_app: HashMap<String, Vec<(u32, WindowId)>> = HashMap::default();
        for (window_id, window) in window_store.iter_windows() {
            let Some(app_id) = window.info.bundle_id.clone() else {
                continue;
            };
            by_app.entry(app_id).or_default().push((window_id.idx.get(), window_id));
        }

        for (app_id, mut windows) in by_app {
            windows.sort_unstable();
            let slots: Vec<ProjectedSlot> = windows
                .into_iter()
                .filter_map(|(_, window_id)| {
                    let window = window_store.window(window_id)?;
                    // The window's OWN space and workspace, which is not the same question as which
                    // workspace is showing: a window sitting in a workspace nobody is looking at still
                    // belongs to it, and using an active-workspace-only lookup here meant every
                    // application whose windows were all parked lost its memory on the next save.
                    let info = self
                        .virtual_workspace_manager
                        .workspace_info_for_window_any(window_store, window_id)?;
                    // Its recorded home, or the display its space is on. A home is written once, only if
                    // the space's display was known then, so a window may have none.
                    let display = memory
                        .affinity
                        .window_home(window_id)
                        .or_else(|| memory.affinity.display_for_space(info.space))?
                        .to_owned();
                    let index = self
                        .virtual_workspace_manager
                        .list_workspaces(info.space)
                        .iter()
                        .position(|(id, _)| *id == info.workspace_id)?;
                    // The width the window actually has, from its layout. The affinity map alone missed
                    // widths that came from the layout rather than a command.
                    //
                    // `Unknown` when no layout holding the window could be consulted and the record
                    // has nothing either. It must NOT collapse to "no width": that is what erased a
                    // remembered full width on the next autosave.
                    let width =
                        match self.width_from_layouts(info.space, info.workspace_id, window_id) {
                            Some(width) => ProjectedWidth::Known(width),
                            None => match memory.affinity.window_width(&display, window_id) {
                                Some(width) => ProjectedWidth::Known(Some(width)),
                                None => ProjectedWidth::Unknown,
                            },
                        };
                    Some(ProjectedSlot {
                        title: (!window.info.title.trim().is_empty())
                            .then(|| window.info.title.clone()),
                        display_uuid: display,
                        workspace_index: index,
                        width,
                    })
                })
                .collect();
            memory.launch.remember(&app_id, &topology, slots);
        }
    }

    /// The width `window` has in its workspace, read from the layout that holds it.
    ///
    /// `None` means no layout of this workspace holds the window at all, which is not the same as the
    /// window having no deliberate width — see `ProjectedWidth`. `Some(None)` is a window sitting at
    /// the display's configured default.
    ///
    /// Every configuration is consulted, not only the active display size: a window made full width
    /// on the external lives in that size's layout, and while the built-in is showing the active
    /// layout knows nothing about it.
    fn width_from_layouts(
        &self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        window: WindowId,
    ) -> Option<Option<ColumnWidth>> {
        let tree = self.workspace_tree(workspace_id);
        let mut consulted = false;
        for layout in self.workspace_layouts.configurations_for(space, workspace_id) {
            if let Some(width) = tree.column_width(layout, window) {
                return Some(Some(width));
            }
            consulted |= tree.contains_window(layout, window);
        }
        consulted.then_some(None)
    }

    /// The slot a window appearing for the first time should take, if its application left one.
    ///
    /// The window's position among its application's windows, and which slots its siblings already took,
    /// are recomputed from the siblings themselves rather than tracked. Placement order is window server
    /// id order, so replaying the same matching over the siblings gives the same answers it gave them,
    /// and two windows cannot land on one slot.
    pub(super) fn launch_slot_for_new_window(
        &self,
        window_store: &WindowStore,
        memory: &DisplayMemory,
        window: WindowId,
        app_id: &str,
        title: Option<&str>,
    ) -> Option<crate::workspaces::domain::launch_memory::Slot> {
        use crate::workspaces::domain::launch_memory::{slot_for_window, topology_key};

        // Identity comes from the caller, not from the window store. The first sighting of a launching
        // application's window happens while the rules are being applied, and the window is not in the
        // store yet at that point — reading it there is why this silently did nothing.
        // Every way of giving up here is logged. The first three times a remembered full width was
        // lost, the reason had to be reconstructed from the layout file, because this returned `None`
        // for four different reasons without saying which.
        if self.connected_displays.is_empty() {
            warn!(
                idx = window.idx.get(),
                app_id,
                "no display topology yet, so nothing remembered can be looked up: this window gets \
                 today's defaults"
            );
            return None;
        }
        let topology = topology_key(&self.connected_displays);
        let slots = memory.launch.slots(app_id, &topology);
        if slots.is_empty() {
            debug!(
                idx = window.idx.get(),
                app_id, topology, "nothing remembered for this application under this topology"
            );
            return None;
        }

        // Siblings already placed, in the order they were placed.
        let mut siblings: Vec<(u32, WindowId)> = window_store
            .iter_windows()
            .filter(|(other, state)| {
                *other != window
                    && state.info.bundle_id.as_deref() == Some(app_id)
                    && memory.affinity.window_home(*other).is_some()
            })
            .map(|(other, _)| (other.idx.get(), other))
            .collect();
        siblings.sort_unstable();

        let mut claimed = Vec::with_capacity(siblings.len());
        for (ordinal, (_, sibling)) in siblings.iter().enumerate() {
            let title = window_store.window(*sibling).map(|state| state.info.title.as_str());
            if let Some(index) = slot_for_window(slots, title, ordinal, &claimed) {
                claimed.push(index);
            }
        }

        let Some(index) = slot_for_window(slots, title, siblings.len(), &claimed) else {
            debug!(
                idx = window.idx.get(),
                app_id,
                ordinal = siblings.len(),
                of = slots.len(),
                "no free remembered slot for this window; it gets today's defaults"
            );
            return None;
        };
        debug!(
            idx = window.idx.get(),
            app_id,
            slot = index,
            of = slots.len(),
            width = ?slots.get(index).and_then(|slot| slot.width),
            "a relaunched window matched a remembered slot"
        );
        slots.get(index).cloned()
    }
}
