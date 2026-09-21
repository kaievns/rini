use objc2_core_foundation::CGSize;
use serde::{Deserialize, Serialize};

use crate::workspaces::{LayoutId, LayoutSystem};
use rini_core::ids::SpaceId;

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct WorkspaceLayouts {
    map: rustc_hash::FxHashMap<
        (SpaceId, crate::workspaces::VirtualWorkspaceId),
        SpaceLayoutInfo,
    >,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct SpaceLayoutInfo {
    configurations: rustc_hash::FxHashMap<Size, LayoutId>,
    active_size: Size,
    last_saved: Option<LayoutId>,
}

/// Opaque workspace-layout payload used by transactional restore code.
/// Keeping `SpaceLayoutInfo` private prevents persistence from depending on its internal maps.
pub struct WorkspaceLayoutSnapshot(SpaceLayoutInfo);

impl SpaceLayoutInfo {
    fn active(&self) -> Option<LayoutId> {
        self.configurations.get(&self.active_size).copied()
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct Size {
    width: i32,
    height: i32,
}

impl From<CGSize> for Size {
    fn from(value: CGSize) -> Self {
        Self {
            width: value.width.round() as i32,
            height: value.height.round() as i32,
        }
    }
}

impl WorkspaceLayouts {
    pub fn validate_persisted(
        &self,
        workspaces: &crate::workspaces::WorkspaceStore,
    ) -> Result<(), String> {
        for (&(space, workspace), info) in &self.map {
            let Some(workspace_info) = workspaces.workspaces.get(workspace) else {
                return Err(format!(
                    "layout state references missing workspace {workspace:?}"
                ));
            };
            // No space-ownership check: a workspace owns one strip per display, so having
            // layout state under several native spaces is now correct rather than corrupt.
            let _ = (space, workspace_info);
            if info.configurations.is_empty() {
                return Err(format!("workspace {workspace:?} has no layout configurations"));
            }
            if !info.configurations.contains_key(&info.active_size) {
                return Err(format!(
                    "workspace {workspace:?} has no configuration for its active display size"
                ));
            }
            for layout in info.configurations.values().copied().chain(info.last_saved) {
                if !workspace_info.layout_system.contains_layout(layout) {
                    return Err(format!(
                        "workspace {workspace:?} references missing layout {layout:?}"
                    ));
                }
            }
        }

        for space in workspaces.initialized_spaces() {
            for (workspace, _) in workspaces.existing_workspaces(space) {
                if !self.map.contains_key(&(space, workspace)) {
                    return Err(format!(
                        "workspace {workspace:?} on native space {} has no layout state",
                        space.get()
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn snapshot_workspace(
        &self,
        space: SpaceId,
        workspace: crate::workspaces::VirtualWorkspaceId,
    ) -> Option<WorkspaceLayoutSnapshot> {
        self.map.get(&(space, workspace)).cloned().map(WorkspaceLayoutSnapshot)
    }

    pub fn install_workspace_snapshot(
        &mut self,
        space: SpaceId,
        workspace: crate::workspaces::VirtualWorkspaceId,
        snapshot: WorkspaceLayoutSnapshot,
    ) {
        self.map.insert((space, workspace), snapshot.0);
    }

    pub fn contains_workspace(
        &self,
        space: SpaceId,
        workspace: crate::workspaces::VirtualWorkspaceId,
    ) -> bool {
        self.map.contains_key(&(space, workspace))
    }

    pub fn ensure_active_for_space(
        &mut self,
        space: SpaceId,
        size: CGSize,
        workspaces: impl IntoIterator<Item = crate::workspaces::VirtualWorkspaceId>,
        tree: &mut impl LayoutSystem,
    ) {
        let size = Size::from(size);
        for workspace_id in workspaces {
            let workspace_key = (space, workspace_id);
            let (workspace_layout, mut unchanged) = match self.map.entry(workspace_key) {
                std::collections::hash_map::Entry::Vacant(entry) => (
                    entry.insert(SpaceLayoutInfo {
                        active_size: size,
                        configurations: Default::default(),
                        last_saved: None,
                    }),
                    None,
                ),
                std::collections::hash_map::Entry::Occupied(entry) => {
                    let info = entry.into_mut();
                    let old_size = info.active_size;
                    if old_size != size {
                        if let Some(active_layout) = info.active() {
                            info.configurations.entry(old_size).or_insert(active_layout);
                        }
                        let taken = info.configurations.remove(&old_size);
                        info.active_size = size;
                        (info, taken)
                    } else {
                        (info, None)
                    }
                }
            };

            let layout = match workspace_layout.configurations.entry(size) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    *entry.insert(if let Some(source) = unchanged.take() {
                        source
                    } else if let Some(source) = workspace_layout.last_saved {
                        tree.clone_layout(source)
                    } else {
                        tree.create_layout()
                    })
                }
                std::collections::hash_map::Entry::Occupied(entry) => {
                    workspace_layout.last_saved = Some(*entry.get());
                    *entry.get()
                }
            };

            if let Some(removed) = unchanged {
                tree.remove_layout(removed);
            }

            tracing::debug!(
                "Using layout {:?} for workspace {:?} on space {:?}",
                layout,
                workspace_id,
                space
            );
        }
    }

    pub fn remap_space(&mut self, old_space: SpaceId, new_space: SpaceId) {
        if old_space == new_space {
            return;
        }

        let old_keys: Vec<_> =
            self.map.keys().filter(|(space, _)| *space == old_space).cloned().collect();

        if old_keys.is_empty() {
            return;
        }

        // Prefer the migrated state over anything already associated with the
        // new space (e.g. default layouts created after a reconnect).
        self.map.retain(|(space, _), _| *space != new_space);

        for (space, workspace_id) in old_keys {
            if let Some(info) = self.map.remove(&(space, workspace_id)) {
                self.map.insert((new_space, workspace_id), info);
            }
        }
    }

    pub fn active(
        &self,
        space: SpaceId,
        workspace_id: crate::workspaces::VirtualWorkspaceId,
    ) -> Option<LayoutId> {
        self.map.get(&(space, workspace_id)).and_then(|l| l.active())
    }

    pub fn mark_last_saved(
        &mut self,
        space: SpaceId,
        workspace_id: crate::workspaces::VirtualWorkspaceId,
        layout: LayoutId,
    ) {
        if let Some(info) = self.map.get_mut(&(space, workspace_id)) {
            info.last_saved = Some(layout);
        }
    }

    pub fn active_layouts_for_space(
        &self,
        space: SpaceId,
    ) -> Vec<(crate::workspaces::VirtualWorkspaceId, LayoutId)> {
        let mut layouts = self
            .map
            .iter()
            .filter_map(|(&(sp, ws), info)| {
                if sp == space {
                    info.active().map(|l| (ws, l))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        layouts.sort_unstable();
        layouts
    }

    /// Every layout configuration for ONE workspace on one space, the active display size first.
    ///
    /// A workspace holds a strip per display size, so a window made full width on the external is
    /// recorded in that size's layout and is invisible to `active` while the built-in is showing.
    /// Reading a window's width has to look at all of them, nearest first.
    pub fn configurations_for(
        &self,
        space: SpaceId,
        workspace_id: crate::workspaces::VirtualWorkspaceId,
    ) -> Vec<LayoutId> {
        let Some(info) = self.map.get(&(space, workspace_id)) else {
            return Vec::new();
        };
        let mut layouts: Vec<LayoutId> = info.active().into_iter().collect();
        for layout in info.configurations.values() {
            if !layouts.contains(layout) {
                layouts.push(*layout);
            }
        }
        layouts
    }

    /// Enumerate every serialized layout configuration, not only the currently active display
    /// size. Old-size configurations are restored later and therefore must be sanitized too.
    pub fn all_layouts(&self) -> Vec<(SpaceId, crate::workspaces::VirtualWorkspaceId, LayoutId)> {
        let mut layouts = Vec::new();
        for (&(space, workspace), info) in &self.map {
            layouts.extend(info.configurations.values().map(|layout| (space, workspace, *layout)));
            if let Some(layout) = info.last_saved {
                layouts.push((space, workspace, layout));
            }
        }
        layouts.sort_unstable();
        layouts.dedup();
        layouts
    }

    #[cfg(test)]
    pub fn insert_layout_configuration_for_test(
        &mut self,
        space: SpaceId,
        workspace: crate::workspaces::VirtualWorkspaceId,
        size: CGSize,
        layout: LayoutId,
    ) {
        self.map
            .get_mut(&(space, workspace))
            .expect("test workspace must be initialized")
            .configurations
            .insert(Size::from(size), layout);
    }

    pub fn ensure_active_for_workspace(
        &mut self,
        space: SpaceId,
        size: CGSize,
        workspace_id: crate::workspaces::VirtualWorkspaceId,
        tree: &mut impl LayoutSystem,
    ) {
        self.ensure_active_for_space(space, size, std::iter::once(workspace_id), tree);
    }

    pub fn spaces(&self) -> std::collections::BTreeSet<SpaceId> {
        self.map.keys().map(|(sp, _)| *sp).collect()
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;
    use crate::workspaces::{ScrollingLayoutSystem, VirtualWorkspaceId, WorkspaceStore};

    fn workspace() -> VirtualWorkspaceId {
        WorkspaceStore::new().list_workspaces(SpaceId::new(1))[0].0
    }

    #[test]
    fn a_display_size_change_carries_the_strip_with_it() {
        let mut layouts = WorkspaceLayouts::default();
        let mut tree = ScrollingLayoutSystem::new(&Default::default());
        let space = SpaceId::new(1);
        let ws = workspace();
        layouts.ensure_active_for_space(space, CGSize::new(1000.0, 800.0), [ws], &mut tree);
        let strip = layouts.active(space, ws).unwrap();
        layouts.ensure_active_for_space(space, CGSize::new(2000.0, 1200.0), [ws], &mut tree);
        assert_eq!(layouts.active(space, ws), Some(strip), "a resize must not lose the windows");
        layouts.ensure_active_for_space(space, CGSize::new(1000.0, 800.0), [ws], &mut tree);
        assert_eq!(layouts.active(space, ws), Some(strip));
        assert_eq!(layouts.all_layouts().len(), 1, "no orphaned layout per size");
    }

    #[test]
    fn a_sub_point_size_change_is_the_same_size() {
        let mut layouts = WorkspaceLayouts::default();
        let mut tree = ScrollingLayoutSystem::new(&Default::default());
        let space = SpaceId::new(1);
        let ws = workspace();
        layouts.ensure_active_for_space(space, CGSize::new(1000.0, 800.0), [ws], &mut tree);
        let first = layouts.active(space, ws).unwrap();
        layouts.ensure_active_for_space(space, CGSize::new(1000.4, 799.6), [ws], &mut tree);
        assert_eq!(layouts.active(space, ws), Some(first));
    }

    #[test]
    fn remap_space_moves_the_state_and_drops_what_the_new_id_already_had() {
        let mut layouts = WorkspaceLayouts::default();
        let mut tree = ScrollingLayoutSystem::new(&Default::default());
        let old = SpaceId::new(1);
        let new = SpaceId::new(2);
        let ws = workspace();
        layouts.ensure_active_for_space(old, CGSize::new(1000.0, 800.0), [ws], &mut tree);
        let migrated = layouts.active(old, ws).unwrap();
        layouts.ensure_active_for_space(new, CGSize::new(1000.0, 800.0), [ws], &mut tree);
        assert_ne!(layouts.active(new, ws), Some(migrated));

        layouts.remap_space(old, new);
        assert_eq!(layouts.active(new, ws), Some(migrated));
        assert_eq!(layouts.active(old, ws), None);
        assert_eq!(layouts.spaces().into_iter().collect::<Vec<_>>(), vec![new]);
    }
}
