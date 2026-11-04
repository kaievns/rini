//! Launch memory, the projection and the lookup: where each application's windows are now,
//! recorded under the attached displays, and the slot a relaunched window takes from it. See
//! `src/workspaces/docs/launch-memory.md`.

use rini_core::ids::{DisplayUuid, SpaceId, WindowId};
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, info, warn};

use crate::layout::ColumnWidth;
use crate::workspaces::WindowStore;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::domain::display_setup::SetupId;
use crate::workspaces::domain::launch_memory::{Slot, is_focuslab_window, slot_for_window};
use crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId;
use crate::workspaces::engine::LayoutEngine;

/// What the layouts of a window's workspace say about its width.
///
/// `NotHeld` is not `AtDefault`: a window no layout holds has an unread width, not a default one.
/// See `ProjectedWidth`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum LayoutWidth {
    /// A layout holding the window carries this width for it.
    Carried(ColumnWidth),
    /// A layout holds the window at the display's configured default.
    AtDefault,
    /// No layout of the workspace holds the window.
    NotHeld,
}

/// Why a window takes no remembered slot, which the lookup logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoLaunchSlot {
    /// No display is attached yet, so there is no arrangement to look under.
    NoTopology,
    /// The application left no slot under the displays attached now.
    NothingRemembered,
    /// The window is the `ordinal`th of its application, and none of the `of` slots is free for it,
    /// by title or by ordinal.
    NoFreeSlot { ordinal: usize, of: usize },
    /// A window rule applies to it, and a rule is placed over a slot.
    RuleApplies,
    /// The slot's workspace, the `workspace_index`th, could not take the window.
    WorkspaceRefused { workspace_index: usize },
    /// The window is one focuslab opened, which neither leaves a slot nor takes one.
    FocuslabWindow,
}

impl LayoutEngine {
    /// The displays attached right now, which is the topology the launch memory is keyed by.
    pub fn set_connected_displays(&mut self, uuids: Vec<DisplayUuid>) {
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
        connected: &[DisplayUuid],
    ) {
        use crate::workspaces::domain::launch_memory::{ProjectedSlot, ProjectedWidth};

        let topology = SetupId::of(connected);
        if topology.is_empty() {
            return;
        }

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
                        .clone();
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
                            LayoutWidth::Carried(width) => ProjectedWidth::Known(Some(width)),
                            LayoutWidth::AtDefault => ProjectedWidth::Known(None),
                            LayoutWidth::NotHeld => {
                                match memory.affinity.window_width(&display, window_id) {
                                    Some(width) => ProjectedWidth::Known(Some(width)),
                                    None => ProjectedWidth::Unknown,
                                }
                            }
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
    /// Every configuration is consulted, not only the active display size: a window made full width
    /// on the external lives in that size's layout, and while the built-in is showing the active
    /// layout knows nothing about it.
    pub(super) fn width_from_layouts(
        &self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        window: WindowId,
    ) -> LayoutWidth {
        let tree = self.workspace_tree(workspace_id);
        let mut held = false;
        for layout in self.workspace_layouts.configurations_for(space, workspace_id) {
            if let Some(width) = tree.carried_width(layout, window) {
                return LayoutWidth::Carried(width);
            }
            held |= tree.contains_window(layout, window);
        }
        if held {
            LayoutWidth::AtDefault
        } else {
            LayoutWidth::NotHeld
        }
    }

    /// The slot a window appearing for the first time should take, if its application left one,
    /// with the reason logged when it takes none.
    pub(super) fn launch_slot_for_new_window(
        &self,
        window_store: &WindowStore,
        memory: &DisplayMemory,
        window: WindowId,
        app_id: &str,
        title: Option<&str>,
    ) -> Option<Slot> {
        match self.find_launch_slot(window_store, memory, window, app_id, title) {
            Ok(slot) => Some(slot),
            Err(gave_up) => {
                self.log_no_launch_slot(window, app_id, gave_up);
                None
            }
        }
    }

    /// Whether `app_id` left any slot under the displays attached now.
    pub(super) fn has_launch_slots(&self, memory: &DisplayMemory, app_id: &str) -> bool {
        !memory.launch.slots(app_id, &SetupId::of(&self.connected_displays)).is_empty()
    }

    /// Logs why `window` takes no remembered slot. Every way the lookup gives up is logged at a level
    /// the service keeps (`src/workspaces/docs/launch-memory.md`).
    pub(super) fn log_no_launch_slot(&self, window: WindowId, app_id: &str, gave_up: NoLaunchSlot) {
        let topology = SetupId::of(&self.connected_displays);
        match gave_up {
            NoLaunchSlot::NoTopology => warn!(
                idx = window.idx.get(),
                app_id,
                ?gave_up,
                "no display topology yet, so nothing remembered can be looked up: this window gets \
                 today's defaults"
            ),
            NoLaunchSlot::NothingRemembered
            | NoLaunchSlot::NoFreeSlot { .. }
            | NoLaunchSlot::RuleApplies
            | NoLaunchSlot::WorkspaceRefused { .. }
            | NoLaunchSlot::FocuslabWindow => info!(
                idx = window.idx.get(),
                app_id,
                %topology,
                ?gave_up,
                "no remembered slot for this window; it gets today's defaults"
            ),
        }
    }

    /// The slot `window` takes, or why it takes none.
    ///
    /// The window's position among its application's windows, and which slots its siblings already took,
    /// are recomputed from the siblings themselves rather than tracked. Placement order is window server
    /// id order, so replaying the same matching over the siblings gives the same answers it gave them,
    /// and two windows cannot land on one slot.
    ///
    /// Identity comes from the caller, not from the window store: the first sighting of a launching
    /// application's window happens while the rules are being applied, before the window is in the
    /// store.
    pub(super) fn find_launch_slot(
        &self,
        window_store: &WindowStore,
        memory: &DisplayMemory,
        window: WindowId,
        app_id: &str,
        title: Option<&str>,
    ) -> Result<Slot, NoLaunchSlot> {
        let topology = SetupId::of(&self.connected_displays);
        if topology.is_empty() {
            return Err(NoLaunchSlot::NoTopology);
        }
        if is_focuslab_window(app_id, title) {
            return Err(NoLaunchSlot::FocuslabWindow);
        }
        let slots = memory.launch.slots(app_id, &topology);
        if slots.is_empty() {
            return Err(NoLaunchSlot::NothingRemembered);
        }

        // Siblings already placed, in the order they were placed. A focuslab window left no slot, so
        // it is no sibling either: counted, it would shift every ordinal after it.
        let mut siblings: Vec<(u32, WindowId)> = window_store
            .iter_windows()
            .filter(|(other, state)| {
                *other != window
                    && state.info.bundle_id.as_deref() == Some(app_id)
                    && !is_focuslab_window(app_id, Some(&state.info.title))
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

        let ordinal = siblings.len();
        let (index, slot) = slot_for_window(slots, title, ordinal, &claimed)
            .and_then(|index| Some((index, slots.get(index)?)))
            .ok_or(NoLaunchSlot::NoFreeSlot { ordinal, of: slots.len() })?;
        debug!(
            idx = window.idx.get(),
            app_id,
            slot = index,
            of = slots.len(),
            width = ?slot.width,
            "a relaunched window matched a remembered slot"
        );
        Ok(slot.clone())
    }
}
