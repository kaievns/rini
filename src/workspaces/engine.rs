use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::{DisplayUuid, SpaceId, WindowId, pid_t};
use rustc_hash::FxHashMap as HashMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{Direction, FloatingManager, LayoutId, ScrollingLayoutSystem, WorkspaceLayouts};
use crate::layout::WindowLayoutConstraints;
use crate::layout::domain::edge_drag::Drag;
use crate::layout::domain::scrolling::ResizeReport;
use crate::layout::domain::tile::Borders;
use crate::layout::settings::LayoutSettings;
use crate::windows::domain::info::AppInfo;
use crate::windows::domain::rules::AppRuleEngine;
use crate::windows::domain::state::WindowState;
use crate::workspaces::domain::app_rules::AppRuleOutcome;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::domain::virtual_workspace::{VirtualWorkspaceId, WorkspaceStore};
use crate::workspaces::domain::workspace_focus;
use crate::workspaces::{FloatingPositionStore, WindowStore};

mod affinity;
mod commands;
mod discovery;
mod frames;
mod launch;
mod moves;
mod navigation;
mod persistence;
mod rules;

use persistence::PersistenceState;
pub use rini_ipc::protocol::LayoutCommand;

use crate::workspaces::domain::command::EngineCommand;

/// One window as `WindowsOnScreenUpdated` describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct OnScreenEntry {
    pub wid: WindowId,
    pub title: Option<String>,
    pub ax_role: Option<String>,
    pub ax_subrole: Option<String>,
    pub is_modal: bool,
    pub is_resizable: bool,
    pub size: CGSize,
    pub min_size: Option<CGSize>,
    pub max_size: Option<CGSize>,
}

/// The entry for `wid`. A window the store no longer has is resizable, sized zero, untitled.
pub fn on_screen_entry(wid: WindowId, window: Option<&WindowState>) -> OnScreenEntry {
    OnScreenEntry {
        wid,
        title: window.map(|w| w.info.title.clone()),
        ax_role: window.and_then(|w| w.info.ax_role.clone()),
        ax_subrole: window.and_then(|w| w.info.ax_subrole.clone()),
        is_modal: window.is_some_and(|w| w.info.is_modal),
        is_resizable: window.is_none_or(|w| w.info.is_resizable),
        size: window.map_or(CGSize::new(0.0, 0.0), |w| w.frame_monotonic.size),
        min_size: window.and_then(|w| w.info.min_size),
        max_size: window.and_then(|w| w.info.max_size),
    }
}

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum LayoutEvent {
    WindowsOnScreenUpdated(SpaceId, pid_t, Vec<OnScreenEntry>, Option<AppInfo>),
    /// The complete cross-space discovery batch for one application has been applied.
    WindowDiscoveryCompleted(pid_t, Option<String>, Vec<SpaceId>),
    AppClosed(pid_t),
    WindowAdded(SpaceId, WindowId),
    WindowRemoved(WindowId),
    WindowRemovedPreserveFloating(WindowId),
    WindowFocused(SpaceId, WindowId),
    /// The window's frame changed size. `drag` is the mouse drag on its edges that did it, and `None`
    /// when the app resized the window itself.
    WindowResized {
        wid: WindowId,
        drag: Option<Drag>,
        new_frame: CGRect,
        screens: Vec<(SpaceId, CGRect, Option<DisplayUuid>)>,
    },
    /// The window's app zoomed it from its title bar, on the strip `space` is showing.
    WindowZoomed(SpaceId, WindowId),
    SpaceExposed(SpaceId, CGSize),
}

#[must_use]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventResponse {
    /// Whether handling the request changed layout-engine state.
    #[serde(default)]
    pub changed: bool,
    pub raise_windows: Vec<WindowId>,
    pub focus_window: Option<WindowId>,
    /// A command tried to go past an end: the strip's first/last column (`Left`/`Right`) or the
    /// workspace stack's top/bottom (`Up`/`Down`). Nothing moved; the reactor bounces the view
    /// so the stop reads as an edge rather than a dropped keypress.
    #[serde(default)]
    pub edge_hit: Option<Direction>,
    /// A window moved along its strip, changing places with the column that way. The reactor draws
    /// the two columns crossing. Not set for a move out of a stacked column, a vertical move, or a
    /// move onto another display.
    #[serde(default)]
    pub moved: Option<(WindowId, Direction)>,
}

#[must_use]
pub struct LayoutEventOutcome {
    pub response: EventResponse,
    pub app_rules: AppRuleOutcome,
}

impl std::ops::Deref for LayoutEventOutcome {
    type Target = EventResponse;

    fn deref(&self) -> &Self::Target { &self.response }
}

pub struct LayoutEngine {
    workspace_layouts: WorkspaceLayouts,
    floating: FloatingManager,
    floating_positions: FloatingPositionStore,
    app_rules: AppRuleEngine,
    focused_window: Option<WindowId>,
    window_layout_constraints: HashMap<WindowId, WindowLayoutConstraints>,
    /// The border each tiled window wears, in points, which the strip takes out of its slot.
    borders: Borders,
    virtual_workspace_manager: WorkspaceStore,
    layout_settings: LayoutSettings,
    /// Display UUIDs currently attached. Runtime only: it describes the machine right now, not the
    /// layout, and it is what the launch memory is keyed by.
    connected_displays: Vec<DisplayUuid>,
    persistence: PersistenceState,
    /// Set only while a saved-file startup restore is waiting for the first display snapshot.
    startup_restore_pending: bool,
}

pub struct WorkspaceLayoutQuerySnapshot {
    pub workspace_id: VirtualWorkspaceId,
    pub workspace_index: usize,
    pub is_active: bool,
    pub selected_window: Option<WindowId>,
    pub container_tree: rini_ipc::protocol::ContainerTreeNode,
}

impl LayoutEngine {
    pub fn focused_window(&self) -> Option<WindowId> { self.focused_window }

    /// Resolve an optional workspace index and snapshot its layout for read-only consumers.
    pub fn query_workspace_layout(
        &self,
        space: SpaceId,
        workspace_index: Option<usize>,
    ) -> Option<WorkspaceLayoutQuerySnapshot> {
        let workspaces = self.virtual_workspace_manager.existing_workspaces();
        let active = self.virtual_workspace_manager.active_workspace(space)?;
        let (workspace_index, workspace_id) = match workspace_index {
            Some(index) => (index, workspaces.get(index)?.0),
            None => (workspaces.iter().position(|(id, _)| *id == active)?, active),
        };
        let layout = self.workspace_layouts.active(space, workspace_id)?;
        let workspace = self.virtual_workspace_manager.workspace_info(workspace_id)?;
        let selected_window = workspace.layout_system().selected_window(layout);
        let container_tree = workspace.layout_system().container_tree(layout);

        Some(WorkspaceLayoutQuerySnapshot {
            workspace_id,
            workspace_index,
            is_active: workspace_id == active,
            selected_window,
            container_tree,
        })
    }

    /// Get the active workspace ID for a space, ensuring initialization.
    fn active_workspace_id(&self, space: SpaceId) -> Option<VirtualWorkspaceId> {
        self.virtual_workspace_manager.active_workspace(space)
    }

    /// Get mutable access to a workspace's layout system.
    fn workspace_tree_mut(&mut self, ws_id: VirtualWorkspaceId) -> &mut ScrollingLayoutSystem {
        self.virtual_workspace_manager.layout_system_mut(ws_id)
    }

    /// Get immutable access to a workspace's layout system.
    fn workspace_tree(&self, ws_id: VirtualWorkspaceId) -> &ScrollingLayoutSystem {
        self.virtual_workspace_manager.layout_system(ws_id)
    }

    /// Get the active workspace and layout for a space.
    fn workspace_and_layout(&self, space: SpaceId) -> Option<(VirtualWorkspaceId, LayoutId)> {
        let ws_id = self.active_workspace_id(space)?;
        let layout = self.workspace_layouts.active(space, ws_id)?;
        Some((ws_id, layout))
    }

    /// Every workspace laid out on `space` at `size`, each keeping a layout per size it has had.
    fn expose_layouts(&mut self, space: SpaceId, size: CGSize) {
        for (id, _) in self.virtual_workspace_manager_mut().list_workspaces(space) {
            let tree = self.virtual_workspace_manager.layout_system_mut(id);
            self.workspace_layouts.ensure_active_for_workspace(space, size, id, tree);
        }
    }

    fn response_for_raised_windows(raise_windows: Vec<WindowId>) -> EventResponse {
        if raise_windows.is_empty() {
            EventResponse::default()
        } else {
            EventResponse {
                changed: true,
                raise_windows,
                focus_window: None,
                edge_hit: None,
                moved: None,
            }
        }
    }
}

impl LayoutEngine {
    /// The border each tiled window wears from the next layout pass: each window is laid out inside
    /// its slot by its width on every side. Whether they differ from the ones it had.
    pub fn set_borders(&mut self, borders: Borders) -> bool {
        let changed = self.borders != borders;
        self.borders = borders;
        changed
    }

    pub fn set_layout_settings(&mut self, settings: &LayoutSettings) {
        self.layout_settings = settings.clone();

        for (_, system) in self.virtual_workspace_manager.layout_systems_mut() {
            system.update_settings(&settings.scrolling);
        }
    }

    pub fn update_virtual_workspace_settings(
        &mut self,
        settings: &crate::workspaces::settings::VirtualWorkspaceSettings,
    ) {
        self.app_rules = AppRuleEngine::new(&settings.app_rules);
        self.virtual_workspace_manager.update_settings(settings, &self.layout_settings);
    }

    /// Where the active strip's first column starts on `space`, against the tiling area's left edge.
    ///
    /// `None` when the space has no active workspace or its strip has not been laid out. Callers compare
    /// it against the value they last saw to learn how far the strip moved on screen, which is exact and
    /// needs no reference to any window's real frame.
    pub fn strip_origin(&self, space: SpaceId) -> Option<f64> {
        let (ws_id, layout) = self.workspace_and_layout(space)?;
        let system = self.workspace_tree(ws_id);
        system.strip_origin(layout)
    }

    fn active_floating_windows_in_workspace(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
    ) -> Vec<WindowId> {
        self.floating
            .active_flat(space)
            .into_iter()
            .filter(|wid| self.is_window_in_active_workspace(window_store, space, *wid))
            .collect()
    }

    fn preferred_focus_for_workspace(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        preferred_focus_window: Option<WindowId>,
    ) -> Option<WindowId> {
        let in_workspace = |wid: &WindowId| {
            self.virtual_workspace_manager.workspace_for_window(window_store, space, *wid)
                == Some(workspace_id)
        };
        let layout = self.workspace_layouts.active(space, workspace_id);
        let floating = self.active_floating_windows_in_workspace(window_store, space);

        workspace_focus::preferred(&workspace_focus::FocusCandidates {
            requested: preferred_focus_window.filter(in_workspace),
            last_focused: self
                .virtual_workspace_manager
                .last_focused_window(space, workspace_id)
                .filter(in_workspace),
            selected: layout
                .and_then(|layout| self.workspace_tree(workspace_id).selected_window(layout))
                .filter(in_workspace),
            first_visible: layout.and_then(|layout| {
                self.workspace_tree(workspace_id)
                    .visible_windows_in_layout(layout)
                    .into_iter()
                    .find(in_workspace)
            }),
            last_floating: self.floating.last_focus().filter(|wid| floating.contains(wid)),
            first_floating: floating.first().copied(),
        })
    }

    /// A workspace's active window, as niri has one: the window focus lands on when the workspace
    /// takes it, which is the one it was on last.
    pub fn active_window(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
    ) -> Option<WindowId> {
        self.preferred_focus_for_workspace(window_store, space, workspace_id, None)
    }

    pub fn commit_workspace_focus(
        &mut self,
        window_store: &mut WindowStore,
        space: SpaceId,
        focus_window: Option<WindowId>,
    ) {
        let Some(workspace_id) = self.virtual_workspace_manager.active_workspace(space) else {
            self.focused_window = None;
            return;
        };

        let focus_window = focus_window.filter(|wid| {
            self.virtual_workspace_manager.workspace_for_window(window_store, space, *wid)
                == Some(workspace_id)
        });

        if let Some(wid) = focus_window {
            self.focused_window = Some(wid);
            self.virtual_workspace_manager
                .set_last_focused_window(space, workspace_id, Some(wid));
            if self.floating.is_floating(wid) {
                self.floating.set_last_focus(Some(wid));
            } else if let Some(layout) = self.workspace_layouts.active(space, workspace_id) {
                let _ = self.workspace_tree_mut(workspace_id).select_window(layout, wid);
            }
        } else {
            // Focus memory is per (workspace, display) and is cleaned up on close; clearing it here
            // runs on every ordinary focus change and lands the return on the first column.
            self.focused_window = None;
        }
    }

    fn activate_workspace(
        &mut self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        preferred_focus_window: Option<WindowId>,
    ) -> EventResponse {
        self.virtual_workspace_manager.set_active_workspace(space, workspace_id);
        self.update_active_floating_windows(window_store, space);

        EventResponse {
            changed: true,
            focus_window: self.preferred_focus_for_workspace(
                window_store,
                space,
                workspace_id,
                preferred_focus_window,
            ),
            raise_windows: vec![],
            edge_hit: None,
            moved: None,
        }
    }

    fn switch_to_workspace(
        &mut self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_index: usize,
        preferred_focus_window: Option<WindowId>,
    ) -> EventResponse {
        let workspaces = self.virtual_workspace_manager_mut().list_workspaces(space);
        if let Some((workspace_id, _)) = workspaces.get(workspace_index) {
            let workspace_id = *workspace_id;
            if self.virtual_workspace_manager.active_workspace(space) == Some(workspace_id) {
                return EventResponse::default();
            }
            return self.activate_workspace(
                window_store,
                space,
                workspace_id,
                preferred_focus_window,
            );
        }
        EventResponse::default()
    }

    /// The active workspace's tiled windows on `space`, in layout order.
    ///
    /// `WindowStore::workspace_windows` sorts by `WindowId`, which is unrelated to what the
    /// user sees. Strip order has to come from the layout tree, which is the thing that
    /// actually defines left-to-right position.
    pub fn ordered_windows_in_active_workspace(&self, space: SpaceId) -> Vec<WindowId> {
        let Some((workspace_id, layout)) = self.workspace_and_layout(space) else {
            return Vec::new();
        };
        self.workspace_tree(workspace_id).all_windows_in_layout(layout)
    }

    /// Move all per-space layout state from `old_space` to `new_space`.
    pub fn remap_space(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        old_space: SpaceId,
        new_space: SpaceId,
    ) {
        if old_space == new_space {
            return;
        }

        self.workspace_layouts.remap_space(old_space, new_space);
        self.floating.remap_space(old_space, new_space);
        self.floating_positions.remap_space(old_space, new_space);
        self.virtual_workspace_manager.remap_space(window_store, old_space, new_space);
        memory.affinity.remap_space(old_space, new_space);
    }

    pub fn new(
        virtual_workspace_config: &crate::workspaces::settings::VirtualWorkspaceSettings,
        layout_settings: &LayoutSettings,
    ) -> Self {
        let virtual_workspace_manager =
            WorkspaceStore::new_with_config(virtual_workspace_config, layout_settings);

        LayoutEngine {
            workspace_layouts: WorkspaceLayouts::default(),
            floating: FloatingManager::new(),
            floating_positions: FloatingPositionStore::default(),
            app_rules: AppRuleEngine::new(&virtual_workspace_config.app_rules),
            focused_window: None,
            window_layout_constraints: HashMap::default(),
            borders: Borders::default(),
            virtual_workspace_manager,
            layout_settings: layout_settings.clone(),
            connected_displays: Vec::new(),
            persistence: PersistenceState::default(),
            startup_restore_pending: false,
        }
    }

    pub fn debug_tree(&self, space: SpaceId) { self.debug_tree_desc(space, ""); }

    pub fn debug_tree_desc(&self, space: SpaceId, desc: &'static str) {
        if let Some(workspace_id) = self.virtual_workspace_manager.active_workspace(space) {
            if let Some(layout) = self.workspace_layouts.active(space, workspace_id) {
                debug!(
                    "Tree {desc}\n{}",
                    self.workspace_tree(workspace_id).draw_tree(layout).trim()
                );
            } else {
                debug!("No layout for workspace {workspace_id:?} on space {space:?}");
            }
        } else {
            debug!("No active workspace for space {space:?}");
        }
    }

    pub fn handle_event(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        event: LayoutEvent,
    ) -> LayoutEventOutcome {
        let mut app_rules = AppRuleOutcome::default();
        let response = self.handle_event_inner(window_store, memory, event, &mut app_rules);
        LayoutEventOutcome { response, app_rules }
    }

    fn handle_event_inner(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        event: LayoutEvent,
        app_rule_outcome: &mut AppRuleOutcome,
    ) -> EventResponse {
        debug!(?event);
        match event {
            LayoutEvent::SpaceExposed(space, size) => {
                self.debug_tree(space);
                self.expose_layouts(space, size);
            }
            LayoutEvent::WindowsOnScreenUpdated(space, pid, windows_with_titles, app_info) => {
                return self.on_windows_on_screen_updated(
                    window_store,
                    memory,
                    space,
                    pid,
                    windows_with_titles,
                    app_info,
                    app_rule_outcome,
                );
            }
            LayoutEvent::WindowDiscoveryCompleted(pid, app_id, discovered_spaces) => {
                let ignored = self.discard_unmatched_candidates_for_app(
                    pid,
                    app_id.as_deref(),
                    &discovered_spaces,
                );
                if ignored > 0 {
                    tracing::info!(
                        pid,
                        native_spaces = discovered_spaces.len(),
                        windows_ignored = ignored,
                        "Ignored unmatched persisted windows after application discovery"
                    );
                }
            }
            LayoutEvent::AppClosed(pid) => self.remove_app(window_store, memory, pid),
            LayoutEvent::WindowAdded(space, wid) => {
                self.debug_tree(space);
                self.add_window_to_layout(window_store, memory, space, wid);
            }
            LayoutEvent::WindowRemoved(wid) => {
                self.remove_window_internal(window_store, memory, wid, false);
            }
            LayoutEvent::WindowRemovedPreserveFloating(wid) => {
                self.remove_window_internal(window_store, memory, wid, true);
            }
            LayoutEvent::WindowFocused(space, wid) => return self.on_window_focused(space, wid),
            LayoutEvent::WindowResized { wid, drag, new_frame, screens } => {
                self.on_window_resized(wid, drag, new_frame, screens);
            }
            LayoutEvent::WindowZoomed(space, wid) => {
                let Some((ws_id, layout)) = self.workspace_and_layout(space) else {
                    return EventResponse::default();
                };
                return self.toggle_full_width(memory, space, ws_id, layout, wid);
            }
        }
        EventResponse::default()
    }

    /// Every window of `pid` out of every strip and the floating layer, and forgotten.
    fn remove_app(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        pid: pid_t,
    ) {
        for (_, system) in self.virtual_workspace_manager.layout_systems_mut() {
            system.remove_windows_for_app(pid);
        }
        self.floating.remove_all_for_pid(pid);
        self.window_layout_constraints.retain(|wid, _| wid.pid != pid);
        self.forget_persisted_app(memory, pid);

        self.virtual_workspace_manager.remove_windows_for_app(window_store, pid);
        self.floating_positions.remove_app(pid);
    }

    /// `wid` took focus: the floating layer's focus, or its strip's selection and the workspace's
    /// focus memory when it is a column of the strip `space` shows. A tiled window that is not is
    /// ignored.
    fn on_window_focused(&mut self, space: SpaceId, wid: WindowId) -> EventResponse {
        if self.floating.is_floating(wid) {
            self.focused_window = Some(wid);
            self.floating.set_last_focus(Some(wid));
            return EventResponse::default();
        }
        let Some((ws_id, layout)) = self.workspace_and_layout(space) else {
            warn!(
                "No active workspace/layout for focused window {:?} on space {:?}",
                wid, space
            );
            return EventResponse::default();
        };
        if !self.workspace_tree(ws_id).contains_window(layout, wid) {
            warn!(
                "WindowFocused ignored: wid={:?} not in active layout for space {:?}",
                wid, space
            );
            return EventResponse::default();
        }
        self.focused_window = Some(wid);
        let _ = self.workspace_tree_mut(ws_id).select_window(layout, wid);
        self.virtual_workspace_manager.set_last_focused_window(space, ws_id, Some(wid));
        EventResponse {
            changed: true,
            ..EventResponse::default()
        }
    }

    /// `wid` resized to `new_frame`, by `drag` or by its app, applied to the strip of each screen
    /// it was laid out on.
    fn on_window_resized(
        &mut self,
        wid: WindowId,
        drag: Option<Drag>,
        new_frame: CGRect,
        screens: Vec<(SpaceId, CGRect, Option<DisplayUuid>)>,
    ) {
        for (space, screen_frame, display_uuid) in screens {
            let Some((ws_id, layout)) = self.workspace_and_layout(space) else {
                debug!(
                    "No active workspace/layout for resized window {:?} on space {:?}; skipping",
                    wid, space
                );
                continue;
            };
            let gaps = self.layout_settings.gaps.effective_for_display(display_uuid.as_ref());
            let border = self.borders.of(wid);
            self.workspace_tree_mut(ws_id).on_window_resized(
                layout,
                ResizeReport {
                    window: wid,
                    drag,
                    frame: new_frame,
                    border,
                },
                screen_frame,
                &gaps,
            );

            self.workspace_layouts.mark_last_saved(space, ws_id, layout);
        }
    }

    /// What the tree looks like as a command arrives. Only ever read from a log.
    fn debug_command_tree(&self, space: Option<SpaceId>) {
        if let Some(space) = space {
            if let Some(ws_id) = self.virtual_workspace_manager.active_workspace(space) {
                if let Some(layout) = self.workspace_layouts.active(space, ws_id) {
                    debug!("Tree:\n{}", self.workspace_tree(ws_id).draw_tree(layout).trim());
                    debug!(selection_window = ?self.workspace_tree(ws_id).selected_window(layout));
                } else {
                    debug!("No active layout for workspace {:?} on space {:?}", ws_id, space);
                }
            } else {
                debug!("No active workspace for space {:?}", space);
            }
        }
    }

    /// Maximizes a FLOATING window to the tiling area, or takes it back out. Its pre-maximize frame
    /// is kept in `floating_positions`, since a floating window has no column to fall back to.
    ///
    /// A tiled window does not come here: its own arm in `handle_floating_command` handles it, which
    /// is why the caller tests `is_floating` before calling rather than this deciding.
    fn toggle_floating_maximized(
        &mut self,
        window_store: &WindowStore,
        space: Option<SpaceId>,
    ) -> EventResponse {
        let Some(wid) = self.focused_window else {
            return EventResponse::default();
        };
        if self.floating.is_maximized(wid) {
            self.floating.set_maximized(wid, false);
        } else {
            // The frame to come back to, saved on the way in.
            if let Some(space) = space {
                let ws = self
                    .virtual_workspace_manager
                    .workspace_for_window(window_store, space, wid)
                    .or_else(|| self.virtual_workspace_manager.active_workspace(space));
                if let (Some(ws), Some(frame)) =
                    (ws, window_store.window(wid).map(|w| w.frame_monotonic))
                {
                    self.floating_positions.store(space, ws, wid, frame);
                }
            }
            self.floating.set_maximized(wid, true);
        }
        EventResponse {
            changed: true,
            raise_windows: vec![wid],
            focus_window: Some(wid),
            edge_hit: None,
            moved: None,
        }
    }

    /// The one entry point for a command. `space` is the space it acts on; a workspace command with
    /// none does nothing, and the rest resolve their target against it.
    pub fn handle_command(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        space: Option<SpaceId>,
        visible_spaces: &[SpaceId],
        visible_space_centers: &HashMap<SpaceId, CGPoint>,
        command: EngineCommand,
    ) -> EventResponse {
        self.debug_command_tree(space);
        let is_floating = self.focused_window.is_some_and(|focus| self.floating.is_floating(focus));
        debug!(?self.focused_window, last_floating_focus=?self.floating.last_focus(), ?is_floating);

        match command {
            EngineCommand::Workspace(command) => match space {
                Some(space) => self.handle_workspace_command(window_store, memory, space, command),
                None => EventResponse::default(),
            },
            EngineCommand::Floating(command) => {
                self.handle_floating_command(window_store, memory, space, is_floating, command)
            }
            EngineCommand::Focus(command) => self.with_target(space, |engine, target| {
                engine.handle_focus_command(
                    window_store,
                    visible_spaces,
                    visible_space_centers,
                    target,
                    is_floating,
                    command,
                )
            }),
            EngineCommand::Arrange(command) => self.with_target(space, |engine, target| {
                engine.handle_arrange_command(
                    window_store,
                    visible_spaces,
                    visible_space_centers,
                    target,
                    command,
                )
            }),
            EngineCommand::Resize(command) => self.with_target(space, |engine, target| {
                engine.handle_resize_command(memory, target, is_floating, command)
            }),
            EngineCommand::Strip(command) => self.with_target(space, |engine, target| {
                engine.handle_strip_command(target, command)
            }),
        }
    }

    /// `act` on the space, active workspace and active layout `space` resolves to, or nothing when
    /// it resolves to none.
    fn with_target(
        &mut self,
        space: Option<SpaceId>,
        act: impl FnOnce(&mut Self, commands::CommandTarget) -> EventResponse,
    ) -> EventResponse {
        match commands::resolve_target(
            space,
            |space| self.virtual_workspace_manager.active_workspace(space),
            |space, workspace| self.workspace_layouts.active(space, workspace),
        ) {
            Ok(target) => act(self, target),
            Err(reason) => {
                if reason.is_worth_warning_about() {
                    warn!(?space, ?reason, "layout command has nothing to act on");
                }
                EventResponse::default()
            }
        }
    }

    pub fn switch_to_workspace_with_focus(
        &mut self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_index: usize,
        focus_window: WindowId,
    ) -> EventResponse {
        self.switch_to_workspace(window_store, space, workspace_index, Some(focus_window))
    }

    pub fn virtual_workspace_manager(&self) -> &WorkspaceStore { &self.virtual_workspace_manager }

    pub fn virtual_workspace_manager_mut(&mut self) -> &mut WorkspaceStore {
        &mut self.virtual_workspace_manager
    }

    pub fn active_workspace(
        &self,
        space: SpaceId,
    ) -> Option<crate::workspaces::VirtualWorkspaceId> {
        self.virtual_workspace_manager.active_workspace(space)
    }

    pub fn ensure_active_workspace_info(
        &mut self,
        space: SpaceId,
    ) -> Option<(crate::workspaces::VirtualWorkspaceId, String)> {
        if let Some(workspace_id) = self.virtual_workspace_manager.active_workspace(space) {
            let workspace_name = self
                .workspace_name(workspace_id)
                .unwrap_or_else(|| format!("Workspace {:?}", workspace_id));
            return Some((workspace_id, workspace_name));
        }

        let first_workspace = self
            .virtual_workspace_manager
            .list_workspaces(space)
            .first()
            .map(|(workspace_id, _)| *workspace_id)?;

        self.virtual_workspace_manager.set_active_workspace(space, first_workspace);

        let workspace_name = self
            .workspace_name(first_workspace)
            .unwrap_or_else(|| format!("Workspace {:?}", first_workspace));

        Some((first_workspace, workspace_name))
    }

    pub fn active_workspace_idx(&self, space: SpaceId) -> Option<u64> {
        self.virtual_workspace_manager.active_workspace_idx(space)
    }

    pub fn workspace_name(
        &self,
        workspace_id: crate::workspaces::VirtualWorkspaceId,
    ) -> Option<String> {
        self.virtual_workspace_manager
            .workspace_info(workspace_id)
            .map(|ws| ws.name().to_owned())
    }

    pub fn windows_in_active_workspace(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
    ) -> Vec<WindowId> {
        self.virtual_workspace_manager.windows_in_active_workspace(window_store, space)
    }

    /// Whether `window` is at full width, or `None` when it is not a column of the strip `space` is
    /// showing.
    pub fn full_width_of_column(&self, space: SpaceId, window: WindowId) -> Option<bool> {
        let (ws_id, layout) = self.workspace_and_layout(space)?;
        let tree = self.workspace_tree(ws_id);
        tree.contains_window(layout, window)
            .then(|| tree.is_window_full_width(layout, window))
    }

    pub fn is_window_floating(&self, window_id: WindowId) -> bool {
        self.floating.is_floating(window_id)
    }

    pub fn store_floating_position(
        &mut self,
        space: SpaceId,
        workspace: VirtualWorkspaceId,
        window: WindowId,
        frame: CGRect,
    ) {
        self.floating_positions.store(space, workspace, window, frame);
    }

    #[cfg(test)]
    pub fn get_floating_position(
        &self,
        space: SpaceId,
        workspace: VirtualWorkspaceId,
        window: WindowId,
    ) -> Option<CGRect> {
        self.floating_positions.get(space, workspace, window)
    }

    pub fn remove_floating_position(&mut self, window: WindowId) {
        self.floating_positions.remove_window(window);
    }

    pub fn rekey_window_identity(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        from: WindowId,
        to: WindowId,
    ) {
        window_store.transfer_persistent_window_metadata(from, to);
        self.transfer_persistent_window_identity(memory, from, to);
    }

    pub fn transfer_persistent_window_identity(
        &mut self,
        memory: &mut DisplayMemory,
        from: WindowId,
        to: WindowId,
    ) {
        if from == to {
            return;
        }

        // A live `to` identity can already be provisionally present when a saved `from` identity
        // is matched. Remove that projection before replacement; LayoutSystem::replace_window is
        // not required to deduplicate and otherwise the same window can survive in two workspaces.
        self.remove_window_from_all_tiling_trees(to);
        for (_, system) in self.virtual_workspace_manager.layout_systems_mut() {
            system.replace_window(from, to);
        }
        self.virtual_workspace_manager.transfer_window_identity(from, to);
        self.floating_positions.transfer_window_identity(from, to);
        self.floating.transfer_window_identity(from, to);
        self.transfer_persisted_window_identity(memory, from, to);
        if let Some(constraints) = self.window_layout_constraints.remove(&from) {
            self.window_layout_constraints.insert(to, constraints);
        }
        if self.focused_window == Some(from) {
            self.focused_window = Some(to);
        }
    }

    fn update_active_floating_windows(&mut self, window_store: &WindowStore, space: SpaceId) {
        let windows_in_workspace =
            self.virtual_workspace_manager.windows_in_active_workspace(window_store, space);
        self.floating.rebuild_active_for_workspace(space, windows_in_workspace);
    }

    pub fn store_floating_window_positions(
        &mut self,
        space: SpaceId,
        floating_positions: &[(WindowId, CGRect)],
    ) {
        if let Some(workspace) = self.active_workspace(space) {
            for &(window, frame) in floating_positions {
                self.floating_positions.store(space, workspace, window, frame);
            }
        }
    }

    pub fn is_window_in_active_workspace(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        window_id: WindowId,
    ) -> bool {
        self.virtual_workspace_manager
            .is_window_in_active_workspace(window_store, space, window_id)
    }
}

#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;

    use objc2_core_foundation::{CGPoint, CGSize};
    use rini_ipc::protocol::{RelativeWorkspace, WorkspaceSelector};
    use rustc_hash::FxHashMap as HashMap;

    use super::*;
    use crate::layout::ResizeOrientation;
    use crate::layout::domain::area::compute_tiling_area;
    use crate::layout::settings::LayoutSettings;
    use crate::windows::domain::rules::{AppRuleSize, FloatingPosition, RuleMatch, WindowRule};
    use crate::workspaces::settings::VirtualWorkspaceSettings;

    impl OnScreenEntry {
        /// A resizable, untitled, non-modal window with no size and no limits.
        fn plain(wid: WindowId) -> Self { on_screen_entry(wid, None) }

        fn sized(self, width: f64, height: f64) -> Self {
            Self {
                size: CGSize::new(width, height),
                ..self
            }
        }

        fn modal(self, is_modal: bool) -> Self { Self { is_modal, ..self } }
    }

    /// A `LayoutEngine` with the window store and display memory it is driven with, the screen of
    /// each space it exposed, and the spaces and centres its commands see.
    struct EngineFixture {
        engine: LayoutEngine,
        windows: WindowStore,
        memory: DisplayMemory,
        screens: HashMap<SpaceId, CGRect>,
        visible: Vec<SpaceId>,
        centers: HashMap<SpaceId, CGPoint>,
    }

    impl EngineFixture {
        fn new() -> Self {
            Self::with(&VirtualWorkspaceSettings::default(), &LayoutSettings::default())
        }

        fn with(workspaces: &VirtualWorkspaceSettings, layout: &LayoutSettings) -> Self {
            Self {
                engine: LayoutEngine::new(workspaces, layout),
                windows: WindowStore::default(),
                memory: DisplayMemory::default(),
                screens: HashMap::default(),
                visible: Vec::new(),
                centers: HashMap::default(),
            }
        }

        fn event(&mut self, event: LayoutEvent) -> LayoutEventOutcome {
            self.engine.handle_event(&mut self.windows, &mut self.memory, event)
        }

        /// `space` exposed at `size`, its screen at the origin, and visible to commands.
        fn expose(&mut self, space: SpaceId, size: CGSize) {
            self.screens.insert(space, CGRect::new(CGPoint::new(0.0, 0.0), size));
            if !self.visible.contains(&space) {
                self.visible.push(space);
            }
            let _ = self.event(LayoutEvent::SpaceExposed(space, size));
        }

        /// The app owning the first of `windows` reports all of them on `space`.
        fn open(&mut self, space: SpaceId, windows: Vec<OnScreenEntry>) -> LayoutEventOutcome {
            self.open_app(space, windows, None)
        }

        fn open_app(
            &mut self,
            space: SpaceId,
            windows: Vec<OnScreenEntry>,
            app: Option<AppInfo>,
        ) -> LayoutEventOutcome {
            let pid = windows[0].wid.pid;
            self.event(LayoutEvent::WindowsOnScreenUpdated(space, pid, windows, app))
        }

        fn command(&mut self, space: SpaceId, command: impl Into<EngineCommand>) -> EventResponse {
            self.engine.handle_command(
                &mut self.windows,
                &mut self.memory,
                Some(space),
                &self.visible,
                &self.centers,
                command.into(),
            )
        }

        /// Every frame the application lays out on `space`'s screen, in window order.
        fn frames(&mut self, space: SpaceId) -> Vec<(WindowId, CGRect)> {
            let screen = self.screens[&space];
            let gaps = self.engine.layout_settings.gaps.effective_for_display(None);
            let mut frames = self.engine.calculate_layout_with_virtual_workspaces(
                &self.windows,
                space,
                screen,
                &gaps,
                |_| None,
                &[screen],
            );
            frames.sort_by_key(|&(wid, _)| wid);
            frames
        }

        fn frame(&mut self, space: SpaceId, wid: WindowId) -> CGRect {
            let frames = self.frames(space);
            frames.into_iter().find(|&(window, _)| window == wid).expect("laid out").1
        }

        fn workspaces(&mut self, space: SpaceId) -> Vec<VirtualWorkspaceId> {
            let workspaces = self.engine.virtual_workspace_manager_mut().list_workspaces(space);
            workspaces.into_iter().map(|(id, _)| id).collect()
        }

        fn workspace_of(&self, space: SpaceId, wid: WindowId) -> Option<VirtualWorkspaceId> {
            self.engine
                .virtual_workspace_manager
                .workspace_for_window(&self.windows, space, wid)
        }

        fn show(&mut self, space: SpaceId, workspace: VirtualWorkspaceId) {
            let shown = self
                .engine
                .virtual_workspace_manager_mut()
                .set_active_workspace(space, workspace);
            assert!(shown);
        }

        /// `wid` out of its strip and into the floating layer on `space`.
        fn float(&mut self, space: SpaceId, wid: WindowId) {
            self.engine.remove_window_from_all_tiling_trees(wid);
            self.engine.floating.add_floating(wid);
            self.engine.floating.add_active(space, wid.pid, wid);
        }
    }

    fn app(bundle_id: &str) -> Option<AppInfo> {
        Some(AppInfo {
            bundle_id: Some(bundle_id.into()),
            localized_name: None,
        })
    }

    fn widen() -> LayoutCommand {
        LayoutCommand::ResizeWindowBy {
            amount: 0.2,
            orientation: ResizeOrientation::Horizontal,
        }
    }

    fn move_window(wid: WindowId, workspace: WorkspaceSelector, follow: bool) -> LayoutCommand {
        LayoutCommand::MoveWindowToWorkspace {
            workspace,
            follow,
            window_id: Some(wid.idx.get()),
        }
    }

    fn build_three_spaces() -> (
        Vec<SpaceId>,
        HashMap<SpaceId, CGPoint>,
        SpaceId,
        SpaceId,
        SpaceId,
    ) {
        let left = SpaceId::new(1);
        let right = SpaceId::new(2);
        let middle = SpaceId::new(3);

        let mut centers = HashMap::default();
        centers.insert(left, CGPoint::new(0.0, 0.0));
        centers.insert(right, CGPoint::new(4000.0, 0.0));
        centers.insert(middle, CGPoint::new(2000.0, 0.0));

        (vec![left, right, middle], centers, left, middle, right)
    }

    /// Two displays, `first` and `second`, one window on each.
    struct TwoDisplays {
        fx: EngineFixture,
        first: SpaceId,
        second: SpaceId,
        on_first: WindowId,
        on_second: WindowId,
    }

    /// Two displays, one window each, side by side. The side-by-side arrangement is what makes a
    /// horizontal command have somewhere it COULD go, which is the whole point of the test.
    fn two_displays_side_by_side(first_space: u64) -> TwoDisplays {
        two_displays(first_space, CGPoint::new(1000.0, 0.0))
    }

    /// The same, stacked vertically, so up and down have somewhere to go and left and right do not.
    ///
    /// The second display is BELOW the first, which is a NEGATIVE y offset: space centres are in the
    /// window server's coordinates, where y grows upward, and `directional_delta` reads them that way.
    fn two_displays_stacked(first_space: u64) -> TwoDisplays {
        two_displays(first_space, CGPoint::new(0.0, -800.0))
    }

    fn two_displays(first_space: u64, second_centre: CGPoint) -> TwoDisplays {
        let mut fx = EngineFixture::new();
        let (first, second) = (SpaceId::new(first_space), SpaceId::new(first_space + 1));
        let (on_first, on_second) = (WindowId::new(73, 1), WindowId::new(73, 2));
        for (space, wid) in [(first, on_first), (second, on_second)] {
            fx.expose(space, CGSize::new(1000.0, 800.0));
            let _ = fx.open(space, vec![OnScreenEntry::plain(wid)]);
        }
        fx.centers.insert(first, CGPoint::new(0.0, 0.0));
        fx.centers.insert(second, second_centre);
        TwoDisplays {
            fx,
            first,
            second,
            on_first,
            on_second,
        }
    }

    #[test]
    fn next_space_for_direction_respects_physical_layout() {
        let engine = EngineFixture::new().engine;
        let (visible_spaces, centers, left, middle, right) = build_three_spaces();

        assert_eq!(
            engine.next_space_for_direction(middle, Direction::Right, &visible_spaces, &centers),
            Some(right)
        );
        assert_eq!(
            engine.next_space_for_direction(middle, Direction::Left, &visible_spaces, &centers),
            Some(left)
        );
        assert_eq!(
            engine.next_space_for_direction(middle, Direction::Up, &visible_spaces, &centers),
            None
        );
    }

    #[test]
    fn handle_command_does_not_panic_before_layout_initialization() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(42);
        fx.visible.push(space);

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            fx.command(space, LayoutCommand::NextWindow)
        }));

        assert!(
            result.is_ok(),
            "handle_command should not panic before SpaceExposed"
        );
    }

    /// The reference config's modal rule: a modal window reported alongside a plain one floats, and
    /// the plain one takes a column.
    #[test]
    fn under_the_modal_rule_a_modal_window_floats_and_a_plain_one_is_tiled() {
        let modal_floats = WindowRule {
            open_floating: Some(true),
            ..WindowRule::when(RuleMatch {
                is_modal: Some(true),
                ..RuleMatch::default()
            })
        };
        let settings = VirtualWorkspaceSettings {
            app_rules: vec![modal_floats],
            ..VirtualWorkspaceSettings::default()
        };
        let space = SpaceId::new(91);
        let screen = CGSize::new(1200.0, 800.0);
        let (plain, modal) = (WindowId::new(7, 1), WindowId::new(7, 2));
        let info = |wid, is_modal| OnScreenEntry::plain(wid).sized(300.0, 200.0).modal(is_modal);

        let mut fx = EngineFixture::with(&settings, &LayoutSettings::default());
        fx.expose(space, screen);
        let windows = vec![info(plain, false), info(modal, true)];
        let _ = fx.open_app(space, windows, app("com.example.Tool"));
        assert!(fx.engine.is_window_floating(modal), "the modal floats");
        assert!(!fx.engine.is_window_floating(plain), "the app window is tiled");

        let mut fx = EngineFixture::new();
        fx.expose(space, screen);
        let _ = fx.open(space, vec![info(modal, true)]);
        assert!(
            !fx.engine.is_window_floating(modal),
            "with no rule a modal is tiled"
        );
    }

    #[test]
    fn floating_app_rule_emits_one_shot_placement_and_switches_focus_workspace() {
        let position = FloatingPosition {
            x: 40.0,
            y: 30.0,
            ..Default::default()
        };
        let settings = VirtualWorkspaceSettings {
            app_rules: vec![WindowRule {
                open_on_workspace: Some("Development".into()),
                open_floating: Some(true),
                default_floating_position: Some(position),
                default_column_width: Some(640.0),
                default_window_height: Some(480.0),
                open_focused: Some(true),
                ..WindowRule::when(RuleMatch::app("com.example.Tool"))
            }],
            ..VirtualWorkspaceSettings::default()
        };
        let mut fx = EngineFixture::with(&settings, &LayoutSettings::default());
        let space = SpaceId::new(90);
        let window = WindowId::new(7, 1);
        let screen = CGSize::new(1200.0, 800.0);
        fx.expose(space, screen);

        let entry = OnScreenEntry::plain(window).sized(300.0, 200.0);
        let outcome = fx.open_app(space, vec![entry], app("com.example.Tool"));

        let (placement, resizes, focus) = outcome.app_rules.into_parts();
        assert!(resizes.is_empty());
        assert_eq!(placement.len(), 1);
        assert_eq!(placement[0].window, window);
        assert_eq!(placement[0].position, Some(position));
        let size = AppRuleSize { w: Some(640.0), h: Some(480.0) };
        assert_eq!(placement[0].size, Some(size));
        assert_eq!(
            placement[0].resolve_frame(
                CGRect::new(CGPoint::new(10.0, 20.0), CGSize::new(300.0, 200.0)),
                CGRect::new(CGPoint::new(1000.0, 50.0), screen),
            ),
            CGRect::new(CGPoint::new(1040.0, 80.0), CGSize::new(640.0, 480.0))
        );
        // `changed` because this discovery is where the window's size limits were learned. The
        // fixture window is locked to 300x200, and a layout computed before that was known is the
        // clipping this reports.
        let changed = EventResponse {
            changed: true,
            ..EventResponse::default()
        };
        assert_eq!(outcome.response, changed);
        let focus = focus.expect("focus rule should request a workspace switch");
        assert_eq!(
            (focus.window, focus.space, focus.workspace_index),
            (window, space, 1)
        );
        let response = fx.engine.switch_to_workspace_with_focus(
            &fx.windows,
            focus.space,
            focus.workspace_index,
            focus.window,
        );
        assert_eq!(response.focus_window, Some(window));
        assert!(response.changed);
        assert_eq!(fx.engine.active_workspace(space), Some(fx.workspaces(space)[1]));
        assert!(fx.engine.is_window_floating(window));
    }

    #[test]
    fn tiled_app_rule_size_sets_scrolling_column_width() {
        let settings = VirtualWorkspaceSettings {
            app_rules: vec![WindowRule {
                default_column_width: Some(234.0),
                ..WindowRule::when(RuleMatch::app("com.example.Editor"))
            }],
            ..VirtualWorkspaceSettings::default()
        };
        let mut layout_settings = LayoutSettings::default();
        layout_settings.scrolling.min_column_width_ratio = 0.1;
        let mut fx = EngineFixture::with(&settings, &layout_settings);
        let space = SpaceId::new(91);
        let window = WindowId::new(8, 1);
        fx.expose(space, CGSize::new(1200.0, 800.0));
        let screen = fx.screens[&space];
        let entry = OnScreenEntry::plain(window).sized(500.0, 500.0);
        let outcome = fx.open_app(space, vec![entry], app("com.example.Editor"));

        let (placements, resizes, workspace_focus) = outcome.app_rules.into_parts();
        assert_eq!(placements, Vec::new());
        assert_eq!(workspace_focus, None);
        assert_eq!(resizes.len(), 1);
        assert_eq!((resizes[0].size.w, resizes[0].size.h), (Some(234.0), None));
        let old_frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(500.0, 500.0));
        let new_frame = CGRect::new(old_frame.origin, CGSize::new(234.0, 500.0));
        fx.engine.apply_app_rule_resize(resizes[0], new_frame, screen, None);
        assert_eq!(fx.frame(space, window).size.width, 234.0);

        let user_frame = CGRect::new(new_frame.origin, CGSize::new(400.0, new_frame.size.height));
        let _ = fx.event(LayoutEvent::WindowResized {
            wid: window,
            drag: Some(Drag::report(new_frame, user_frame, None)),
            new_frame: user_frame,
            screens: vec![(space, screen, None)],
        });
        assert_eq!(fx.frame(space, window).size.width, 400.0);
    }

    #[test]
    fn tiled_membership_sync_does_not_rebalance_other_spaces() {
        let mut fx = EngineFixture::new();
        let (space_a, space_b) = (SpaceId::new(101), SpaceId::new(202));
        let size = CGSize::new(1000.0, 800.0);
        let plain = OnScreenEntry::plain;
        fx.visible = vec![space_a, space_b];

        fx.expose(space_a, size);
        let _ = fx.open(space_a, vec![
            plain(WindowId::new(1, 1)),
            plain(WindowId::new(1, 2)),
        ]);
        let _ = fx.command(space_a, widen());
        let resized_layout = fx.frames(space_a);

        fx.expose(space_b, size);
        let _ = fx.open(space_b, vec![plain(WindowId::new(2, 1))]);

        assert_eq!(
            fx.frames(space_a),
            resized_layout,
            "membership sync on one space must not rebalance saved layouts on another space"
        );
    }

    #[test]
    fn window_removed_preserve_floating_keeps_workspace_assignment() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(303);
        let wid = WindowId::new(42, 1);
        fx.expose(space, CGSize::new(1000.0, 800.0));
        let _ = fx.open(space, vec![OnScreenEntry::plain(wid)]);
        let assigned =
            fx.workspace_of(space, wid).expect("window should have a workspace assignment");

        let _ = fx.event(LayoutEvent::WindowRemovedPreserveFloating(wid));
        assert_eq!(
            fx.workspace_of(space, wid),
            Some(assigned),
            "temporary layout removal must not clear workspace ownership"
        );

        let _ = fx.event(LayoutEvent::WindowAdded(space, wid));
        assert_eq!(
            fx.workspace_of(space, wid),
            Some(assigned),
            "window should reappear in the same workspace after a temporary hide"
        );
    }

    #[test]
    fn moving_floating_window_to_space_clears_source_floating_state() {
        let mut fx = EngineFixture::new();
        let (source, target) = (SpaceId::new(304), SpaceId::new(305));
        let size = CGSize::new(1000.0, 800.0);
        let wid = WindowId::new(43, 1);
        fx.expose(source, size);
        fx.expose(target, size);
        let _ = fx.open(source, vec![OnScreenEntry::plain(wid)]);
        let source_workspace = fx.engine.active_workspace(source).expect("source workspace");
        let target_workspace = fx.engine.active_workspace(target).expect("target workspace");
        fx.float(source, wid);
        let position = CGRect::new(CGPoint::new(120.0, 140.0), CGSize::new(260.0, 220.0));
        fx.engine.store_floating_position(source, source_workspace, wid, position);

        let response = fx.engine.move_window_to_space(
            &mut fx.windows,
            &mut fx.memory,
            source,
            target,
            size,
            wid,
        );

        assert_eq!(response.focus_window, Some(wid));
        assert_eq!(fx.workspace_of(target, wid), Some(target_workspace));
        assert_eq!(
            fx.engine.get_floating_position(source, source_workspace, wid),
            None,
            "cross-space moves must clear the source workspace's saved floating frame"
        );
        let gaps = LayoutSettings::default().gaps.effective_for_display(None);
        let source_frames = fx.engine.calculate_layout_for_workspace(
            &fx.windows,
            source,
            source_workspace,
            fx.screens[&source],
            &gaps,
        );
        assert!(
            !source_frames.into_iter().any(|(window_id, _)| window_id == wid),
            "source workspace layout must not keep emitting the moved floating window"
        );
    }

    /// Horizontal focus must be able to leave the floating layer; a modulo cycle over the floating
    /// set never reached the strip.
    #[test]
    fn horizontal_focus_escapes_the_floating_layer_into_the_strip() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(410);
        let [tiled, float_a, float_b] = [1, 2, 3].map(|idx| WindowId::new(71, idx));
        fx.expose(space, CGSize::new(1200.0, 800.0));
        let _ = fx.open(
            space,
            [tiled, float_a, float_b].map(OnScreenEntry::plain).to_vec(),
        );
        fx.float(space, float_a);
        fx.float(space, float_b);
        fx.centers.insert(space, CGPoint::new(0.0, 0.0));

        // Walk right repeatedly from a floating window. Without the fix this only
        // ever alternates between float_a and float_b.
        fx.engine.focused_window = Some(float_a);
        let mut reached_tiled = false;
        for _ in 0..6 {
            let response = fx.command(space, LayoutCommand::MoveFocus(Direction::Right));
            if let Some(next) = response.focus_window {
                fx.engine.focused_window = Some(next);
                if next == tiled {
                    reached_tiled = true;
                    break;
                }
            }
        }

        assert!(
            reached_tiled,
            "focus never escaped the floating layer; it cycled between the floating \
             windows instead of falling through to the tiled strip"
        );
    }

    /// A showing floating window saves the frame it is laid out at over what its workspace had; a
    /// parked one saves its frame only when its workspace has none, so it comes back where it was.
    #[test]
    fn a_parked_floating_window_keeps_the_frame_its_workspace_saved() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(411);
        let wid = WindowId::new(72, 1);
        fx.expose(space, CGSize::new(1200.0, 800.0));
        let _ = fx.open(space, vec![OnScreenEntry::plain(wid)]);
        fx.float(space, wid);
        let shown = fx.workspace_of(space, wid).expect("assigned");
        let other = fx.workspaces(space).into_iter().find(|&w| w != shown).expect("another");
        let saved = || CGRect::new(CGPoint::new(100.0, 100.0), CGSize::new(300.0, 200.0));
        let saved_in = |fx: &EngineFixture| fx.engine.floating_positions.get(space, shown, wid);
        fx.engine.floating_positions.store(space, shown, wid, saved());
        assert_eq!(fx.frame(space, wid), saved());

        fx.show(space, other);
        let parked = fx.frame(space, wid);
        assert_ne!(parked, saved());
        assert_eq!(saved_in(&fx), Some(saved()), "a park keeps the saved frame");

        fx.engine.floating_positions.remove_window(wid);
        let _ = fx.frames(space);
        assert!(
            saved_in(&fx).is_some(),
            "a park with nothing saved saves a frame"
        );
    }

    /// Each display is its own strip, so horizontal focus stops at this display's last column and
    /// never continues onto the neighbour. Baked in: this was a setting, defaulting to the hop.
    #[test]
    fn horizontal_focus_stops_at_the_end_of_this_displays_strip() {
        let TwoDisplays {
            mut fx,
            first: left,
            on_first: on_left,
            on_second: on_right,
            ..
        } = two_displays_side_by_side(540);

        // Sitting on the only window of the LEFT display, walk right: there is no further column on
        // this display, and the neighbour is not a continuation of this strip.
        fx.engine.focused_window = Some(on_left);
        let response = fx.command(left, LayoutCommand::MoveFocus(Direction::Right));
        assert_ne!(
            response.focus_window,
            Some(on_right),
            "focus must not cross to the adjacent display"
        );
        assert_eq!(
            response.edge_hit,
            Some(Direction::Right),
            "the strip stopped at its end: the reactor bounces it"
        );

        // Up at the top of a one-window column is not a strip edge, so nothing bounces.
        fx.engine.focused_window = Some(on_left);
        let up = fx.command(left, LayoutCommand::MoveFocus(Direction::Up));
        assert_eq!(up.edge_hit, None);
    }

    /// The half that must NOT be broken by baking the strips apart. Up and down are not strip axes, so
    /// they still cross between displays — treating all four directions alike would cut the displays
    /// off from each other entirely.
    #[test]
    fn vertical_focus_still_crosses_between_displays() {
        let TwoDisplays {
            mut fx,
            first: above,
            on_first: up_top,
            on_second: down_below,
            ..
        } = two_displays_stacked(560);

        fx.engine.focused_window = Some(up_top);
        let response = fx.command(above, LayoutCommand::MoveFocus(Direction::Down));
        assert_eq!(
            response.focus_window,
            Some(down_below),
            "vertical navigation between displays must keep working"
        );
        assert_eq!(response.edge_hit, None, "focus moved on: no edge");
    }

    /// Reported: pushing a window past the last column sent it to the other monitor, while focus in
    /// the same direction stopped. The strips are independent for MOVING too, and the stop reports the
    /// edge so the strip bounces instead of the key looking dead.
    #[test]
    fn a_moved_window_stays_on_its_own_displays_strip() {
        let TwoDisplays {
            mut fx,
            first: left,
            on_first: on_left,
            ..
        } = two_displays_side_by_side(530);

        // The only window of the LEFT display, pushed right. There is no further column on this
        // display, and the neighbour is a strip of its own.
        fx.engine.focused_window = Some(on_left);
        let response = fx.command(left, LayoutCommand::MoveNode(Direction::Right));
        assert_eq!(
            fx.engine.space_with_window(on_left),
            Some(left),
            "the window must not travel to the adjacent display"
        );
        assert_eq!(
            response.edge_hit,
            Some(Direction::Right),
            "the strip stopped at its end: the reactor bounces it"
        );
    }

    /// Vertical moves are not along a strip, so a window can still be pushed to the display below.
    #[test]
    fn a_window_can_still_be_moved_to_the_display_below() {
        let TwoDisplays {
            mut fx,
            first: above,
            second: below,
            on_first: up_top,
            ..
        } = two_displays_stacked(570);

        fx.engine.focused_window = Some(up_top);
        let response = fx.command(above, LayoutCommand::MoveNode(Direction::Down));
        assert_eq!(
            fx.engine.space_with_window(up_top),
            Some(below),
            "a vertical move still carries the window across"
        );
        assert_eq!(response.edge_hit, None);
    }

    /// Two windows side by side on one space, the first selected.
    fn two_columns() -> (EngineFixture, SpaceId, WindowId, WindowId) {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(532);
        let (first, second) = (WindowId::new(75, 1), WindowId::new(75, 2));
        fx.expose(space, CGSize::new(1000.0, 800.0));
        let _ = fx.open(space, vec![
            OnScreenEntry::plain(first),
            OnScreenEntry::plain(second),
        ]);
        fx.engine.focused_window = Some(first);
        let _ = fx.command(space, LayoutCommand::MoveFocus(Direction::Left));
        (fx, space, first, second)
    }

    /// A move that lands somewhere reports no edge, or every successful press would bounce too. It
    /// names the window it moved and the way it went, so the two columns are drawn changing places.
    #[test]
    fn a_move_with_room_to_go_names_the_window_and_reports_no_edge() {
        let (mut fx, space, first, _) = two_columns();
        let moved = fx.command(space, LayoutCommand::MoveNode(Direction::Right));
        assert_eq!(moved.edge_hit, None);
        assert_eq!(moved.moved, Some((first, Direction::Right)));

        let stopped = fx.command(space, LayoutCommand::MoveNode(Direction::Right));
        assert_eq!(stopped.edge_hit, Some(Direction::Right));
        assert_eq!(stopped.moved, None, "at the end nothing changed places");
    }

    /// Pulling a window out of a stacked column makes a new column and resizes the ones it leaves: not
    /// two columns changing places. Nor is a move up or down its column.
    #[test]
    fn a_move_out_of_a_stack_or_along_a_column_names_nothing() {
        let (mut fx, space, _, _) = two_columns();
        let _ = fx.command(space, LayoutCommand::MoveFocus(Direction::Right));
        let _ = fx.command(space, LayoutCommand::ToggleFold(Direction::Left));
        let vertical = fx.command(space, LayoutCommand::MoveNode(Direction::Up));
        assert_eq!(vertical.moved, None);
        let pulled = fx.command(space, LayoutCommand::MoveNode(Direction::Right));
        assert_eq!(pulled.edge_hit, None, "it moved");
        assert_eq!(pulled.moved, None);
    }

    /// Exploding a column raises the focused window alone, so macOS keys it and focus stays on it.
    #[test]
    fn exploding_a_column_raises_only_the_focused_window() {
        let (mut fx, space, _, second) = two_columns();
        let _ = fx.command(space, LayoutCommand::MoveFocus(Direction::Right));
        let _ = fx.command(space, LayoutCommand::ToggleFold(Direction::Left));

        let exploded = fx.command(space, LayoutCommand::ExplodeColumn);

        assert!(exploded.changed);
        assert_eq!(exploded.raise_windows, vec![second]);
        assert_eq!(exploded.focus_window, None);
    }

    /// Moving a window down the workspace stack from the bottom one has nowhere to go: the stack
    /// never wraps. Reported as feeling stuck, because nothing moved and nothing said why.
    #[test]
    fn moving_a_window_past_the_end_of_the_workspace_stack_reports_the_edge() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(533);
        let wid = WindowId::new(76, 1);
        fx.expose(space, CGSize::new(1000.0, 800.0));
        let _ = fx.open(space, vec![OnScreenEntry::plain(wid)]);
        fx.engine.focused_window = Some(wid);
        let workspaces = fx.workspaces(space);
        fx.show(space, *workspaces.last().unwrap());
        let move_focused = |workspace| LayoutCommand::MoveWindowToWorkspace {
            workspace,
            follow: true,
            window_id: None,
        };
        let _ = fx.command(
            space,
            move_focused(WorkspaceSelector::Index(workspaces.len() - 1)),
        );

        let next = WorkspaceSelector::Relative(RelativeWorkspace::Next);
        let past_the_bottom = fx.command(space, move_focused(next));
        assert_eq!(past_the_bottom.edge_hit, Some(Direction::Down));

        // Naming a workspace that does not exist is a request that cannot be honoured, not a push
        // against an end, so it reports no direction to bounce in.
        let nonexistent = fx.command(space, move_focused(WorkspaceSelector::Index(99)));
        assert_eq!(nonexistent.edge_hit, None);
    }

    #[test]
    fn move_focus_to_uninitialized_adjacent_space_does_not_panic() {
        let mut fx = EngineFixture::new();
        let (current, adjacent) = (SpaceId::new(50), SpaceId::new(51));
        fx.expose(current, CGSize::new(1920.0, 1080.0));
        fx.visible.push(adjacent);
        fx.centers.insert(current, CGPoint::new(0.0, 0.0));
        fx.centers.insert(adjacent, CGPoint::new(1920.0, 0.0));

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            fx.command(current, LayoutCommand::MoveFocus(Direction::Right))
        }));

        assert!(
            result.is_ok(),
            "cross-space move focus should not panic when adjacent space is not initialized"
        );
    }

    #[test]
    fn workspace_switch_response_reports_whether_workspace_changed() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(81);
        let first = fx.workspaces(space)[0];
        fx.show(space, first);

        let already_active = fx.command(space, LayoutCommand::SwitchToWorkspace(0));
        assert!(!already_active.changed);
        let missing = fx.command(space, LayoutCommand::SwitchToWorkspace(usize::MAX));
        assert!(!missing.changed);
        let switched = fx.command(space, LayoutCommand::SwitchToWorkspace(1));
        assert!(switched.changed);
    }

    #[test]
    fn workspace_navigation_no_ops_report_unchanged() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(82);
        let workspaces = fx.workspaces(space);

        fx.show(space, *workspaces.last().unwrap());
        let past_the_bottom = fx.command(space, LayoutCommand::NextWorkspace(None));
        assert!(!past_the_bottom.changed);
        assert_eq!(
            past_the_bottom.edge_hit,
            Some(Direction::Down),
            "the bottom of the stack"
        );

        fx.show(space, workspaces[0]);
        let no_eligible = fx.command(space, LayoutCommand::NextWorkspace(Some(true)));
        assert!(!no_eligible.changed);
        assert_eq!(
            no_eligible.edge_hit,
            Some(Direction::Down),
            "nothing further to skip to"
        );
        let top = fx.command(space, LayoutCommand::PrevWorkspace(None));
        assert!(!top.changed);
        assert_eq!(top.edge_hit, Some(Direction::Up), "the top of the stack");
    }

    #[test]
    fn partial_windows_on_screen_update_preserves_assigned_tiled_windows() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(94);
        let (w1, w2) = (WindowId::new(5153, 1), WindowId::new(5153, 2));
        let info = |wid| OnScreenEntry::plain(wid).sized(500.0, 500.0);
        fx.expose(space, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(space, vec![info(w1), info(w2)]);
        let _ = fx.event(LayoutEvent::WindowFocused(space, w1));
        let _ = fx.command(space, widen());
        let before = fx.frames(space);

        // Simulate a discovery snapshot that temporarily omitted w2.
        let _ = fx.open(space, vec![info(w1)]);

        assert_eq!(
            fx.frames(space),
            before,
            "partial discovery must not remove an assigned window or reset its split"
        );
    }

    #[test]
    fn removing_a_window_does_not_rebalance_other_workspaces() {
        let mut fx = EngineFixture::new();
        let (space_a, space_b) = (SpaceId::new(95), SpaceId::new(96));
        let info = |wid| OnScreenEntry::plain(wid).sized(500.0, 500.0);
        let (a1, a2, b1) = (
            WindowId::new(5154, 1),
            WindowId::new(5154, 2),
            WindowId::new(5155, 1),
        );
        fx.expose(space_a, CGSize::new(1000.0, 1000.0));
        fx.expose(space_b, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(space_a, vec![info(a1), info(a2)]);
        let _ = fx.event(LayoutEvent::WindowFocused(space_a, a1));
        let _ = fx.command(space_a, widen());
        let before = fx.frames(space_a);

        let _ = fx.open(space_b, vec![info(b1)]);
        let _ = fx.windows.remove_window_assignment(b1);
        let _ = fx.event(LayoutEvent::WindowRemoved(b1));

        assert_eq!(
            fx.frames(space_a),
            before,
            "removing a window must not rebalance layouts in other workspaces"
        );
    }

    #[test]
    fn removing_unknown_window_does_not_rebalance_layout() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(92);
        let windows = [1, 2, 3].map(|idx| WindowId::new(5151, idx));
        fx.expose(space, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(
            space,
            windows.map(|wid| OnScreenEntry::plain(wid).sized(500.0, 500.0)).to_vec(),
        );
        let _ = fx.event(LayoutEvent::WindowFocused(space, windows[0]));
        let _ = fx.command(space, LayoutCommand::MoveNode(Direction::Up));
        let modified = fx.frames(space);

        let _ = fx.event(LayoutEvent::WindowRemoved(WindowId::new(9999, 1)));

        assert_eq!(fx.frames(space), modified);
    }

    #[test]
    fn a_preview_of_a_workspace_answers_its_next_pass_and_leaves_the_strip() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(94);
        let pid: pid_t = 5153;
        fx.expose(space, CGSize::new(1000.0, 1000.0));
        let windows = (1..=2)
            .map(|idx| OnScreenEntry::plain(WindowId::new(pid, idx)).sized(500.0, 500.0))
            .collect();
        let _ = fx.open(space, windows);
        let screen = fx.screens[&space];
        let gaps = fx.engine.layout_settings.gaps.effective_for_display(None);
        let workspace = fx.engine.active_workspace(space).expect("an active workspace");
        let layout = fx.engine.workspace_layouts.active(space, workspace).expect("a layout");

        let mut preview =
            fx.engine
                .preview_layout_for_workspace(&fx.windows, space, workspace, screen, &gaps);
        assert_eq!(preview.len(), 2);
        assert_eq!(fx.engine.workspace_tree(workspace).strip_origin(layout), None);

        let mut laid_out =
            fx.engine
                .calculate_layout_for_workspace(&fx.windows, space, workspace, screen, &gaps);
        laid_out.sort_by_key(|(wid, _)| wid.idx);
        preview.sort_by_key(|(wid, _)| wid.idx);
        assert_eq!(laid_out, preview);
        assert!(fx.engine.workspace_tree(workspace).strip_origin(layout).is_some());
    }

    #[test]
    fn duplicate_window_added_is_treated_as_noop_for_active_layout() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(93);
        let wid = WindowId::new(5152, 1);
        fx.expose(space, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(space, vec![OnScreenEntry::plain(wid).sized(500.0, 500.0)]);
        let before = fx.frames(space);

        assert!(!fx.engine.add_window_to_layout(&mut fx.windows, &mut fx.memory, space, wid));
        assert_eq!(fx.frames(space), before);
    }

    #[test]
    fn window_added_with_auto_assign_failing_and_no_active_workspace_adds_nothing() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(95);
        let wid = WindowId::new(5153, 1);
        fx.engine
            .virtual_workspace_manager
            .update_settings(&VirtualWorkspaceSettings::default(), &LayoutSettings::default());
        fx.engine.virtual_workspace_manager.forget_every_workspace();
        fx.memory.affinity.set_display_space(&"display-a".into(), space);
        fx.engine.floating.add_floating(wid);

        let _ = fx.event(LayoutEvent::WindowAdded(space, wid));

        assert!(!fx.engine.floating.active_flat(space).contains(&wid));
        assert_eq!(fx.memory.affinity.window_home(wid), None);
    }

    #[test]
    fn toggle_floating_on_a_space_with_no_active_workspace_leaves_the_window_floating() {
        let mut fx = EngineFixture::new();
        let (home, uninitialised) = (SpaceId::new(96), SpaceId::new(97));
        let wid = WindowId::new(5154, 1);
        fx.expose(home, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(home, vec![OnScreenEntry::plain(wid).sized(500.0, 500.0)]);
        fx.float(home, wid);
        fx.engine.focused_window = Some(wid);

        let response = fx.command(uninitialised, LayoutCommand::ToggleWindowFloating);

        assert_eq!(response.focus_window, None);
        assert!(fx.engine.floating.is_floating(wid));
        assert_eq!(fx.engine.active_workspace(uninitialised), None);
    }

    #[test]
    fn workspace_switch_only_commits_focus_after_authoritative_commit() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(94);
        let (wid1, wid2) = (WindowId::new(5153, 1), WindowId::new(5153, 2));
        let info = |wid| OnScreenEntry::plain(wid).sized(500.0, 500.0);
        fx.expose(space, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(space, vec![info(wid1), info(wid2)]);
        let _ = fx.event(LayoutEvent::WindowFocused(space, wid1));
        let workspace_two = fx.workspaces(space)[1];

        let _ = fx.command(space, move_window(wid2, WorkspaceSelector::Index(1), false));
        let response = fx.command(space, LayoutCommand::SwitchToWorkspace(1));

        assert_eq!(fx.engine.active_workspace(space), Some(workspace_two));
        assert_eq!(response.focus_window, Some(wid2));
        assert_ne!(fx.engine.focused_window, Some(wid2));

        fx.engine.commit_workspace_focus(&mut fx.windows, space, response.focus_window);

        assert_eq!(fx.engine.focused_window, Some(wid2));
        assert_eq!(
            fx.engine.virtual_workspace_manager().last_focused_window(space, workspace_two),
            Some(wid2)
        );
    }

    #[test]
    fn move_window_to_workspace_updates_authoritative_workspace_membership() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(95);
        let wid = WindowId::new(6001, 1);
        fx.expose(space, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(space, vec![OnScreenEntry::plain(wid).sized(500.0, 500.0)]);
        let workspaces = fx.workspaces(space);
        let members = |fx: &EngineFixture, workspace| {
            fx.engine
                .virtual_workspace_manager
                .workspace_windows(&fx.windows, space, workspace)
        };

        let _ = fx.command(space, move_window(wid, WorkspaceSelector::Index(1), false));

        assert!(
            members(&fx, workspaces[0]).is_empty(),
            "source workspace must be empty after a same-space workspace move"
        );
        assert_eq!(fx.workspace_of(space, wid), Some(workspaces[1]));
        assert_eq!(members(&fx, workspaces[1]), vec![wid]);

        for (step, expected) in [
            (RelativeWorkspace::Next, workspaces[2]),
            (RelativeWorkspace::Prev, workspaces[1]),
        ] {
            let target = WorkspaceSelector::Relative(step);
            let _ = fx.command(space, move_window(wid, target, false));
            assert_eq!(fx.workspace_of(space, wid), Some(expected));
        }
    }

    /// The default bounds stop a column at 0.9 of the tiling width; the drag took it to 0.95, and
    /// the workspace it arrives on has to know that no bound holds it.
    #[test]
    fn a_window_moved_to_another_workspace_keeps_its_dragged_width() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(97);
        let wid = WindowId::new(6003, 1);
        fx.memory.affinity.set_display_space(&"built-in".into(), space);
        fx.expose(space, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(space, vec![OnScreenEntry::plain(wid).sized(500.0, 500.0)]);
        let screen = fx.screens[&space];
        let start = fx.frame(space, wid);
        let gaps = LayoutSettings::default().gaps.effective_for_display(None);
        let dragged = (compute_tiling_area(screen, &gaps).size.width * 0.95).round();
        let wider = CGRect::new(start.origin, CGSize::new(dragged, start.size.height));
        let _ = fx.event(LayoutEvent::WindowResized {
            wid,
            drag: Some(Drag::report(start, wider, None)),
            new_frame: wider,
            screens: vec![(space, screen, None)],
        });
        assert_eq!(fx.frame(space, wid).size.width, dragged);

        let next = WorkspaceSelector::Relative(RelativeWorkspace::Next);
        let _ = fx.command(space, move_window(wid, next, true));
        assert_eq!(fx.engine.active_workspace(space), Some(fx.workspaces(space)[1]));
        assert_eq!(fx.frame(space, wid).size.width, dragged);
    }

    /// The one way a placement fails is a workspace the store does not have; the window it took
    /// out goes back to the strip it left rather than being lost from every layer.
    #[test]
    fn a_placement_that_cannot_be_assigned_puts_a_tiled_window_back() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(98);
        let wid = WindowId::new(6004, 1);
        fx.expose(space, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(space, vec![OnScreenEntry::plain(wid).sized(500.0, 500.0)]);
        let workspace = fx.workspace_of(space, wid).expect("assigned");
        let before = fx.frames(space);
        let from = moves::Slot { space, workspace };
        let nowhere = moves::Slot {
            space,
            workspace: VirtualWorkspaceId::default(),
        };

        let detached = fx.engine.take_window(wid, from);
        assert_eq!(detached.layer, moves::Layer::Tiled);
        assert!(fx.frames(space).is_empty());
        assert!(!fx.engine.place_window(&mut fx.windows, &fx.memory, detached, nowhere));

        assert_eq!(fx.frames(space), before);
        assert_eq!(fx.workspace_of(space, wid), Some(workspace));
    }

    #[test]
    fn a_placement_that_cannot_be_assigned_puts_a_floating_window_back() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(99);
        let wid = WindowId::new(6005, 1);
        fx.expose(space, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(space, vec![OnScreenEntry::plain(wid)]);
        let workspace = fx.workspace_of(space, wid).expect("assigned");
        fx.float(space, wid);
        let from = moves::Slot { space, workspace };
        let nowhere = moves::Slot {
            space,
            workspace: VirtualWorkspaceId::default(),
        };

        let detached = fx.engine.take_window(wid, from);
        assert_eq!(detached.layer, moves::Layer::Floating);
        assert!(!fx.engine.floating.active_flat(space).contains(&wid));
        assert!(!fx.engine.place_window(&mut fx.windows, &fx.memory, detached, nowhere));

        assert!(fx.engine.floating.active_flat(space).contains(&wid));
        assert_eq!(fx.workspace_of(space, wid), Some(workspace));
    }

    /// The floating layer holds the windows of the workspace showing, so a floating window sent to
    /// one that is not showing leaves it, and comes back with the move home.
    #[test]
    fn a_floating_window_moved_to_a_hidden_workspace_leaves_the_floating_layer() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(100);
        let wid = WindowId::new(6006, 1);
        fx.expose(space, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(space, vec![OnScreenEntry::plain(wid)]);
        fx.float(space, wid);
        let workspaces = fx.workspaces(space);

        let _ = fx.command(space, move_window(wid, WorkspaceSelector::Index(1), false));
        assert_eq!(fx.workspace_of(space, wid), Some(workspaces[1]));
        assert!(!fx.engine.floating.active_flat(space).contains(&wid));

        let response = fx.command(space, move_window(wid, WorkspaceSelector::Index(0), false));
        assert_eq!(fx.workspace_of(space, wid), Some(workspaces[0]));
        assert!(fx.engine.floating.active_flat(space).contains(&wid));
        assert_eq!(response.focus_window, Some(wid));
    }

    #[test]
    fn a_tiled_window_moved_to_another_space_joins_its_strip_and_keeps_focus() {
        let TwoDisplays {
            mut fx,
            first,
            second,
            on_first,
            on_second,
        } = two_displays_side_by_side(101);
        fx.engine.focused_window = Some(on_first);
        let size = CGSize::new(1000.0, 800.0);

        let response = fx.engine.move_window_to_space(
            &mut fx.windows,
            &mut fx.memory,
            first,
            second,
            size,
            on_first,
        );

        assert_eq!(response.focus_window, Some(on_first));
        assert_eq!(fx.engine.focused_window, Some(on_first));
        assert!(fx.frames(first).is_empty());
        let on_second_strip: Vec<WindowId> =
            fx.frames(second).into_iter().map(|(wid, _)| wid).collect();
        assert_eq!(on_second_strip, vec![on_first, on_second]);
        let target = fx.engine.active_workspace(second).expect("shown");
        assert_eq!(
            fx.engine.virtual_workspace_manager().last_focused_window(second, target),
            Some(on_first)
        );
    }

    /// Floating a window takes it out of every strip, also when the space the command names shows
    /// no workspace to look the strip up by: a window both floating and tiled is laid out twice.
    #[test]
    fn floating_a_window_from_a_space_with_no_active_workspace_takes_it_out_of_its_strip() {
        let mut fx = EngineFixture::new();
        let (home, uninitialised) = (SpaceId::new(102), SpaceId::new(103));
        let wid = WindowId::new(6007, 1);
        fx.expose(home, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(home, vec![OnScreenEntry::plain(wid).sized(500.0, 500.0)]);
        fx.engine.focused_window = Some(wid);

        let _ = fx.command(uninitialised, LayoutCommand::ToggleWindowFloating);

        assert!(fx.engine.floating.is_floating(wid));
        assert!(fx.frames(home).is_empty());
    }

    #[test]
    fn floating_a_window_with_no_space_named_takes_it_out_of_its_strip() {
        let mut fx = EngineFixture::new();
        let home = SpaceId::new(104);
        let wid = WindowId::new(6008, 1);
        fx.expose(home, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(home, vec![OnScreenEntry::plain(wid).sized(500.0, 500.0)]);
        fx.engine.focused_window = Some(wid);

        let _ = fx.engine.handle_command(
            &mut fx.windows,
            &mut fx.memory,
            None,
            &fx.visible,
            &fx.centers,
            LayoutCommand::ToggleWindowFloating.into(),
        );

        assert!(fx.engine.floating.is_floating(wid));
        assert!(fx.frames(home).is_empty());
    }

    #[test]
    fn move_window_to_workspace_can_follow_the_window() {
        let mut fx = EngineFixture::new();
        let space = SpaceId::new(96);
        let wid = WindowId::new(6002, 1);
        fx.expose(space, CGSize::new(1000.0, 1000.0));
        let _ = fx.open(space, vec![OnScreenEntry::plain(wid).sized(500.0, 500.0)]);
        let target = fx.workspaces(space)[1];

        let next = WorkspaceSelector::Relative(RelativeWorkspace::Next);
        let response = fx.command(space, move_window(wid, next, true));

        assert_eq!(fx.engine.active_workspace(space), Some(target));
        assert_eq!(response.focus_window, Some(wid));
        assert_eq!(fx.workspace_of(space, wid), Some(target));
    }
}
