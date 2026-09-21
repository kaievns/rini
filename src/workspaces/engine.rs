use std::cmp::Ordering;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{
    Direction, FloatingManager, LayoutId, LayoutSystemKind, ResizeOrientation, WorkspaceLayouts,
};
use crate::windows::domain::info::AppInfo;
use rini_core::ids::{WindowId, pid_t};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use crate::layout::settings::LayoutSettings;
use rini_ipc::protocol::WorkspaceSelector;
use crate::workspaces::LayoutSystem;
use crate::workspaces::domain::floating::FloatingFullscreenKind;
use crate::layout::WindowLayoutConstraints;
use crate::workspaces::domain::app_rules::{AfterRules, AppRuleOutcome, AppRuleResize, AppRuleWorkspaceFocus, BeforeRules};
use crate::windows::domain::state::WindowState;
use crate::workspaces::broadcast::{BroadcastEvent, BroadcastSender, protocol_workspace_id};
use crate::workspaces::domain::display_affinity::ColumnWidth;
use crate::workspaces::domain::virtual_workspace::{VirtualWorkspaceId, WorkspaceStore};
use crate::workspaces::{AppRuleEffects, AppRuleResult, DisplayAffinity, FloatingPositionStore, WindowStore};
use crate::windows::domain::rules::{AppRuleDecision, AppRuleEngine, WindowRuleContext};
use rini_core::ids::SpaceId;

mod persistence;

use persistence::PersistenceState;
pub use persistence::{RestoreReport, RestoreRequest, RestoreScope, RestoreSource, RestoreWarning};
pub use rini_ipc::protocol::LayoutCommand;

#[derive(Debug, Default)]
struct WindowRemovalImpact {
    active_space: Option<SpaceId>,
}

/// One window as `WindowsOnScreenUpdated` describes it: id, title, AX role, AX subrole,
/// is_modal, is_resizable, size, min size, max size.
pub type OnScreenEntry = (
    WindowId,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    bool,
    CGSize,
    Option<CGSize>,
    Option<CGSize>,
);

/// The entry for `wid`. A window the store no longer has is resizable, sized zero, untitled.
pub fn on_screen_entry(wid: WindowId, window: Option<&WindowState>) -> OnScreenEntry {
    (
        wid,
        window.map(|w| w.info.title.clone()),
        window.and_then(|w| w.info.ax_role.clone()),
        window.and_then(|w| w.info.ax_subrole.clone()),
        window.is_some_and(|w| w.info.is_modal),
        window.map_or(true, |w| w.info.is_resizable),
        window.map_or(CGSize::new(0.0, 0.0), |w| w.frame_monotonic.size),
        window.and_then(|w| w.info.min_size),
        window.and_then(|w| w.info.max_size),
    )
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
    WindowResized {
        wid: WindowId,
        old_frame: CGRect,
        new_frame: CGRect,
        screens: Vec<(SpaceId, CGRect, Option<String>)>,
    },
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
    /// A gesture overscrolled the strip past `workspace_switch_threshold`: the swipe continues
    /// into a workspace switch (`propagate_to_workspace_swipe`).
    pub boundary_hit: Option<Direction>,
    /// A command tried to go past an end: the strip's first/last column (`Left`/`Right`) or the
    /// workspace stack's top/bottom (`Up`/`Down`). Nothing moved; the reactor bounces the view
    /// so the stop reads as an edge rather than a dropped keypress.
    #[serde(default)]
    pub edge_hit: Option<Direction>,
}

#[must_use]
pub struct LayoutEventOutcome {
    pub response: EventResponse,
    pub app_rules: AppRuleOutcome,
}

impl std::ops::Deref for LayoutEventOutcome {
    type Target = EventResponse;

    fn deref(&self) -> &Self::Target {
        &self.response
    }
}

pub struct LayoutEngine {
    workspace_layouts: WorkspaceLayouts,
    floating: FloatingManager,
    floating_positions: FloatingPositionStore,
    app_rules: AppRuleEngine,
    focused_window: Option<WindowId>,
    window_layout_constraints: HashMap<WindowId, WindowLayoutConstraints>,
    virtual_workspace_manager: WorkspaceStore,
    layout_settings: LayoutSettings,
    broadcast_tx: Option<BroadcastSender>,
    /// Durable display identity: which native space each physical display owns, and which
    /// display each window belongs to. Replaces the former `space_display_map` /
    /// `display_last_space` pair, which could disagree with each other.
    display_affinity: DisplayAffinity,
    /// Where each application's windows belong, under a key that survives the application. See
    /// `docs/workspaces/launch-memory.md` (in this crate).
    launch_memory: crate::workspaces::domain::launch_memory::LaunchMemory,
    /// Display UUIDs currently attached. Runtime only: it describes the machine right now, not the
    /// layout, and it is what the launch memory is keyed by.
    connected_displays: Vec<String>,
    persistence: PersistenceState,
    /// Set only while a master-file startup restore is waiting for the first display snapshot.
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
    pub fn focused_window(&self) -> Option<WindowId> {
        self.focused_window
    }

    /// Resolve an optional workspace index and snapshot its layout for read-only consumers.
    pub fn query_workspace_layout(
        &self,
        space: SpaceId,
        workspace_index: Option<usize>,
    ) -> Option<WorkspaceLayoutQuerySnapshot> {
        let workspaces = self.virtual_workspace_manager.existing_workspaces(space);
        let active = self.virtual_workspace_manager.active_workspace(space)?;
        let (workspace_index, workspace_id) = match workspace_index {
            Some(index) => (index, workspaces.get(index)?.0),
            None => (workspaces.iter().position(|(id, _)| *id == active)?, active),
        };
        let layout = self.workspace_layouts.active(space, workspace_id)?;
        let workspace = self.virtual_workspace_manager.workspace_info(space, workspace_id)?;
        let selected_window = workspace.layout_system.selected_window(layout);
        let container_tree = workspace.layout_system.container_tree(layout);

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
    fn workspace_tree_mut(&mut self, ws_id: VirtualWorkspaceId) -> &mut LayoutSystemKind {
        &mut self.virtual_workspace_manager.workspaces[ws_id].layout_system
    }

    /// Get immutable access to a workspace's layout system.
    fn workspace_tree(&self, ws_id: VirtualWorkspaceId) -> &LayoutSystemKind {
        &self.virtual_workspace_manager.workspaces[ws_id].layout_system
    }

    /// Get the active workspace and layout for a space.
    fn workspace_and_layout(&self, space: SpaceId) -> Option<(VirtualWorkspaceId, LayoutId)> {
        let ws_id = self.active_workspace_id(space)?;
        let layout = self.workspace_layouts.active(space, ws_id)?;
        Some((ws_id, layout))
    }

    fn response_for_raised_windows(raise_windows: Vec<WindowId>) -> EventResponse {
        if raise_windows.is_empty() {
            EventResponse::default()
        } else {
            EventResponse {
                changed: true,
                raise_windows,
                focus_window: None,
                boundary_hit: None,
                edge_hit: None,
            }
        }
    }

    fn toggle_stack_for_workspace(
        &mut self,
        workspace_id: VirtualWorkspaceId,
        layout: LayoutId,
    ) -> EventResponse {
        let unstacked_windows =
            self.workspace_tree_mut(workspace_id).unstack_parent_of_selection(layout);
        if !unstacked_windows.is_empty() {
            return Self::response_for_raised_windows(unstacked_windows);
        }

        let stacked_windows =
            self.workspace_tree_mut(workspace_id).apply_stacking_to_parent_of_selection(layout);
        if !stacked_windows.is_empty() {
            return Self::response_for_raised_windows(stacked_windows);
        }

        let visible_windows = self.workspace_tree(workspace_id).visible_windows_in_layout(layout);
        Self::response_for_raised_windows(visible_windows)
    }

}

impl LayoutEngine {
    pub fn set_layout_settings(&mut self, settings: &LayoutSettings) {
        self.layout_settings = settings.clone();

        let mut scrolling = settings.scrolling.clone();
        scrolling.base = settings.resolved_base();
        for (_, ws) in self.virtual_workspace_manager.workspaces.iter_mut() {
            let LayoutSystemKind::Scrolling(system) = &mut ws.layout_system;
            system.update_settings(&scrolling);
        }
    }

    pub fn update_virtual_workspace_settings(
        &mut self,
        settings: &crate::workspaces::settings::VirtualWorkspaceSettings,
    ) {
        self.app_rules = AppRuleEngine::new(&settings.app_rules, settings.float_modal_windows);
        self.virtual_workspace_manager.update_settings(settings, &self.layout_settings);
    }

    /// Where the strip's viewport sits on `space`, in points along the strip.
    ///
    /// `None` when the space has no active workspace. Callers compare it against the value they last saw to
    /// learn how far the strip moved, which is exact and needs no reference to any window's real frame.
    pub fn strip_scroll_offset(&self, space: SpaceId) -> Option<f64> {
        let (ws_id, layout) = self.workspace_and_layout(space)?;
        let LayoutSystemKind::Scrolling(system) = self.workspace_tree(ws_id);
        system.scroll_offset(layout)
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
        let mut focus_window = preferred_focus_window.filter(|wid| {
            self.virtual_workspace_manager.workspace_for_window(window_store, space, *wid)
                == Some(workspace_id)
        });

        if focus_window.is_none() {
            focus_window = self
                .virtual_workspace_manager
                .last_focused_window(space, workspace_id)
                .filter(|wid| {
                    self.virtual_workspace_manager.workspace_for_window(window_store, space, *wid)
                        == Some(workspace_id)
                });
        }

        if focus_window.is_none() {
            if let Some(layout) = self.workspace_layouts.active(space, workspace_id) {
                let selected =
                    self.workspace_tree(workspace_id).selected_window(layout).filter(|wid| {
                        self.virtual_workspace_manager.workspace_for_window(
                            window_store,
                            space,
                            *wid,
                        ) == Some(workspace_id)
                    });
                let visible = self
                    .workspace_tree(workspace_id)
                    .visible_windows_in_layout(layout)
                    .into_iter()
                    .find(|wid| {
                        self.virtual_workspace_manager.workspace_for_window(
                            window_store,
                            space,
                            *wid,
                        ) == Some(workspace_id)
                    });
                focus_window = selected.or(visible);
            }
        }

        if focus_window.is_none() {
            let floating_windows = self.active_floating_windows_in_workspace(window_store, space);
            let floating_focus =
                self.floating.last_focus().filter(|wid| floating_windows.contains(wid));
            focus_window = floating_focus.or_else(|| floating_windows.first().copied());
        }

        focus_window
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
        self.broadcast_workspace_changed(space);
        self.broadcast_windows_changed(window_store, space);

        EventResponse {
            changed: true,
            focus_window: self.preferred_focus_for_workspace(
                window_store,
                space,
                workspace_id,
                preferred_focus_window,
            ),
            raise_windows: vec![],
            boundary_hit: None,
            edge_hit: None,
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
                // Check if workspace_auto_back_and_forth is enabled
                if self.virtual_workspace_manager.workspace_auto_back_and_forth() {
                    // Switch to last workspace instead
                    if let Some(last_workspace) =
                        self.virtual_workspace_manager.last_workspace(space)
                    {
                        return self.activate_workspace(window_store, space, last_workspace, None);
                    }
                }
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

    fn filter_active_workspace_windows(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        windows: Vec<WindowId>,
    ) -> Vec<WindowId> {
        windows
            .into_iter()
            .filter(|wid| self.is_window_in_active_workspace(window_store, space, *wid))
            .collect()
    }

    fn filter_active_workspace_window(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        window: Option<WindowId>,
    ) -> Option<WindowId> {
        window.filter(|wid| self.is_window_in_active_workspace(window_store, space, *wid))
    }

    pub fn resize_selection(
        &mut self,
        ws_id: VirtualWorkspaceId,
        layout: LayoutId,
        resize_amount: f64,
    ) {
        self.workspace_tree_mut(ws_id).resize_selection_by(
            layout,
            resize_amount,
            ResizeOrientation::Horizontal,
        );
    }

    fn apply_focus_response(
        &mut self,
        _window_store: &mut WindowStore,
        space: SpaceId,
        ws_id: VirtualWorkspaceId,
        layout: LayoutId,
        response: &EventResponse,
    ) {
        if let Some(wid) = response.focus_window {
            self.focused_window = Some(wid);
            if self.floating.is_floating(wid) {
                self.floating.set_last_focus(Some(wid));
            } else {
                let _ = self.workspace_tree_mut(ws_id).select_window(layout, wid);
                self.virtual_workspace_manager.set_last_focused_window(space, ws_id, Some(wid));
            }
        }
    }

    /// Move focus from the floating layer back into the tiled strip.
    ///
    /// Extracted so the "fell off the end of the floating set" path and the
    /// original "floating navigation could not proceed" path share one
    /// implementation instead of duplicating it.
    fn move_focus_escape_to_tiled(
        &mut self,
        window_store: &mut WindowStore,
        space: SpaceId,
        ws_id: VirtualWorkspaceId,
        layout: LayoutId,
    ) -> EventResponse {
        let tiled_windows = self.filter_active_workspace_windows(
            window_store,
            space,
            self.workspace_tree(ws_id).visible_windows_in_layout(layout),
        );
        if tiled_windows.is_empty() {
            return EventResponse::default();
        }
        // Resume at the strip's own selection, falling back to the first column only when the
        // strip has none. Taking `first()` unconditionally is why leaving a floating window
        // always jumped to the leftmost column instead of back to where the strip was.
        let focus_window = self
            .workspace_tree(ws_id)
            .selected_window(layout)
            .filter(|wid| tiled_windows.contains(wid))
            .or_else(|| tiled_windows.first().copied());
        let response = EventResponse {
            changed: true,
            focus_window,
            raise_windows: tiled_windows,
            boundary_hit: None,
            edge_hit: None,
        };
        self.apply_focus_response(window_store, space, ws_id, layout, &response);
        response
    }

    fn move_focus_internal(
        &mut self,
        window_store: &mut WindowStore,
        space: SpaceId,
        visible_spaces: &[SpaceId],
        visible_space_centers: &HashMap<SpaceId, CGPoint>,
        direction: Direction,
        is_floating: bool,
    ) -> EventResponse {
        let Some((ws_id, layout)) = self.workspace_and_layout(space) else {
            warn!(
                "No active workspace/layout for space {:?}; move_focus ignored",
                space
            );
            return EventResponse::default();
        };

        if is_floating {
            // Floating windows are not strip members: navigation moves to the strip and resumes at its
            // own selection. See "Navigation" in `docs/layout/strip.md`.
            return self.move_focus_escape_to_tiled(window_store, space, ws_id, layout);
        }

        let previous_selection = self.workspace_tree(ws_id).selected_window(layout);

        let (focus_window_raw, raise_windows) =
            self.workspace_tree_mut(ws_id).move_focus(layout, direction);
        let focus_window =
            self.filter_active_workspace_window(window_store, space, focus_window_raw);
        let raise_windows =
            self.filter_active_workspace_windows(window_store, space, raise_windows);
        if focus_window.is_some() {
            let response = EventResponse {
                changed: true,
                focus_window,
                raise_windows,
                boundary_hit: None,
                edge_hit: None,
            };
            self.apply_focus_response(window_store, space, ws_id, layout, &response);
            response
        } else {
            if let Some(prev_wid) = previous_selection {
                let _ = self.workspace_tree_mut(ws_id).select_window(layout, prev_wid);
            }
            // With isolate_displays set, horizontal focus stops at the ends of this
            // display's strip instead of continuing onto the neighbouring display,
            // so each display behaves as its own scrollable strip.
            //
            // Vertical navigation still crosses: up/down is not a strip axis, so
            // there is nothing to isolate there.
            let isolate_horizontal = self.layout_settings.scrolling.isolate_displays
                && matches!(direction, Direction::Left | Direction::Right);

            let adjacent_space = if isolate_horizontal {
                None
            } else {
                self.next_space_for_direction(
                    space,
                    direction,
                    visible_spaces,
                    visible_space_centers,
                )
            };

            if let Some(new_space) = adjacent_space {
                let Some((new_ws_id, new_layout)) = self.workspace_and_layout(new_space) else {
                    debug!(
                        "No active workspace/layout for adjacent space {:?}; skipping cross-space focus",
                        new_space
                    );
                    return EventResponse::default();
                };
                let windows_in_new_space = self.filter_active_workspace_windows(
                    window_store,
                    new_space,
                    self.workspace_tree(new_ws_id).visible_windows_in_layout(new_layout),
                );
                if let Some(target_window) = self
                    .filter_active_workspace_window(
                        window_store,
                        new_space,
                        self.workspace_tree(new_ws_id).window_in_direction(new_layout, direction),
                    )
                    .or_else(|| windows_in_new_space.first().copied())
                {
                    let _ =
                        self.workspace_tree_mut(new_ws_id).select_window(new_layout, target_window);
                    let response = EventResponse {
                        changed: true,
                        focus_window: Some(target_window),
                        raise_windows: windows_in_new_space,
                        boundary_hit: None,
                        edge_hit: None,
                    };
                    self.apply_focus_response(
                        window_store,
                        new_space,
                        new_ws_id,
                        new_layout,
                        &response,
                    );
                    return response;
                }
            }

            // The strip stops at its edge; it does not fall into the floating layer (`docs/layout/strip.md`).

            let visible_windows = self.filter_active_workspace_windows(
                window_store,
                space,
                self.workspace_tree(ws_id).visible_windows_in_layout(layout),
            );

            if let Some(fallback_focus) = self
                .filter_active_workspace_window(window_store, space, previous_selection)
                .or_else(|| visible_windows.first().copied())
            {
                // The strip stopped at its end. Up/down is not a strip axis (a stack's top is
                // not an edge of anything the view can bounce), so only left/right report it.
                let edge_hit =
                    matches!(direction, Direction::Left | Direction::Right).then_some(direction);
                let response = EventResponse {
                    changed: true,
                    focus_window: Some(fallback_focus),
                    raise_windows: vec![],
                    boundary_hit: None,
                    edge_hit,
                };
                self.apply_focus_response(window_store, space, ws_id, layout, &response);
                return response;
            }

            EventResponse::default()
        }
    }

    fn next_space_for_direction(
        &self,
        current_space: SpaceId,
        direction: Direction,
        visible_spaces: &[SpaceId],
        space_centers: &HashMap<SpaceId, CGPoint>,
    ) -> Option<SpaceId> {
        if visible_spaces.len() <= 1 {
            return None;
        }

        let current_center = space_centers.get(&current_space)?;
        let mut candidates = Vec::new();
        for &candidate_space in visible_spaces {
            if candidate_space == current_space {
                continue;
            }
            if let Some(candidate_center) = space_centers.get(&candidate_space) {
                if let Some(delta) =
                    Self::directional_delta(direction, current_center, candidate_center)
                {
                    candidates.push((candidate_space, delta));
                }
            }
        }

        if !candidates.is_empty() {
            candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
            return Some(candidates[0].0);
        }

        match direction {
            Direction::Left => {
                visible_spaces.iter().rev().copied().find(|&space| space != current_space)
            }
            Direction::Right => {
                visible_spaces.iter().copied().find(|&space| space != current_space)
            }
            Direction::Up | Direction::Down => None,
        }
    }

    fn directional_delta(
        direction: Direction,
        current: &CGPoint,
        candidate: &CGPoint,
    ) -> Option<f64> {
        match direction {
            Direction::Left => {
                let delta = current.x - candidate.x;
                if delta > 0.0 { Some(delta) } else { None }
            }
            Direction::Right => {
                let delta = candidate.x - current.x;
                if delta > 0.0 { Some(delta) } else { None }
            }
            Direction::Up => {
                let delta = candidate.y - current.y;
                if delta > 0.0 { Some(delta) } else { None }
            }
            Direction::Down => {
                let delta = current.y - candidate.y;
                if delta > 0.0 { Some(delta) } else { None }
            }
        }
    }

    fn remove_window_internal(
        &mut self,
        window_store: &mut WindowStore,
        wid: WindowId,
        preserve_floating: bool,
    ) {
        let removal = self.remove_window_layout_membership(window_store, wid);

        if preserve_floating {
            self.floating.remove_active_for_window(wid);
        } else {
            self.floating.remove_floating(wid);
        }

        if !preserve_floating {
            self.virtual_workspace_manager.remove_window(window_store, wid);
            self.floating_positions.remove_window(wid);
            self.forget_persisted_window(wid);
        }

        if self.focused_window == Some(wid) {
            self.focused_window = None;
        }
        self.window_layout_constraints.remove(&wid);

        if let Some(space) = removal.active_space {
            self.broadcast_windows_changed(window_store, space);
        }
    }

    fn remove_window_layout_membership(
        &mut self,
        window_store: &WindowStore,
        wid: WindowId,
    ) -> WindowRemovalImpact {
        let active_space = self.space_with_window(wid);
        let tiled_workspaces =
            self.virtual_workspace_manager.workspaces_for_window(window_store, wid);

        if !tiled_workspaces.is_empty() {
            for ws_id in &tiled_workspaces {
                self.workspace_tree_mut(*ws_id).remove_window(wid);
            }
            return WindowRemovalImpact { active_space };
        }

        // The store may already have dropped the record (for example after
        // WindowDestroyed). Layout membership is only a projection, so scrub
        // every tree when its authoritative assignment is unavailable.
        let ws_ids: Vec<_> = self.virtual_workspace_manager.workspaces.keys().collect();
        for ws_id in ws_ids {
            self.workspace_tree_mut(ws_id).remove_window(wid);
        }
        WindowRemovalImpact { active_space }
    }

    fn add_window_to_layout(
        &mut self,
        window_store: &mut WindowStore,
        space: SpaceId,
        wid: WindowId,
    ) -> bool {
        let active_space_before = self.space_with_window(wid);

        // Where this application's windows were the last time this set of displays was attached. Only
        // consulted for a window with no workspace yet, so it cannot override an explicit rule or a
        // window rini has already placed. See `docs/workspaces/launch-memory.md` (in this crate).
        let remembered = self
            .virtual_workspace_manager
            .workspace_for_window(window_store, space, wid)
            .is_none()
            .then(|| {
                let state = window_store.window(wid)?;
                let app_id = state.info.bundle_id.clone()?;
                let title = state.info.title.clone();
                self.launch_slot_for_new_window(window_store, wid, &app_id, Some(title.as_str()))
            })
            .flatten();
        if let Some(slot) = &remembered {
            let workspaces = self.virtual_workspace_manager.list_workspaces(space);
            if let Some((workspace_id, _)) = workspaces.get(slot.workspace_index) {
                let workspace_id = *workspace_id;
                self.virtual_workspace_manager
                    .assign_window_to_workspace(window_store, space, wid, workspace_id);
            }
            self.display_affinity.set_window_home_if_absent(wid, &slot.display_uuid);
            if let Some(width) = slot.width {
                self.display_affinity.set_window_width(&slot.display_uuid, wid, width);
            }
        }

        let assigned_workspace =
            match self.virtual_workspace_manager.workspace_for_window(window_store, space, wid) {
                Some(workspace_id) => workspace_id,
                None => match self.virtual_workspace_manager.auto_assign_window(
                    window_store,
                    wid,
                    space,
                ) {
                    Ok(workspace_id) => workspace_id,
                    Err(e) => {
                        warn!("Failed to auto-assign window to workspace: {:?}", e);
                        self.virtual_workspace_manager
                            .active_workspace(space)
                            .expect("No active workspace available")
                    }
                },
            };

        // Establish a home for a window Rini has not placed before. Absent-only, so the
        // reassignment that follows an unplug cannot overwrite the home a window already
        // has — that record is what brings it back when its display returns.
        self.note_window_display_home(wid, space);

        let should_be_floating = self.floating.is_floating(wid);

        if should_be_floating {
            self.floating.add_active(space, wid.pid, wid);
        } else if let Some(layout) = self.workspace_layouts.active(space, assigned_workspace) {
            if !self.workspace_tree(assigned_workspace).contains_window(layout, wid) {
                self.workspace_tree_mut(assigned_workspace)
                    .add_window_after_selection(layout, wid);
                // A fresh column starts at the display's default ratio, so re-apply whatever this
                // window last had here. Without this a relaunched window was recorded and looked up
                // correctly and still opened at the default: the width reached the affinity record and
                // never reached the layout, which is the thing that decides how wide the column is.
                self.apply_remembered_column_width(space, assigned_workspace, layout, wid);
            }
        } else {
            warn!(
                "No active layout for workspace {:?} on space {:?}; window {:?} not added to tree",
                assigned_workspace, space, wid
            );
        }

        self.space_with_window(wid) != active_space_before
    }

    fn remove_window_from_all_tiling_trees(&mut self, wid: WindowId) {
        let ws_ids: Vec<_> = self.virtual_workspace_manager.workspaces.keys().collect();
        for ws_id in ws_ids {
            self.workspace_tree_mut(ws_id).remove_window(wid);
        }
    }

    fn space_with_window(&self, wid: WindowId) -> Option<SpaceId> {
        for space in self.workspace_layouts.spaces() {
            if let Some(ws_id) = self.virtual_workspace_manager.active_workspace(space) {
                if let Some(layout) = self.workspace_layouts.active(space, ws_id) {
                    if self.workspace_tree(ws_id).contains_window(layout, wid) {
                        return Some(space);
                    }
                }
            }

            if self.floating.active_flat(space).contains(&wid) {
                return Some(space);
            }
        }
        None
    }

    fn active_workspace_id_and_name(
        &self,
        space_id: SpaceId,
    ) -> Option<(crate::workspaces::VirtualWorkspaceId, String)> {
        let workspace_id = self.virtual_workspace_manager.active_workspace(space_id)?;
        let workspace_name = self
            .virtual_workspace_manager
            .workspace_info(space_id, workspace_id)
            .map(|ws| ws.name.clone())
            .unwrap_or_else(|| format!("Workspace {:?}", workspace_id));
        Some((workspace_id, workspace_name))
    }

    fn window_no_longer_assigned_to_space(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        wid: WindowId,
    ) -> bool {
        self.virtual_workspace_manager
            .workspace_for_window(window_store, space, wid)
            .is_none()
    }

    fn sync_tiled_windows_for_app(
        &mut self,
        window_store: &WindowStore,
        space: SpaceId,
        pid: pid_t,
        tiled_by_workspace: &HashMap<crate::workspaces::VirtualWorkspaceId, Vec<WindowId>>,
    ) -> Vec<(crate::workspaces::VirtualWorkspaceId, LayoutId)> {
        let total_tiled_count: usize = tiled_by_workspace.values().map(|v| v.len()).sum();
        let mut changed_layouts = Vec::new();

        for (ws_id, layout) in self.workspace_layouts.active_layouts_for_space(space) {
            let mut desired = tiled_by_workspace.get(&ws_id).cloned().unwrap_or_default();
            for wid in self.virtual_workspace_manager.workspace_windows(window_store, space, ws_id)
            {
                let authoritative_native_space =
                    window_store.current_window_server_space_for_window(wid);
                // Skip re-adding if the VWM no longer assigns this window to this space
                // (it was moved to another space during this discovery cycle).
                if wid.pid != pid
                    || self.floating.is_floating(wid)
                    || desired.contains(&wid)
                    || authoritative_native_space.is_some_and(|native_space| native_space != space)
                    || self.window_no_longer_assigned_to_space(window_store, space, wid)
                {
                    continue;
                }
                desired.push(wid);
            }

            if desired.is_empty() && total_tiled_count == 0 {
                // Empty discovery can mean AX temporarily omitted the app. Preserve
                // windows still assigned to this workspace, but allow moved windows
                // to be removed from this layout tree.
                let tree_windows = self.workspace_tree(ws_id).windows_for_app(layout, pid);
                desired = tree_windows
                    .into_iter()
                    .filter(|wid| {
                        self.virtual_workspace_manager.workspace_for_window(
                            window_store,
                            space,
                            *wid,
                        ) == Some(ws_id)
                    })
                    .collect();
            }

            desired.sort_unstable();
            let mut current = self.workspace_tree(ws_id).windows_for_app(layout, pid);
            current.sort_unstable();

            // AX/window-server discovery can temporarily omit windows. Keep windows that are
            // still assigned to this workspace so a partial snapshot does not tear them out
            // of the tree and cause their sibling weights to be rebuilt.
            for wid in current.iter().copied() {
                if !desired.contains(&wid)
                    && !self.floating.is_floating(wid)
                    && self.virtual_workspace_manager.workspace_for_window(window_store, space, wid)
                        == Some(ws_id)
                {
                    desired.push(wid);
                }
            }
            desired.sort_unstable();
            if desired == current {
                continue;
            }

            // Per-app membership reconciliation is not a focus operation.
            // Several layout systems select newly inserted windows as part of
            // their normal insertion semantics, so preserve the selection
            // explicitly across discovery-driven synchronization.
            let selected_window = self.workspace_tree(ws_id).selected_window(layout);
            // Windows entering the tree here, which is where a launching application's windows arrive.
            // A fresh column starts at the display's default ratio, so each one needs whatever it last
            // had re-applied — the record alone does not decide how wide a column is.
            let arriving: Vec<WindowId> =
                desired.iter().copied().filter(|wid| !current.contains(wid)).collect();
            self.workspace_tree_mut(ws_id).set_windows_for_app(layout, pid, desired);
            for wid in arriving {
                self.apply_remembered_column_width(space, ws_id, layout, wid);
            }
            if let Some(selected_window) = selected_window
                && self.workspace_tree(ws_id).contains_window(layout, selected_window)
            {
                let _ = self.workspace_tree_mut(ws_id).select_window(layout, selected_window);
            }
            changed_layouts.push((ws_id, layout));
        }

        changed_layouts
    }

    pub fn update_space_display(&mut self, space: SpaceId, display_uuid: Option<String>) {
        if let Some(uuid) = display_uuid {
            self.display_affinity.set_display_space(&uuid, space);
        }
    }

    pub fn last_space_for_display_uuid(&self, display_uuid: &str) -> Option<SpaceId> {
        self.display_affinity.space_for_display(display_uuid)
    }

    pub fn display_seen_before(&self, display_uuid: &str) -> bool {
        self.display_affinity.knows_display(display_uuid)
    }

    fn display_uuid_for_space(&self, space: SpaceId) -> Option<String> {
        self.display_affinity.display_for_space(space).map(str::to_owned)
    }

    /// Returns the last known space associated with the given display UUID.
    /// Useful when the OS recreates spaces (e.g. after sleep/resume) and we
    /// want to migrate layout state to the new space id.
    pub fn space_for_display_uuid(&self, display_uuid: &str) -> Option<SpaceId> {
        self.display_affinity.space_for_display(display_uuid)
    }

    pub fn display_affinity(&self) -> &DisplayAffinity {
        &self.display_affinity
    }

    /// Record that `window` belongs to the display currently owning `space`.
    ///
    /// Only call this from paths that express intent — an explicit move, or the first
    /// time a window is seen. The forced reassignment that follows a display change must
    /// NOT call it: an unplug evacuates windows onto the remaining display, and recording
    /// that as their home destroys the record needed to bring them back on replug.
    pub fn set_window_display_home(&mut self, window: WindowId, space: SpaceId) {
        if let Some(display) = self.display_affinity.display_for_space(space) {
            let display = display.to_owned();
            self.display_affinity.set_window_home(window, &display);
        }
    }

    /// Record a home for a newly seen window, leaving an existing home untouched.
    /// Projects live windows into the launch memory, so their next launch can find them.
    ///
    /// A projection computed before each save rather than hooks on every move and resize: one write
    /// path, and no new work in the paths that place windows. See `docs/workspaces/launch-memory.md` (in this crate).
    ///
    /// `connected` is the display UUIDs currently attached, which is the topology the answer is filed
    /// under. Nothing is recorded without it, since an answer with no topology cannot be looked up.
    pub fn remember_launch_slots(&mut self, window_store: &WindowStore, connected: &[String]) {
        use crate::workspaces::domain::launch_memory::{Slot, topology_key};

        if connected.is_empty() {
            return;
        }
        let topology = topology_key(connected);

        // Grouped by bundle identifier, ordered by window server id, which is creation order. A
        // relaunched application creates its windows in the same order, so the Nth window can find the
        // Nth slot when its title is no help.
        let mut by_app: HashMap<String, Vec<(u32, WindowId)>> = HashMap::default();
        for (window_id, window) in window_store.iter_windows() {
            let Some(app_id) = window.info.bundle_id.clone() else { continue };
            by_app.entry(app_id).or_default().push((window_id.idx.get(), window_id));
        }

        for (app_id, mut windows) in by_app {
            windows.sort_unstable();
            let slots: Vec<Slot> = windows
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
                    let display = self
                        .display_affinity
                        .window_home(window_id)
                        .or_else(|| self.display_affinity.display_for_space(info.space))?
                        .to_owned();
                    let index = self
                        .virtual_workspace_manager
                        .list_workspaces(info.space)
                        .iter()
                        .position(|(id, _)| *id == info.workspace_id)?;
                    // The width the window actually has, from its layout. The affinity map alone missed
                    // widths that came from the layout rather than a command.
                    let width = self
                        .workspace_layouts
                        .active(info.space, info.workspace_id)
                        .and_then(|layout| {
                            let tree = self.workspace_tree(info.workspace_id);
                            if tree.is_window_full_width(layout, window_id) {
                                Some(ColumnWidth::FullWidth)
                            } else {
                                tree.column_width_offset(layout, window_id).map(ColumnWidth::Offset)
                            }
                        })
                        .or_else(|| self.display_affinity.window_width(&display, window_id));
                    Some(Slot {
                        title: (!window.info.title.trim().is_empty())
                            .then(|| window.info.title.clone()),
                        display_uuid: display,
                        workspace_index: index,
                        width,
                    })
                })
                .collect();
            self.launch_memory.remember(&app_id, &topology, slots);
        }
    }

    /// The displays attached right now, which is the topology the launch memory is keyed by.
    pub fn set_connected_displays(&mut self, uuids: Vec<String>) {
        self.connected_displays = uuids;
    }

    /// The slot a window appearing for the first time should take, if its application left one.
    ///
    /// The window's position among its application's windows, and which slots its siblings already took,
    /// are recomputed from the siblings themselves rather than tracked. Placement order is window server
    /// id order, so replaying the same matching over the siblings gives the same answers it gave them,
    /// and two windows cannot land on one slot.
    fn launch_slot_for_new_window(
        &self,
        window_store: &WindowStore,
        window: WindowId,
        app_id: &str,
        title: Option<&str>,
    ) -> Option<crate::workspaces::domain::launch_memory::Slot> {
        use crate::workspaces::domain::launch_memory::{slot_for_window, topology_key};

        // Identity comes from the caller, not from the window store. The first sighting of a launching
        // application's window happens while the rules are being applied, and the window is not in the
        // store yet at that point — reading it there is why this silently did nothing.
        let topology = topology_key(&self.connected_displays);
        let slots = self.launch_memory.slots(app_id, &topology);
        if slots.is_empty() {
            return None;
        }

        // Siblings already placed, in the order they were placed.
        let mut siblings: Vec<(u32, WindowId)> = window_store
            .iter_windows()
            .filter(|(other, state)| {
                *other != window
                    && state.info.bundle_id.as_deref() == Some(app_id)
                    && self.display_affinity.window_home(*other).is_some()
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

        let index = slot_for_window(slots, title, siblings.len(), &claimed)?;
        debug!(
            idx = window.idx.get(),
            app_id,
            slot = index,
            of = slots.len(),
            "a relaunched window matched a remembered slot"
        );
        slots.get(index).cloned()
    }

    pub fn note_window_display_home(&mut self, window: WindowId, space: SpaceId) {
        if let Some(display) = self.display_affinity.display_for_space(space) {
            let display = display.to_owned();
            self.display_affinity.set_window_home_if_absent(window, &display);
        }
    }

    /// Re-observe where windows are, and in what order, for one attached display on a settled
    /// topology. `live_windows` is the display's strip in visual order. Windows homed to a
    /// detached display keep that home (evacuation). See `docs/workspaces/workspaces-and-displays.md`.
    pub fn sync_display_affinity(
        &mut self,
        display_uuid: &str,
        live_windows: &[WindowId],
        attached_displays: &[String],
    ) {
        let attached: HashSet<&str> = attached_displays.iter().map(String::as_str).collect();
        for window in live_windows {
            let keeps_absent_home = self
                .display_affinity
                .window_home(*window)
                .is_some_and(|home| home != display_uuid && !attached.contains(home));
            if !keeps_absent_home {
                self.display_affinity.set_window_home(*window, display_uuid);
            }
        }

        // The strip is the arrangement to rebuild on replug, so it must reflect only the
        // windows that genuinely live here. A window parked here while its own display is
        // unplugged would otherwise be recorded as part of this display's layout.
        let strip: Vec<WindowId> = live_windows
            .iter()
            .copied()
            .filter(|window| self.display_affinity.window_home(*window) == Some(display_uuid))
            .collect();
        self.display_affinity.set_display_strip(display_uuid, strip);
    }

    /// Windows homed to `display_uuid`, in the order last seen on it.
    pub fn windows_homed_to_display(&self, display_uuid: &str) -> Vec<WindowId> {
        self.display_affinity.windows_homed_to(display_uuid)
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

    pub fn window_display_home(&self, window: WindowId) -> Option<&str> {
        self.display_affinity.window_home(window)
    }

    /// Record the width `window` now occupies on the display owning `space`.
    ///
    /// Called after any command that deliberately sets a width. Width is remembered per
    /// DISPLAY, so this is the only place the two are tied together; see
    /// `DisplayAffinity::window_width` for why the layout tree is the wrong home for it.
    fn remember_column_width(
        &mut self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        layout: LayoutId,
        window: WindowId,
    ) {
        let Some(display) = self.display_affinity.display_for_space(space).map(str::to_owned)
        else {
            return;
        };
        let tree = self.workspace_tree(workspace_id);
        if tree.is_window_full_width(layout, window) {
            self.display_affinity.set_window_width(&display, window, ColumnWidth::FullWidth);
            return;
        }
        match tree.column_width_offset(layout, window) {
            Some(offset) => {
                self.display_affinity.set_window_width(
                    &display,
                    window,
                    ColumnWidth::Offset(offset),
                );
            }
            // Toggled back to the default. Forget rather than pin, so the window follows
            // each display's configured ratio again.
            None => self.display_affinity.clear_window_width(&display, window),
        }
    }

    /// Record the width of whichever column the resize commands just acted on.
    ///
    /// Those commands operate on the selection and report nothing back, so the window has to
    /// be looked up rather than taken from a return value.
    fn remember_selected_column_width(
        &mut self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        layout: LayoutId,
    ) {
        let Some(window) = self.workspace_tree(workspace_id).selected_window(layout) else {
            return;
        };
        self.remember_column_width(space, workspace_id, layout, window);
    }

    /// Re-apply the width `window` last had on the display owning `space`.
    ///
    /// Called when a window ARRIVES in a strip — a new workspace, or a new display. A fresh
    /// column starts at the configured default, so without this a window sized to fill the
    /// built-in lost that size the moment it changed workspace.
    fn apply_remembered_column_width(
        &mut self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        layout: LayoutId,
        window: WindowId,
    ) {
        let Some(display) = self.display_affinity.display_for_space(space).map(str::to_owned)
        else {
            return;
        };
        let found = self.display_affinity.window_width(&display, window);
        let on_display = display.clone();
        debug!(idx = window.idx.get(), on_display, width = ?found, "applying a remembered width");
        let Some(width) = found else {
            return;
        };
        match width {
            ColumnWidth::FullWidth => {
                self.workspace_tree_mut(workspace_id)
                    .set_window_full_width(layout, window, true);
            }
            ColumnWidth::Offset(offset) => {
                self.workspace_tree_mut(workspace_id)
                    .set_column_width_offset(layout, window, offset);
            }
        }
    }

    /// Windows that belong on `display` but are currently assigned somewhere else.
    ///
    /// This is what a replug consults. It deliberately reports only windows whose current
    /// assignment disagrees with their home, so a display that came back to find its own
    /// windows already in place produces no moves at all.
    pub fn windows_to_repatriate(
        &self,
        window_store: &WindowStore,
        display_uuid: &str,
        target_space: SpaceId,
    ) -> Vec<WindowId> {
        self.display_affinity
            .windows_homed_to(display_uuid)
            .into_iter()
            .filter(|window| {
                window_store
                    .workspace_info_for_window(*window)
                    .is_some_and(|assignment| assignment.space != target_space)
            })
            .collect()
    }

    /// Drop affinity for windows that no longer exist. Called on every settled topology; the
    /// removal paths alone missed windows closed while their display was unplugged.
    pub fn forget_affinity_for_dead_windows(&mut self, window_store: &WindowStore) {
        let stale: Vec<WindowId> = self
            .display_affinity
            .homed_windows()
            .into_iter()
            .filter(|window| !window_store.contains_window(*window))
            .collect();
        for window in stale {
            self.display_affinity.forget_window(window);
        }
    }

    /// Move all per-space layout state from `old_space` to `new_space`.
    pub fn remap_space(
        &mut self,
        window_store: &mut WindowStore,
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
        self.display_affinity.remap_space(old_space, new_space);
    }

    pub fn new(
        virtual_workspace_config: &crate::workspaces::settings::VirtualWorkspaceSettings,
        layout_settings: &LayoutSettings,
        broadcast_tx: Option<BroadcastSender>,
    ) -> Self {
        let virtual_workspace_manager =
            WorkspaceStore::new_with_config(virtual_workspace_config, layout_settings);

        LayoutEngine {
            workspace_layouts: WorkspaceLayouts::default(),
            floating: FloatingManager::new(),
            floating_positions: FloatingPositionStore::default(),
            app_rules: AppRuleEngine::new(&virtual_workspace_config.app_rules, virtual_workspace_config.float_modal_windows),
            focused_window: None,
            window_layout_constraints: HashMap::default(),
            virtual_workspace_manager,
            layout_settings: layout_settings.clone(),
            broadcast_tx,
            display_affinity: DisplayAffinity::default(),
            launch_memory: crate::workspaces::domain::launch_memory::LaunchMemory::default(),
            connected_displays: Vec::new(),
            persistence: PersistenceState::default(),
            startup_restore_pending: false,
        }
    }

    fn apply_app_rule_outcome(
        &mut self,
        window_store: &mut WindowStore,
        window: WindowId,
        space: SpaceId,
        was_floating: bool,
        result: AppRuleResult,
        app_rule_outcome: &mut AppRuleOutcome,
        windows_by_workspace: &mut HashMap<VirtualWorkspaceId, Vec<WindowId>>,
    ) -> Option<(WindowId, VirtualWorkspaceId)> {
        let AppRuleResult::Managed(effects) = result else {
            return None;
        };
        let should_float = effects.should_float(was_floating);
        if should_float {
            self.floating.add_floating(window);
            self.floating.add_active(space, window.pid, window);
        } else if was_floating {
            self.floating.remove_floating(window);
        }

        if let Some(placement) = effects.floating_placement(window, space) {
            app_rule_outcome.push_placement(placement);
        } else if let Some(resize) = effects.tiled_resize(window, space, was_floating) {
            app_rule_outcome.push_resize(resize);
        }

        if !should_float {
            windows_by_workspace.entry(effects.workspace_id).or_default().push(window);
        }

        self.virtual_workspace_manager_mut().set_last_rule_decision(
            window_store,
            space,
            window,
            effects.floating,
        );

        effects.focus.then_some((window, effects.workspace_id))
    }

    pub fn apply_app_rule_resize(
        &mut self,
        resize: AppRuleResize,
        old_frame: CGRect,
        new_frame: CGRect,
        screen_frame: CGRect,
        display_uuid: Option<&str>,
    ) {
        let Some(layout) = self.workspace_layouts.active(resize.space, resize.workspace_id) else {
            return;
        };
        let gaps = self.layout_settings.gaps.effective_for_display(display_uuid);
        let previous_selection = self.workspace_tree(resize.workspace_id).selected_window(layout);
        let tree = self.workspace_tree_mut(resize.workspace_id);
        let _ = tree.select_window(layout, resize.window);
        tree.on_window_resized(layout, resize.window, old_frame, new_frame, screen_frame, &gaps);
        if let Some(previous) = previous_selection.filter(|window| *window != resize.window) {
            let _ = tree.select_window(layout, previous);
        }
        self.workspace_layouts
            .mark_last_saved(resize.space, resize.workspace_id, layout);
    }

    pub fn debug_tree(&self, space: SpaceId) {
        self.debug_tree_desc(space, "", false);
    }

    pub fn debug_tree_desc(&self, space: SpaceId, desc: &'static str, print: bool) {
        if let Some(workspace_id) = self.virtual_workspace_manager.active_workspace(space) {
            if let Some(layout) = self.workspace_layouts.active(space, workspace_id) {
                if print {
                    println!(
                        "Tree {desc}\n{}",
                        self.workspace_tree(workspace_id).draw_tree(layout).trim()
                    );
                } else {
                    debug!(
                        "Tree {desc}\n{}",
                        self.workspace_tree(workspace_id).draw_tree(layout).trim()
                    );
                }
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
        event: LayoutEvent,
    ) -> LayoutEventOutcome {
        let mut app_rules = AppRuleOutcome::default();
        let response = self.handle_event_inner(window_store, event, &mut app_rules);
        LayoutEventOutcome { response, app_rules }
    }

    /// Every window an app has on a space, as the app itself reports them.
    ///
    /// The authoritative list: windows missing from it are gone, and windows in it rini has not
    /// placed are new. App rules are applied here, because this is where a window first arrives with
    /// its bundle id and title known.
    fn on_windows_on_screen_updated(
        &mut self,
        window_store: &mut WindowStore,
        space: SpaceId,
        pid: pid_t,
        windows_with_titles: Vec<OnScreenEntry>,
        app_info: Option<AppInfo>,
        app_rule_outcome: &mut AppRuleOutcome,
    ) -> EventResponse {
                self.debug_tree(space);
                self.floating.clear_active_for_app(space, pid);

                let mut windows_by_workspace: HashMap<
                    crate::workspaces::VirtualWorkspaceId,
                    Vec<WindowId>,
                > = HashMap::default();

                let (app_bundle_id, app_name) = match app_info.as_ref() {
                    Some(info) => (info.bundle_id.as_deref(), info.localized_name.as_deref()),
                    None => (None, None),
                };
                let mut focus_request = None;

                for (
                    wid,
                    title_opt,
                    ax_role_opt,
                    ax_subrole_opt,
                    is_modal,
                    is_resizable,
                    size_hint,
                    min_size,
                    max_size,
                ) in windows_with_titles
                {
                    self.observe_window_for_persistence(
                        window_store,
                        space,
                        wid,
                        title_opt.as_deref(),
                        size_hint,
                        app_bundle_id,
                    );

                    self.window_layout_constraints.insert(
                        wid,
                        WindowLayoutConstraints {
                            is_resizable,
                            locked_width: size_hint.width,
                            locked_height: size_hint.height,
                            min_width: min_size.map_or(0.0, |s| s.width),
                            min_height: min_size.map_or(0.0, |s| s.height),
                            max_width: max_size.map_or(0.0, |s| s.width),
                            max_height: max_size.map_or(0.0, |s| s.height),
                        }
                        .normalized(),
                    );

                    let title_ref = title_opt.as_deref();
                    let ax_role_ref = ax_role_opt.as_deref();
                    let ax_subrole_ref = ax_subrole_opt.as_deref();

                    let was_floating = self.floating.is_floating(wid);
                    let outcome = match self.assign_window_with_app_info(
                        window_store,
                        wid,
                        space,
                        app_bundle_id,
                        app_name,
                        title_ref,
                        ax_role_ref,
                        ax_subrole_ref,
                        is_modal,
                    ) {
                        Ok(outcome) => outcome,
                        Err(_) => {
                            match self.virtual_workspace_manager.auto_assign_window(
                                window_store,
                                wid,
                                space,
                            ) {
                                Ok(ws) => AppRuleResult::Managed(AppRuleEffects {
                                    workspace_id: ws,
                                    floating: was_floating,
                                    position: None,
                                    size: None,
                                    focus: false,
                                    prev_rule_decision: false,
                                }),
                                Err(_) => {
                                    warn!(
                                        "Could not determine workspace for window {:?} on space {:?}; skipping assignment",
                                        wid, space
                                    );
                                    continue;
                                }
                            }
                        }
                    };

                    if let Some(request) = self.apply_app_rule_outcome(
                        window_store,
                        wid,
                        space,
                        was_floating,
                        outcome,
                        app_rule_outcome,
                        &mut windows_by_workspace,
                    ) {
                        focus_request = Some(request);
                    }
                }

                // `windows_by_workspace` already excludes floating windows.
                let tiled_by_workspace = windows_by_workspace;
                let changed_layouts =
                    self.sync_tiled_windows_for_app(window_store, space, pid, &tiled_by_workspace);
                if !changed_layouts.is_empty() {
                    self.broadcast_windows_changed(window_store, space);
                }

                if let Some((window, workspace)) = focus_request {
                    let workspace_index = self
                        .virtual_workspace_manager_mut()
                        .list_workspaces(space)
                        .iter()
                        .position(|(id, _)| *id == workspace);
                    if let Some(workspace_index) = workspace_index
                        && self.virtual_workspace_manager.active_workspace(space) != Some(workspace)
                    {
                        app_rule_outcome.set_workspace_focus(AppRuleWorkspaceFocus {
                            window,
                            space,
                            workspace_index,
                        });
                        return EventResponse::default();
                    }
                    self.commit_workspace_focus(window_store, space, Some(window));
                    return EventResponse {
                        changed: app_rule_outcome.has_resizes(),
                        raise_windows: vec![window],
                        focus_window: Some(window),
                        boundary_hit: None,
                        edge_hit: None,
                    };
                }
                if app_rule_outcome.has_resizes() {
                    return EventResponse {
                        changed: true,
                        ..EventResponse::default()
                    };
                }
        EventResponse::default()
    }

    fn handle_event_inner(
        &mut self,
        window_store: &mut WindowStore,
        event: LayoutEvent,
        app_rule_outcome: &mut AppRuleOutcome,
    ) -> EventResponse {
        debug!(?event);
        match event {
            LayoutEvent::SpaceExposed(space, size) => {
                self.debug_tree(space);

                let workspaces =
                    self.virtual_workspace_manager_mut().list_workspaces(space).to_vec();
                for (id, _) in workspaces {
                    let tree = &mut self.virtual_workspace_manager.workspaces[id].layout_system;
                    self.workspace_layouts.ensure_active_for_workspace(space, size, id, tree);
                }
            }
            LayoutEvent::WindowsOnScreenUpdated(space, pid, windows_with_titles, app_info) => {
                return self.on_windows_on_screen_updated(
                    window_store,
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
            LayoutEvent::AppClosed(pid) => {
                for (_, ws) in self.virtual_workspace_manager.workspaces.iter_mut() {
                    ws.layout_system.remove_windows_for_app(pid);
                }
                self.floating.remove_all_for_pid(pid);
                self.window_layout_constraints.retain(|wid, _| wid.pid != pid);
                self.forget_persisted_app(pid);

                self.virtual_workspace_manager.remove_windows_for_app(window_store, pid);
                self.floating_positions.remove_app(pid);
            }
            LayoutEvent::WindowAdded(space, wid) => {
                self.debug_tree(space);
                if self.add_window_to_layout(window_store, space, wid) {
                    self.broadcast_windows_changed(window_store, space);
                }
            }
            LayoutEvent::WindowRemoved(wid) => {
                self.remove_window_internal(window_store, wid, false);
            }
            LayoutEvent::WindowRemovedPreserveFloating(wid) => {
                self.remove_window_internal(window_store, wid, true);
            }
            LayoutEvent::WindowFocused(space, wid) => {
                if self.floating.is_floating(wid) {
                    self.focused_window = Some(wid);
                    self.floating.set_last_focus(Some(wid));
                } else if let Some((ws_id, layout)) = self.workspace_and_layout(space) {
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
                    return EventResponse {
                        changed: true,
                        ..EventResponse::default()
                    };
                } else {
                    warn!(
                        "No active workspace/layout for focused window {:?} on space {:?}",
                        wid, space
                    );
                }
            }
            LayoutEvent::WindowResized {
                wid,
                old_frame,
                new_frame,
                screens,
            } => {
                for (space, screen_frame, display_uuid) in screens {
                    let Some((ws_id, layout)) = self.workspace_and_layout(space) else {
                        debug!(
                            "No active workspace/layout for resized window {:?} on space {:?}; skipping",
                            wid, space
                        );
                        continue;
                    };
                    let gaps =
                        self.layout_settings.gaps.effective_for_display(display_uuid.as_deref());
                    self.workspace_tree_mut(ws_id).on_window_resized(
                        layout,
                        wid,
                        old_frame,
                        new_frame,
                        screen_frame,
                        &gaps,
                    );

                    self.workspace_layouts.mark_last_saved(space, ws_id, layout);
                }
            }
        }
        EventResponse::default()
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

    /// Moves the focused window between the strip and the floating layer.
    fn toggle_window_floating(
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
                    let assigned_workspace = self
                        .virtual_workspace_manager
                        .workspace_for_window(window_store, space, wid)
                        .unwrap_or_else(|| {
                            self.virtual_workspace_manager
                                .active_workspace(space)
                                .expect("No active workspace available")
                        });

                    if let Some(layout) = self.workspace_layouts.active(space, assigned_workspace) {
                        self.workspace_tree_mut(assigned_workspace)
                            .add_window_after_selection(layout, wid);
                        debug!(
                            "Re-added floating window {:?} to tiling tree in workspace {:?}",
                            wid, assigned_workspace
                        );
                    }

                    self.floating.remove_active(space, wid.pid, wid);
                }
                self.floating.remove_floating(wid);
                self.floating.set_last_focus(None);
            } else {
                if let Some(space) = space {
                    self.floating.add_active(space, wid.pid, wid);
                    if let Some((ws_id, _)) = self.workspace_and_layout(space) {
                        self.workspace_tree_mut(ws_id).remove_window(wid);
                    } else {
                        debug!(
                            "No active workspace/layout for space {:?}; leaving window {:?} out of tiling removal",
                            space, wid
                        );
                    }
                }
                self.floating.add_floating(wid);
                self.floating.set_last_focus(Some(wid));
                debug!("Removed window {:?} from tiling tree, now floating", wid);
            }
            return EventResponse::default();
    }

    /// Fullscreens a FLOATING window, or takes it back out. `ToggleFullscreenWithinGaps` keeps the
    /// layout's gaps around the window rather than covering the whole display.
    ///
    /// A tiled window does not come here: its own arm in `handle_command` handles it, which is why
    /// the caller tests `is_floating` before calling rather than this deciding.
    fn toggle_floating_fullscreen(
        &mut self,
        window_store: &WindowStore,
        space: Option<SpaceId>,
        command: &LayoutCommand,
    ) -> EventResponse {
            let Some(wid) = self.focused_window else {
                return EventResponse::default();
            };
            let target = match command {
                LayoutCommand::ToggleFullscreenWithinGaps => FloatingFullscreenKind::WithinGaps,
                _ => FloatingFullscreenKind::Full,
            };
            if self.floating.fullscreen_kind(wid) == Some(target) {
                self.floating.set_fullscreen(wid, None);
            } else {
                // Only save the pre-fullscreen frame when switching from a non-fullscreen state,
                // and _not_ when switching between fullscreen kinds.
                if self.floating.fullscreen_kind(wid).is_none()
                    && let Some(space) = space
                {
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
                self.floating.set_fullscreen(wid, Some(target));
            }
            return EventResponse {
                changed: true,
                raise_windows: vec![wid],
                focus_window: Some(wid),
                boundary_hit: None,
                edge_hit: None,
            };
    }

    /// Moves focus between the strip and the floating layer, without moving any window.
    fn toggle_focus_floating(
        &mut self,
        window_store: &mut WindowStore,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        layout: LayoutId,
        is_floating: bool,
    ) -> EventResponse {
            if is_floating {
                let selection = self.workspace_tree(workspace_id).selected_window(layout);
                let mut raise_windows =
                    self.workspace_tree(workspace_id).visible_windows_in_layout(layout);
                let focus_window = selection.or_else(|| raise_windows.pop());
                let response = EventResponse {
                    changed: true,
                    raise_windows,
                    focus_window,
                    boundary_hit: None,
                    edge_hit: None,
                };
                self.apply_focus_response(window_store, space, workspace_id, layout, &response);
                return response;
            } else {
                let floating_windows: Vec<WindowId> =
                    self.active_floating_windows_in_workspace(window_store, space);
                let mut raise_windows: Vec<_> = floating_windows
                    .iter()
                    .copied()
                    .filter(|wid| Some(*wid) != self.floating.last_focus())
                    .collect();
                let focus_window = self.floating.last_focus().or_else(|| raise_windows.pop());
                let response = EventResponse {
                    changed: true,
                    raise_windows,
                    focus_window,
                    boundary_hit: None,
                    edge_hit: None,
                };
                self.apply_focus_response(window_store, space, workspace_id, layout, &response);
                return response;
            }
    }

    pub fn handle_command(
        &mut self,
        window_store: &mut WindowStore,
        space: Option<SpaceId>,
        visible_spaces: &[SpaceId],
        visible_space_centers: &HashMap<SpaceId, CGPoint>,
        command: LayoutCommand,
    ) -> EventResponse {
        self.debug_command_tree(space);
        let is_floating = self.focused_window.is_some_and(|focus| self.floating.is_floating(focus));
        debug!(?self.focused_window, last_floating_focus=?self.floating.last_focus(), ?is_floating);

        if let LayoutCommand::ToggleWindowFloating = &command {
            return self.toggle_window_floating(window_store, space, is_floating);
        }

        if let LayoutCommand::ToggleFullscreen | LayoutCommand::ToggleFullscreenWithinGaps = &command
            && is_floating
        {
            return self.toggle_floating_fullscreen(window_store, space, &command);
        }


        let Some(space) = space else {
            return EventResponse::default();
        };
        let workspace_id = match self.virtual_workspace_manager.active_workspace(space) {
            Some(id) => id,
            None => {
                warn!("No active virtual workspace for space {:?}", space);
                return EventResponse::default();
            }
        };
        let layout = match self.workspace_layouts.active(space, workspace_id) {
            Some(id) => id,
            None => {
                warn!(
                    "No active layout for workspace {:?} on space {:?}; command ignored",
                    workspace_id, space
                );
                return EventResponse::default();
            }
        };

        if let LayoutCommand::ToggleFocusFloating = &command {
            return self
                .toggle_focus_floating(window_store, space, workspace_id, layout, is_floating);
        }

        match command {
            LayoutCommand::ToggleWindowFloating => unreachable!(),
            LayoutCommand::ToggleFocusFloating => unreachable!(),

            LayoutCommand::SwapWindows(a, b) => {
                let a = rini_core::ids::WindowId::new(a.pid, a.idx);
                let b = rini_core::ids::WindowId::new(b.pid, b.idx);
                let _ = self.workspace_tree_mut(workspace_id).swap_windows(layout, a, b);

                EventResponse::default()
            }
            LayoutCommand::NextWindow | LayoutCommand::PrevWindow => {
                let forward = matches!(command, LayoutCommand::NextWindow);
                let windows = if is_floating {
                    self.active_floating_windows_in_workspace(window_store, space)
                } else {
                    self.filter_active_workspace_windows(
                        window_store,
                        space,
                        self.workspace_tree(workspace_id).visible_windows_in_layout(layout),
                    )
                };
                if let Some(idx) = windows.iter().position(|&w| Some(w) == self.focused_window) {
                    let next = if forward {
                        (idx + 1) % windows.len()
                    } else {
                        (idx + windows.len() - 1) % windows.len()
                    };
                    let response = EventResponse {
                        changed: true,
                        focus_window: Some(windows[next]),
                        raise_windows: vec![windows[next]],
                        boundary_hit: None,
                        edge_hit: None,
                    };
                    self.apply_focus_response(window_store, space, workspace_id, layout, &response);
                    return response;
                } else {
                    let focus_window = self
                        .workspace_tree(workspace_id)
                        .selected_window(layout)
                        .filter(|wid| windows.contains(wid))
                        .or_else(|| windows.first().copied());
                    let raise_windows = focus_window.into_iter().collect();
                    let response = EventResponse {
                        changed: true,
                        focus_window,
                        raise_windows,
                        boundary_hit: None,
                        edge_hit: None,
                    };
                    self.apply_focus_response(window_store, space, workspace_id, layout, &response);
                    return response;
                }
            }
            LayoutCommand::MoveFocus(direction) => {
                debug!(
                    "MoveFocus command received, direction: {:?}, is_floating: {}",
                    direction, is_floating
                );
                return self.move_focus_internal(
                    window_store,
                    space,
                    visible_spaces,
                    visible_space_centers,
                    direction,
                    is_floating,
                );
            }
            LayoutCommand::MoveNode(direction) => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                if !self.workspace_tree_mut(workspace_id).move_selection(layout, direction) {
                    if let Some(new_space) = self.next_space_for_direction(
                        space,
                        direction,
                        visible_spaces,
                        visible_space_centers,
                    ) {
                        let Some((new_ws_id, new_layout)) = self.workspace_and_layout(new_space)
                        else {
                            debug!(
                                "No active workspace/layout for adjacent space {:?}; skipping cross-space move",
                                new_space
                            );
                            return EventResponse::default();
                        };
                        let windows = self
                            .workspace_tree(workspace_id)
                            .visible_windows_under_selection(layout);
                        for wid in windows {
                            self.workspace_tree_mut(workspace_id).remove_window(wid);
                            self.workspace_tree_mut(new_ws_id)
                                .add_window_after_selection(new_layout, wid);
                            self.virtual_workspace_manager.assign_window_to_workspace(
                                window_store,
                                new_space,
                                wid,
                                new_ws_id,
                            );
                        }
                    }
                }
                EventResponse::default()
            }
            LayoutCommand::ToggleFullscreen => {
                let raise_windows =
                    self.workspace_tree_mut(workspace_id).toggle_fullscreen_of_selection(layout);
                if raise_windows.is_empty() {
                    EventResponse::default()
                } else {
                    EventResponse {
                        changed: true,
                        raise_windows,
                        focus_window: None,
                        boundary_hit: None,
                        edge_hit: None,
                    }
                }
            }
            LayoutCommand::ToggleFullscreenWithinGaps => {
                let raise_windows = self
                    .workspace_tree_mut(workspace_id)
                    .toggle_fullscreen_within_gaps_of_selection(layout);
                for window in &raise_windows {
                    self.remember_column_width(space, workspace_id, layout, *window);
                }
                if raise_windows.is_empty() {
                    EventResponse::default()
                } else {
                    EventResponse {
                        changed: true,
                        raise_windows,
                        focus_window: None,
                        boundary_hit: None,
                        edge_hit: None,
                    }
                }
            }
            // handled by upper reactor
            LayoutCommand::NextWorkspace(_)
            | LayoutCommand::PrevWorkspace(_)
            | LayoutCommand::SwitchToWorkspace(_)
            | LayoutCommand::MoveWindowToWorkspace { .. }
            | LayoutCommand::CreateWorkspace
            | LayoutCommand::SwitchToLastWorkspace => EventResponse::default(),
            LayoutCommand::JoinWindow(direction) => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                self.workspace_tree_mut(workspace_id)
                    .join_selection_with_direction(layout, direction);
                EventResponse::default()
            }
            LayoutCommand::ConsumeOrExpelWindow(direction) => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                self.workspace_tree_mut(workspace_id)
                    .consume_or_expel_selection(layout, direction);
                EventResponse::default()
            }
            LayoutCommand::ToggleStack => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                self.toggle_stack_for_workspace(workspace_id, layout)
            }
            LayoutCommand::UnjoinWindows => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                self.workspace_tree_mut(workspace_id).unjoin_selection(layout);
                EventResponse::default()
            }
            LayoutCommand::ResizeWindowGrow(orientation) => {
                if is_floating {
                    return EventResponse::default();
                }

                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                let resize_amount = 0.05;
                self.workspace_tree_mut(workspace_id).resize_selection_by(
                    layout,
                    resize_amount,
                    orientation,
                );
                self.remember_selected_column_width(space, workspace_id, layout);
                EventResponse::default()
            }
            LayoutCommand::ResizeWindowShrink(orientation) => {
                if is_floating {
                    return EventResponse::default();
                }

                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                let resize_amount = -0.05;
                self.workspace_tree_mut(workspace_id).resize_selection_by(
                    layout,
                    resize_amount,
                    orientation,
                );
                self.remember_selected_column_width(space, workspace_id, layout);
                EventResponse::default()
            }
            LayoutCommand::ResizeWindowBy { amount } => {
                if is_floating {
                    return EventResponse::default();
                }

                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                self.workspace_tree_mut(workspace_id).resize_selection_by(
                    layout,
                    amount,
                    ResizeOrientation::Horizontal,
                );
                self.remember_selected_column_width(space, workspace_id, layout);
                EventResponse::default()
            }
            LayoutCommand::ScrollStrip { delta } => {
                let mut resp = EventResponse::default();
                let LayoutSystemKind::Scrolling(system) = self.workspace_tree_mut(workspace_id);
                resp.boundary_hit = system.scroll_by_delta(layout, delta);
                resp
            }
            LayoutCommand::SnapStrip => {
                let LayoutSystemKind::Scrolling(system) = self.workspace_tree_mut(workspace_id);
                system.snap_to_nearest_column(layout);
                EventResponse::default()
            }
            LayoutCommand::CenterSelection => {
                let LayoutSystemKind::Scrolling(system) = self.workspace_tree_mut(workspace_id);
                system.center_selected_column(layout);
                EventResponse::default()
            }
            LayoutCommand::CyclePresetColumnWidth => {
                self.workspace_layouts.mark_last_saved(space, workspace_id, layout);
                let raised =
                    self.workspace_tree_mut(workspace_id).cycle_preset_column_width(layout);
                for window in &raised {
                    self.remember_column_width(space, workspace_id, layout, *window);
                }
                Self::response_for_raised_windows(raised)
            }
        }
    }

    pub fn calculate_layout(
        &mut self,
        space: SpaceId,
        screen: CGRect,
        gaps: &crate::layout::settings::GapSettings,
    ) -> Vec<(WindowId, CGRect)> {
        let Some((ws_id, layout)) = self.workspace_and_layout(space) else {
            return Vec::new();
        };
        self.workspace_tree(ws_id).calculate_layout(
            layout,
            screen,
            &self.window_layout_constraints,
            gaps,
        )
    }

    pub fn calculate_layout_with_virtual_workspaces<F>(
        &mut self,
        window_store: &WindowStore,
        space: SpaceId,
        screen: CGRect,
        gaps: &crate::layout::settings::GapSettings,
        get_window_frame: F,
        all_screens: &[CGRect],
    ) -> Vec<(WindowId, CGRect)>
    where
        F: Fn(WindowId) -> Option<CGRect>,
    {
        use crate::workspaces::HideCorner;

        let mut positions = HashMap::default();
        let window_size = |wid| {
            get_window_frame(wid)
                .map(|f| f.size)
                .unwrap_or_else(|| CGSize::new(500.0, 500.0))
        };
        let center_rect = |size: CGSize| {
            let center = screen.mid();
            let origin = CGPoint::new(center.x - size.width / 2.0, center.y - size.height / 2.0);
            CGRect::new(origin, size)
        };

        fn ensure_visible_floating(
            engine: &mut LayoutEngine,
            positions: &mut HashMap<WindowId, CGRect>,
            space: SpaceId,
            workspace_id: crate::workspaces::VirtualWorkspaceId,
            wid: WindowId,
            candidate: Option<CGRect>,
            store_if_absent: bool,
            screen: &CGRect,
            all_screens: &[CGRect],
            center_rect: &impl Fn(CGSize) -> CGRect,
            window_size: &impl Fn(WindowId) -> CGSize,
        ) {
            let existing = positions.get(&wid).copied();
            let bundle_id = engine.get_app_bundle_id_for_window(wid);
            let visible = candidate.or(existing).filter(|rect| {
                !engine.virtual_workspace_manager.is_hidden_position_multi(
                    screen,
                    rect,
                    bundle_id.as_deref(),
                    all_screens,
                )
            });
            let rect = visible.unwrap_or_else(|| center_rect(window_size(wid)));
            positions.insert(wid, rect);
            if store_if_absent {
                engine.floating_positions.store_if_absent(space, workspace_id, wid, rect);
            } else {
                engine.floating_positions.store(space, workspace_id, wid, rect);
            }
        }

        if let Some(active_workspace_id) = self.virtual_workspace_manager.active_workspace(space) {
            if let Some(layout) = self.workspace_layouts.active(space, active_workspace_id) {
                let tiled_positions = self.workspace_tree(active_workspace_id).calculate_layout(
                    layout,
                    screen,
                    &self.window_layout_constraints,
                    gaps,
                );

                for (wid, rect) in tiled_positions {
                    // A column scrolled right off the strip is parked in a corner rather than written at
                    // its strip coordinate. macOS refuses the coordinate and leaves 40pt of the window
                    // showing at the edge; it honours the corner at 1pt. The strip's own geometry is
                    // untouched — `calculate_layout_for_workspace` still answers with the real position,
                    // which is what the animation canvas is built from. See "macOS will not park a window
                    // further off the left edge than 40pt" in `docs/animation/capture-overlay-research.md`.
                    let placed = if crate::workspaces::HiddenWindowPlacement::is_off_screen(screen, rect) {
                        // The corner records which side of the strip the column was on, so an animation
                        // can bring it back in from that edge instead of up from the bottom.
                        let corner = if rect.max().x <= screen.origin.x {
                            HideCorner::BottomLeft
                        } else {
                            HideCorner::BottomRight
                        };
                        self.virtual_workspace_manager.calculate_hidden_position_multi(
                            screen,
                            rect,
                            corner,
                            None,
                            all_screens,
                        )
                    } else {
                        rect
                    };
                    positions.insert(wid, placed);
                }
            }

            let floating_positions =
                self.floating_positions.workspace_positions(space, active_workspace_id);
            for (window_id, stored_position) in floating_positions {
                if self.floating.is_floating(window_id)
                    && self.virtual_workspace_manager.workspace_for_window(
                        window_store,
                        space,
                        window_id,
                    ) == Some(active_workspace_id)
                {
                    ensure_visible_floating(
                        self,
                        &mut positions,
                        space,
                        active_workspace_id,
                        window_id,
                        Some(stored_position),
                        false,
                        &screen,
                        all_screens,
                        &center_rect,
                        &window_size,
                    );
                }
            }

            let floating_windows = self.active_floating_windows_in_workspace(window_store, space);
            for wid in floating_windows {
                ensure_visible_floating(
                    self,
                    &mut positions,
                    space,
                    active_workspace_id,
                    wid,
                    None,
                    false,
                    &screen,
                    all_screens,
                    &center_rect,
                    &window_size,
                );
            }

            let fullscreen: Vec<(WindowId, FloatingFullscreenKind)> = positions
                .keys()
                .copied()
                .filter_map(|w| self.floating.fullscreen_kind(w).map(|k| (w, k)))
                .collect();
            for (w, kind) in fullscreen {
                let rect = match kind {
                    FloatingFullscreenKind::Full => screen,
                    FloatingFullscreenKind::WithinGaps => {
                        let o = &gaps.outer;
                        CGRect::new(
                            CGPoint::new(screen.origin.x + o.left, screen.origin.y + o.top),
                            CGSize::new(
                                screen.size.width - o.left - o.right,
                                screen.size.height - o.top - o.bottom,
                            ),
                        )
                    }
                };
                positions.insert(w, rect);
            }
        }

        let hidden_windows = self
            .virtual_workspace_manager
            .windows_in_inactive_workspaces(window_store, space);
        for wid in hidden_windows {
            let original_frame = get_window_frame(wid);

            if self.floating.is_floating(wid) {
                if let Some(workspace_id) =
                    self.virtual_workspace_manager.workspace_for_window(window_store, space, wid)
                {
                    ensure_visible_floating(
                        self,
                        &mut positions,
                        space,
                        workspace_id,
                        wid,
                        original_frame,
                        true,
                        &screen,
                        all_screens,
                        &center_rect,
                        &window_size,
                    );
                }
            }

            let original_size =
                original_frame.map(|f| f.size).unwrap_or_else(|| CGSize::new(500.0, 500.0));
            let reference_frame = original_frame.unwrap_or_else(|| {
                CGRect::new(CGPoint::new(screen.origin.x, screen.origin.y), original_size)
            });
            let app_bundle_id = self.get_app_bundle_id_for_window(wid);
            let hidden_rect = self.virtual_workspace_manager.calculate_hidden_position_multi(
                screen,
                reference_frame,
                HideCorner::BottomRight,
                app_bundle_id.as_deref(),
                all_screens,
            );
            positions.insert(wid, hidden_rect);
        }

        positions.into_iter().collect()
    }

    pub fn calculate_layout_for_workspace(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_id: crate::workspaces::VirtualWorkspaceId,
        screen: CGRect,
        gaps: &crate::layout::settings::GapSettings,
    ) -> Vec<(WindowId, CGRect)> {
        let mut positions = HashMap::default();

        if let Some(layout) = self.workspace_layouts.active(space, workspace_id) {
            let tiled_positions = self.workspace_tree(workspace_id).calculate_layout(
                layout,
                screen,
                &self.window_layout_constraints,
                gaps,
            );
            for (wid, rect) in tiled_positions {
                positions.insert(wid, rect);
            }
        }

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

    fn get_app_bundle_id_for_window(&self, _window_id: WindowId) -> Option<String> {
        // The bundle ID is stored in the app info, which we can access via the PID
        // Note: This would need to be available from the reactor state, but since
        // we're in the layout engine, we don't have direct access to that.
        // For now, we'll return None, but this could be improved by passing
        // app information through the layout calculation or storing it separately.

        None
    }

    pub fn layout(&mut self, space: SpaceId) -> LayoutId {
        let workspace_id = self
            .virtual_workspace_manager
            .active_workspace(space)
            .expect("No active workspace for space");

        if let Some(layout) = self.workspace_layouts.active(space, workspace_id) {
            layout
        } else {
            let workspaces = self.virtual_workspace_manager_mut().list_workspaces(space).to_vec();
            let default_size = CGSize::new(1000.0, 1000.0);
            for (id, _) in workspaces {
                let tree = &mut self.virtual_workspace_manager.workspaces[id].layout_system;
                self.workspace_layouts
                    .ensure_active_for_workspace(space, default_size, id, tree);
            }

            self.workspace_layouts
                .active(space, workspace_id)
                .expect("Failed to create an active layout for the workspace")
        }
    }

    #[cfg(test)]
    pub fn selected_window(&mut self, space: SpaceId) -> Option<WindowId> {
        let (ws_id, layout) = self.workspace_and_layout(space)?;
        self.workspace_tree(ws_id).selected_window(layout)
    }

    /// Moves a window to another workspace on the same space, following it if asked.
    ///
    /// The window is whichever index was named, else the focused one. Following means the display
    /// switches to the destination, which is why this cannot simply reassign and return.
    fn move_window_to_workspace(
        &mut self,
        window_store: &mut WindowStore,
        space: SpaceId,
        workspace: &WorkspaceSelector,
        follow: bool,
        maybe_id: &Option<u32>,
    ) -> EventResponse {
                let focused_window = if let Some(spec_u32) = maybe_id {
                    match self.virtual_workspace_manager.find_window_by_idx(
                        window_store,
                        space,
                        *spec_u32,
                    ) {
                        Some(w) => w,
                        None => return EventResponse::default(),
                    }
                } else {
                    match self.focused_window {
                        Some(wid) => wid,
                        None => return EventResponse::default(),
                    }
                };

                let inferred_space = self.space_with_window(focused_window);
                let op_space = if inferred_space == Some(space) {
                    space
                } else {
                    inferred_space.unwrap_or(space)
                };

                let workspaces = self.virtual_workspace_manager_mut().list_workspaces(op_space);
                let Some(current_workspace_id) = self
                    .virtual_workspace_manager
                    .workspace_for_window(window_store, op_space, focused_window)
                else {
                    return EventResponse::default();
                };
                let target_workspace_id = match workspace {
                    WorkspaceSelector::Index(index) => workspaces.get(*index).map(|(id, _)| *id),
                    WorkspaceSelector::Name(name) if name == "next" => self
                        .virtual_workspace_manager
                        .next_workspace(window_store, op_space, current_workspace_id, None),
                    WorkspaceSelector::Name(name) if name == "prev" => self
                        .virtual_workspace_manager
                        .prev_workspace(window_store, op_space, current_workspace_id, None),
                    WorkspaceSelector::Name(name) => workspaces
                        .iter()
                        .find_map(|(id, workspace_name)| (workspace_name == name).then_some(*id)),
                };
                let Some(target_workspace_id) = target_workspace_id else {
                    return EventResponse::default();
                };

                if current_workspace_id == target_workspace_id {
                    return EventResponse::default();
                }

                let is_floating = self.floating.is_floating(focused_window);

                // Capture the width before the window leaves its tree, so a size the user set
                // and never re-applied elsewhere is not lost on the way out. The destination
                // reads it back from per-display affinity below, NOT from a value threaded
                // through this function: the width belongs to the display, and this move keeps
                // the window on the same display, so what it had here is what it gets there.
                if !is_floating
                    && let Some(layout) =
                        self.workspace_layouts.active(op_space, current_workspace_id)
                {
                    self.remember_column_width(
                        op_space,
                        current_workspace_id,
                        layout,
                        focused_window,
                    );
                }

                if is_floating {
                    self.floating.remove_active_for_window(focused_window);
                } else {
                    self.remove_window_from_all_tiling_trees(focused_window);
                }

                let assigned = self.virtual_workspace_manager.assign_window_to_workspace(
                    window_store,
                    op_space,
                    focused_window,
                    target_workspace_id,
                );
                if !assigned {
                    if is_floating {
                        self.floating.add_active(op_space, focused_window.pid, focused_window);
                    } else if let Some(prev_layout) =
                        self.workspace_layouts.active(op_space, current_workspace_id)
                    {
                        self.workspace_tree_mut(current_workspace_id)
                            .add_window_after_selection(prev_layout, focused_window);
                    }
                    return EventResponse::default();
                }

                if !is_floating
                    && let Some(target_layout) =
                        self.workspace_layouts.active(op_space, target_workspace_id)
                {
                    self.workspace_tree_mut(target_workspace_id)
                        .add_window_after_selection(target_layout, focused_window);
                    // A fresh column starts at the display's default ratio, so re-apply
                    // whatever this window last had on THIS display.
                    self.apply_remembered_column_width(
                        op_space,
                        target_workspace_id,
                        target_layout,
                        focused_window,
                    );
                }

                if follow {
                    return self.activate_workspace(
                        window_store,
                        op_space,
                        target_workspace_id,
                        Some(focused_window),
                    );
                }

                let active_workspace = self.virtual_workspace_manager.active_workspace(op_space);

                if Some(target_workspace_id) == active_workspace {
                    if is_floating {
                        self.floating.add_active(op_space, focused_window.pid, focused_window);
                    }
                    self.broadcast_windows_changed(window_store, op_space);
                    return EventResponse {
                        changed: true,
                        focus_window: Some(focused_window),
                        raise_windows: vec![],
                        boundary_hit: None,
                        edge_hit: None,
                    };
                } else if Some(current_workspace_id) == active_workspace {
                    self.focused_window = None;
                    self.virtual_workspace_manager.set_last_focused_window(
                        op_space,
                        current_workspace_id,
                        None,
                    );

                    let remaining_windows = self
                        .virtual_workspace_manager
                        .windows_in_active_workspace(window_store, op_space);
                    if let Some(&new_focus) = remaining_windows.first() {
                        self.broadcast_windows_changed(window_store, op_space);
                        return EventResponse {
                            changed: true,
                            focus_window: Some(new_focus),
                            raise_windows: vec![],
                            boundary_hit: None,
                            edge_hit: None,
                        };
                    }
                }

                self.virtual_workspace_manager.set_last_focused_window(
                    op_space,
                    target_workspace_id,
                    Some(focused_window),
                );

                self.broadcast_windows_changed(window_store, op_space);
                EventResponse {
                    changed: true,
                    ..EventResponse::default()
                }
    }

    pub fn handle_virtual_workspace_command(
        &mut self,
        window_store: &mut WindowStore,
        space: SpaceId,
        command: &LayoutCommand,
    ) -> EventResponse {
        match command {
            // Workspaces stack downward: the next one is below, the previous above. No step
            // (the end with `prevent_wrapping`, or nothing left in that direction to skip to)
            // reports the edge instead of silence.
            LayoutCommand::NextWorkspace(skip_empty) => {
                if let Some(current_workspace) =
                    self.virtual_workspace_manager.active_workspace(space)
                {
                    if let Some(next_workspace) = self.virtual_workspace_manager.next_workspace(
                        window_store,
                        space,
                        current_workspace,
                        *skip_empty,
                    ) {
                        return self.activate_workspace(window_store, space, next_workspace, None);
                    }
                }
                EventResponse { edge_hit: Some(Direction::Down), ..EventResponse::default() }
            }
            LayoutCommand::PrevWorkspace(skip_empty) => {
                if let Some(current_workspace) =
                    self.virtual_workspace_manager.active_workspace(space)
                {
                    if let Some(prev_workspace) = self.virtual_workspace_manager.prev_workspace(
                        window_store,
                        space,
                        current_workspace,
                        *skip_empty,
                    ) {
                        return self.activate_workspace(window_store, space, prev_workspace, None);
                    }
                }
                EventResponse { edge_hit: Some(Direction::Up), ..EventResponse::default() }
            }
            LayoutCommand::SwitchToWorkspace(workspace_index) => {
                self.switch_to_workspace(window_store, space, *workspace_index, None)
            }
            LayoutCommand::MoveWindowToWorkspace {
                workspace,
                follow,
                window_id: maybe_id,
            } => self.move_window_to_workspace(window_store, space, workspace, *follow, maybe_id),
            LayoutCommand::CreateWorkspace => {
                match self.virtual_workspace_manager.create_workspace(space, None) {
                    Ok(_workspace_id) => {
                        self.broadcast_workspace_changed(space);
                        EventResponse {
                            changed: true,
                            ..EventResponse::default()
                        }
                    }
                    Err(e) => {
                        warn!("Failed to create new workspace: {:?}", e);
                        EventResponse::default()
                    }
                }
            }
            LayoutCommand::SwitchToLastWorkspace => {
                if let Some(last_workspace) = self.virtual_workspace_manager.last_workspace(space) {
                    return self.activate_workspace(window_store, space, last_workspace, None);
                }
                EventResponse::default()
            }
            _ => EventResponse::default(),
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

    pub fn virtual_workspace_manager(&self) -> &WorkspaceStore {
        &self.virtual_workspace_manager
    }

    pub fn virtual_workspace_manager_mut(&mut self) -> &mut WorkspaceStore {
        &mut self.virtual_workspace_manager
    }

    pub fn active_workspace(&self, space: SpaceId) -> Option<crate::workspaces::VirtualWorkspaceId> {
        self.virtual_workspace_manager.active_workspace(space)
    }

    /// `assign_window_with_app_info` with the window's own title, role, subrole and modality read
    /// from the store. `Err(AssignmentFailed)` when the store no longer has the window.
    pub fn assign_window_by_rules(
        &mut self,
        window_store: &mut WindowStore,
        window_id: WindowId,
        space: SpaceId,
        app_info: Option<&AppInfo>,
    ) -> Result<AppRuleResult, crate::workspaces::domain::virtual_workspace::WorkspaceError> {
        let Some(window) = window_store.window(window_id) else {
            return Err(crate::workspaces::domain::virtual_workspace::WorkspaceError::AssignmentFailed);
        };
        let title = window.info.title.clone();
        let ax_role = window.info.ax_role.clone();
        let ax_subrole = window.info.ax_subrole.clone();
        let is_modal = window.info.is_modal;
        self.assign_window_with_app_info(
            window_store,
            window_id,
            space,
            app_info.and_then(|a| a.bundle_id.as_deref()),
            app_info.and_then(|a| a.localized_name.as_deref()),
            Some(title.as_str()),
            ax_role.as_deref(),
            ax_subrole.as_deref(),
            is_modal,
        )
    }

    /// What the layout holds for `window_id` on `space`, taken before its rules are re-evaluated.
    pub fn before_rules(&self, window_store: &WindowStore, space: SpaceId, window_id: WindowId) -> BeforeRules {
        BeforeRules {
            assigned: self
                .virtual_workspace_manager
                .workspace_for_window(window_store, space, window_id)
                .is_some(),
            floating: self.floating.is_floating(window_id),
            ignored: window_store.window(window_id).is_some_and(|w| w.ignore_app_rule),
        }
    }

    /// Records a rule verdict on the window (`ignore_app_rule`) and says what the layout must do
    /// next. A failed evaluation is logged and treated as "managed, rules unknown".
    pub fn settle_app_rule(
        &self,
        window_store: &mut WindowStore,
        window_id: WindowId,
        space: SpaceId,
        before: BeforeRules,
        result: &Result<AppRuleResult, crate::workspaces::domain::virtual_workspace::WorkspaceError>,
    ) -> AfterRules {
        if let Err(e) = result {
            warn!("Failed to assign window {:?} to workspace: {:?}", window_id, e);
        }
        if let Some(window) = window_store.window_mut(window_id) {
            window.ignore_app_rule = matches!(result, Ok(AppRuleResult::Unmanaged));
        }
        let in_layout_now = self
            .virtual_workspace_manager
            .workspace_for_window(window_store, space, window_id)
            .is_some()
            || self.floating.is_floating(window_id);
        AfterRules::for_result(before, result.as_ref().map_err(|_| ()), in_layout_now)
    }

    pub fn assign_window_with_app_info(
        &mut self,
        window_store: &mut WindowStore,
        window_id: WindowId,
        space: SpaceId,
        app_bundle_id: Option<&str>,
        app_name: Option<&str>,
        window_title: Option<&str>,
        ax_role: Option<&str>,
        ax_subrole: Option<&str>,
        is_modal: bool,
    ) -> Result<AppRuleResult, crate::workspaces::domain::virtual_workspace::WorkspaceError> {
        let decision = self.app_rules.evaluate(WindowRuleContext {
            app_bundle_id,
            app_name,
            window_title,
            ax_role,
            ax_subrole,
            is_modal,
        });

        // Where this application's windows were, but only when the config had nothing to say and the
        // window has no assignment already. This is the first sighting of a launching application's
        // window, well before `WindowAdded`, so it is the only place the answer can still be changed.
        if decision == AppRuleDecision::NoMatch
            && self
                .virtual_workspace_manager
                .workspace_for_window(window_store, space, window_id)
                .is_none()
            && let Some(app_id) = app_bundle_id
            && let Some(slot) =
                self.launch_slot_for_new_window(window_store, window_id, app_id, window_title)
            && let Some(workspace_id) = self
                .virtual_workspace_manager
                .list_workspaces(space)
                .get(slot.workspace_index)
                .map(|(id, _)| *id)
        {
            if self.virtual_workspace_manager.assign_window_to_workspace(
                window_store,
                space,
                window_id,
                workspace_id,
            ) {
                self.display_affinity.set_window_home_if_absent(window_id, &slot.display_uuid);
                if let Some(width) = slot.width {
                    self.display_affinity.set_window_width(&slot.display_uuid, window_id, width);
                }
                debug!(
                    idx = window_id.idx.get(),
                    workspace = slot.workspace_index,
                    width = ?slot.width,
                    "placing a relaunched window where it was"
                );
                return Ok(AppRuleResult::Managed(AppRuleEffects {
                    workspace_id,
                    floating: self.floating.is_floating(window_id),
                    position: None,
                    size: None,
                    focus: false,
                    prev_rule_decision: false,
                }));
            }
        }

        self.virtual_workspace_manager.apply_app_rule_decision(
            window_store,
            window_id,
            space,
            decision,
        )
    }

    pub fn ensure_active_workspace_info(
        &mut self,
        space: SpaceId,
    ) -> Option<(crate::workspaces::VirtualWorkspaceId, String)> {
        if let Some(workspace_id) = self.virtual_workspace_manager.active_workspace(space) {
            let workspace_name = self
                .workspace_name(space, workspace_id)
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
            .workspace_name(space, first_workspace)
            .unwrap_or_else(|| format!("Workspace {:?}", first_workspace));

        Some((first_workspace, workspace_name))
    }

    pub fn active_workspace_idx(&self, space: SpaceId) -> Option<u64> {
        self.virtual_workspace_manager.active_workspace_idx(space)
    }

    pub fn move_window_to_space(
        &mut self,
        window_store: &mut WindowStore,
        source_space: SpaceId,
        target_space: SpaceId,
        target_screen_size: CGSize,
        window_id: WindowId,
    ) -> EventResponse {
        if source_space == target_space {
            return EventResponse {
                changed: true,
                raise_windows: vec![window_id],
                focus_window: Some(window_id),
                boundary_hit: None,
                edge_hit: None,
            };
        }

        let _ = self.virtual_workspace_manager.list_workspaces(source_space);
        let _ = self.virtual_workspace_manager.list_workspaces(target_space);

        let source_workspace = self
            .virtual_workspace_manager
            .workspace_for_window(window_store, source_space, window_id)
            .or_else(|| self.virtual_workspace_manager.active_workspace(source_space));

        let Some(source_workspace_id) = source_workspace else {
            return EventResponse::default();
        };

        let mut target_workspace_id = self.virtual_workspace_manager.active_workspace(target_space);
        if target_workspace_id.is_none() {
            if let Some((id, _)) =
                self.virtual_workspace_manager.list_workspaces(target_space).first()
            {
                self.virtual_workspace_manager.set_active_workspace(target_space, *id);
                target_workspace_id = Some(*id);
            }
        }

        let Some(target_workspace_id) = target_workspace_id else {
            return EventResponse::default();
        };

        let was_floating = self.floating.is_floating(window_id);

        if was_floating {
            self.floating.remove_active_for_window(window_id);
        } else {
            self.remove_window_from_all_tiling_trees(window_id);
        }

        let assigned = self.virtual_workspace_manager.assign_window_to_workspace(
            window_store,
            target_space,
            window_id,
            target_workspace_id,
        );

        if !assigned {
            if was_floating {
                self.floating.add_active(source_space, window_id.pid, window_id);
            } else if let Some(src_layout) =
                self.workspace_layouts.active(source_space, source_workspace_id)
            {
                self.workspace_tree_mut(source_workspace_id)
                    .add_window_after_selection(src_layout, window_id);
            }
            return EventResponse::default();
        }

        if was_floating {
            self.floating_positions.remove_window(window_id);
        }

        {
            let workspace_ids = self.virtual_workspace_manager.list_workspaces(target_space);
            for (id, _) in workspace_ids {
                let tree = &mut self.virtual_workspace_manager.workspaces[id].layout_system;
                self.workspace_layouts.ensure_active_for_workspace(
                    target_space,
                    target_screen_size,
                    id,
                    tree,
                );
            }
        }

        if was_floating {
            self.floating.add_active(target_space, window_id.pid, window_id);
            self.floating.set_last_focus(Some(window_id));
        } else if let Some(target_layout) =
            self.workspace_layouts.active(target_space, target_workspace_id)
        {
            self.workspace_tree_mut(target_workspace_id)
                .add_window_after_selection(target_layout, window_id);
            // Adopt the size this window last had on the DESTINATION display, not the one it
            // had on the source. A half-width column is roomy on a 2338pt monitor and cramped
            // on a 1728pt laptop panel, so carrying the source width across would be wrong;
            // the whole point of keying widths by display is that each has its own answer.
            // A window that has never been here keeps the display's configured default.
            self.apply_remembered_column_width(
                target_space,
                target_workspace_id,
                target_layout,
                window_id,
            );
        }

        if self.focused_window == Some(window_id) {
            self.focused_window = None;
        }

        if let Some(active_ws) = self.virtual_workspace_manager.active_workspace(source_space) {
            if active_ws == source_workspace_id {
                self.virtual_workspace_manager.set_last_focused_window(
                    source_space,
                    source_workspace_id,
                    None,
                );
            }
        }

        self.virtual_workspace_manager.set_last_focused_window(
            target_space,
            target_workspace_id,
            Some(window_id),
        );
        self.focused_window = Some(window_id);

        if source_space != target_space {
            self.broadcast_windows_changed(window_store, source_space);
        }
        self.broadcast_windows_changed(window_store, target_space);

        EventResponse {
            changed: true,
            raise_windows: vec![window_id],
            focus_window: Some(window_id),
            boundary_hit: None,
            edge_hit: None,
        }
    }

    pub fn workspace_name(
        &self,
        space: SpaceId,
        workspace_id: crate::workspaces::VirtualWorkspaceId,
    ) -> Option<String> {
        self.virtual_workspace_manager
            .workspace_info(space, workspace_id)
            .map(|ws| ws.name.clone())
    }

    pub fn windows_in_active_workspace(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
    ) -> Vec<WindowId> {
        self.virtual_workspace_manager.windows_in_active_workspace(window_store, space)
    }

    pub fn get_workspace_stats(
        &self,
        window_store: &WindowStore,
    ) -> crate::workspaces::domain::virtual_workspace::WorkspaceStats {
        self.virtual_workspace_manager.get_stats(window_store)
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

    pub fn get_floating_position(
        &self,
        space: SpaceId,
        workspace: VirtualWorkspaceId,
        window: WindowId,
    ) -> Option<CGRect> {
        self.floating_positions.get(space, workspace, window)
    }

    pub fn workspace_floating_positions(
        &self,
        space: SpaceId,
        workspace: VirtualWorkspaceId,
    ) -> Vec<(WindowId, CGRect)> {
        self.floating_positions.workspace_positions(space, workspace)
    }

    pub fn remove_floating_position(&mut self, window: WindowId) {
        self.floating_positions.remove_window(window);
    }

    pub fn rekey_window_identity(
        &mut self,
        window_store: &mut WindowStore,
        from: WindowId,
        to: WindowId,
    ) {
        window_store.transfer_persistent_window_metadata(from, to);
        self.transfer_persistent_window_identity(from, to);
    }

    pub fn transfer_persistent_window_identity(&mut self, from: WindowId, to: WindowId) {
        if from == to {
            return;
        }

        // A live `to` identity can already be provisionally present when a saved `from` identity
        // is matched. Remove that projection before replacement; LayoutSystem::replace_window is
        // not required to deduplicate and otherwise the same window can survive in two workspaces.
        self.remove_window_from_all_tiling_trees(to);
        for (_, workspace) in self.virtual_workspace_manager.workspaces.iter_mut() {
            workspace.layout_system.replace_window(from, to);
        }
        self.virtual_workspace_manager.transfer_window_identity(from, to);
        self.floating_positions.transfer_window_identity(from, to);
        self.floating.transfer_window_identity(from, to);
        self.transfer_persisted_window_identity(from, to);
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

    fn broadcast_workspace_changed(&self, space_id: SpaceId) {
        if let Some(ref broadcast_tx) = self.broadcast_tx {
            if let Some((active_workspace_id, active_workspace_name)) =
                self.active_workspace_id_and_name(space_id)
            {
                let display_uuid = self.display_uuid_for_space(space_id);
                let _ = broadcast_tx.send(BroadcastEvent::WorkspaceChanged {
                    workspace_id: protocol_workspace_id(active_workspace_id),
                    workspace_name: active_workspace_name.clone(),
                    space_id: space_id.get(),
                    display_uuid,
                });
            }
        }
    }

    fn broadcast_windows_changed(&self, window_store: &WindowStore, space_id: SpaceId) {
        if let Some(ref broadcast_tx) = self.broadcast_tx {
            if let Some((workspace_id, workspace_name)) =
                self.active_workspace_id_and_name(space_id)
            {
                let windows = self
                    .virtual_workspace_manager
                    .windows_in_active_workspace(window_store, space_id)
                    .iter()
                    .map(|window_id| window_id.to_debug_string())
                    .collect();

                let display_uuid = self.display_uuid_for_space(space_id);
                let event = BroadcastEvent::WindowsChanged {
                    workspace_id: protocol_workspace_id(workspace_id),
                    workspace_name,
                    windows,
                    space_id: space_id.get(),
                    display_uuid,
                };

                let _ = broadcast_tx.send(event);
            }
        }
    }

    pub fn debug_log_workspace_stats(&self, window_store: &WindowStore) {
        let stats = self.virtual_workspace_manager.get_stats(window_store);
        info!(
            "Workspace Stats: {} workspaces, {} windows, {} active spaces",
            stats.total_workspaces, stats.total_windows, stats.active_spaces
        );

        for (workspace_id, window_count) in &stats.workspace_window_counts {
            info!("  - '{:?}': {} windows", workspace_id, window_count);
        }
    }

    pub fn debug_log_workspace_state(&self, window_store: &WindowStore, space: SpaceId) {
        if let Some(active_workspace) = self.virtual_workspace_manager.active_workspace(space) {
            if let Some(workspace) =
                self.virtual_workspace_manager.workspace_info(space, active_workspace)
            {
                let active_windows =
                    self.virtual_workspace_manager.windows_in_active_workspace(window_store, space);
                let inactive_windows = self
                    .virtual_workspace_manager
                    .windows_in_inactive_workspaces(window_store, space);

                info!(
                    "Space {:?}: Active workspace '{}' with {} windows",
                    space,
                    workspace.name,
                    active_windows.len()
                );
                info!("  Active windows: {:?}", active_windows);
                info!("  Inactive windows: {} total", inactive_windows.len());
                if !inactive_windows.is_empty() {
                    info!("  Inactive window IDs: {:?}", inactive_windows);
                }
            }
        } else {
            warn!("Space {:?}: No active workspace set", space);
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

    use super::*;
    use rustc_hash::FxHashMap as HashMap;
    use crate::windows::domain::rules::{AppRulePosition, AppRuleSize, AppWorkspaceRule};
    use crate::layout::settings::LayoutSettings;
    use crate::workspaces::settings::VirtualWorkspaceSettings;
    use rini_ipc::protocol::WorkspaceSelector;

    fn test_engine() -> LayoutEngine {
        LayoutEngine::new(
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
            None,
        )
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

    #[test]
    fn next_space_for_direction_respects_physical_layout() {
        let engine = test_engine();
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
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space = SpaceId::new(42);
        let visible_spaces = vec![space];
        let visible_space_centers = HashMap::default();

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            engine.handle_command(
                &mut window_store,
                Some(space),
                &visible_spaces,
                &visible_space_centers,
                LayoutCommand::NextWindow,
            )
        }));

        assert!(
            result.is_ok(),
            "handle_command should not panic before SpaceExposed"
        );
    }

    /// `float_modal_windows` (default on): a modal window reported alongside a plain one floats
    /// with no app rule configured, and the plain one takes a column.
    #[test]
    fn a_modal_window_floats_by_default_and_a_plain_one_is_tiled() {
        let settings = VirtualWorkspaceSettings::default();
        assert!(settings.float_modal_windows);
        let mut engine = LayoutEngine::new(&settings, &LayoutSettings::default(), None);
        let mut window_store = WindowStore::default();
        let space = SpaceId::new(91);
        let (plain, modal) = (WindowId::new(7, 1), WindowId::new(7, 2));
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
        );
        let info = |wid, is_modal| (wid, None, None, None, is_modal, true, CGSize::new(300.0, 200.0), None, None);
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(
                space,
                7,
                vec![info(plain, false), info(modal, true)],
                Some(AppInfo { bundle_id: Some("com.example.Tool".into()), localized_name: None }),
            ),
        );
        assert!(engine.is_window_floating(modal), "the modal floats");
        assert!(!engine.is_window_floating(plain), "the app window is tiled");

        let mut off = VirtualWorkspaceSettings::default();
        off.float_modal_windows = false;
        let mut engine = LayoutEngine::new(&off, &LayoutSettings::default(), None);
        let mut window_store = WindowStore::default();
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
        );
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(space, 7, vec![info(modal, true)], None),
        );
        assert!(!engine.is_window_floating(modal), "with the setting off a modal is tiled");
    }

    #[test]
    fn floating_app_rule_emits_one_shot_placement_and_switches_focus_workspace() {
        let mut settings = VirtualWorkspaceSettings::default();
        settings.app_rules = vec![AppWorkspaceRule {
            app_id: Some("com.example.Tool".into()),
            workspace: Some(WorkspaceSelector::Index(1)),
            floating: true,
            position: Some(AppRulePosition { x: 0.4, y: 0.7 }),
            size: Some(AppRuleSize { w: Some(640.0), h: Some(480.0) }),
            focus: true,
            manage: true,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
            modal: None,
        }];
        let mut engine = LayoutEngine::new(&settings, &LayoutSettings::default(), None);
        let mut window_store = WindowStore::default();
        let space = SpaceId::new(90);
        let window = WindowId::new(7, 1);
        let screen = CGSize::new(1200.0, 800.0);
        let _ = engine.handle_event(&mut window_store, LayoutEvent::SpaceExposed(space, screen));

        let layout_outcome = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(
                space,
                window.pid,
                vec![(
                    window,
                    None,
                    None,
                    None,
                    false,
                    true,
                    CGSize::new(300.0, 200.0),
                    None,
                    None,
                )],
                Some(AppInfo {
                    bundle_id: Some("com.example.Tool".into()),
                    localized_name: None,
                }),
            ),
        );

        let (placement, resizes, focus) = layout_outcome.app_rules.into_parts();
        assert!(resizes.is_empty());
        assert_eq!(placement.len(), 1);
        assert_eq!(placement[0].window, window);
        assert_eq!(placement[0].position, Some(AppRulePosition { x: 0.4, y: 0.7 }));
        assert_eq!(
            placement[0].size,
            Some(AppRuleSize { w: Some(640.0), h: Some(480.0) })
        );
        assert_eq!(
            placement[0].resolve_frame(
                CGRect::new(CGPoint::new(10.0, 20.0), CGSize::new(300.0, 200.0)),
                CGRect::new(CGPoint::new(1000.0, 50.0), screen),
            ),
            CGRect::new(CGPoint::new(1224.0, 274.0), CGSize::new(640.0, 480.0))
        );
        assert_eq!(layout_outcome.response, EventResponse::default());
        let focus = focus.expect("focus rule should request a workspace switch");
        assert_eq!(focus.window, window);
        assert_eq!(focus.space, space);
        assert_eq!(focus.workspace_index, 1);
        let response = engine.switch_to_workspace_with_focus(
            &window_store,
            focus.space,
            focus.workspace_index,
            focus.window,
        );
        assert_eq!(response.focus_window, Some(window));
        assert!(response.changed);
        let target = engine.virtual_workspace_manager_mut().list_workspaces(space)[1].0;
        assert_eq!(engine.active_workspace(space), Some(target));
        assert!(engine.is_window_floating(window));
    }

    #[test]
    fn tiled_app_rule_size_sets_scrolling_column_width() {
        let mut settings = VirtualWorkspaceSettings::default();
        settings.app_rules = vec![AppWorkspaceRule {
            app_id: Some("com.example.Editor".into()),
            workspace: None,
            floating: false,
            position: None,
            size: Some(AppRuleSize { w: Some(234.0), h: None }),
            focus: false,
            manage: true,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
            modal: None,
        }];
        let mut layout_settings = LayoutSettings::default();
        layout_settings.scrolling.min_column_width_ratio = 0.1;
        let mut engine = LayoutEngine::new(&settings, &layout_settings, None);
        let mut window_store = WindowStore::default();
        let space = SpaceId::new(91);
        let window = WindowId::new(8, 1);
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1200.0, 800.0));
        let _ =
            engine.handle_event(&mut window_store, LayoutEvent::SpaceExposed(space, screen.size));
        let layout_outcome = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(
                space,
                window.pid,
                vec![(
                    window,
                    None,
                    None,
                    None,
                    false,
                    true,
                    CGSize::new(500.0, 500.0),
                    None,
                    None,
                )],
                Some(AppInfo {
                    bundle_id: Some("com.example.Editor".into()),
                    localized_name: None,
                }),
            ),
        );

        let (placements, resizes, workspace_focus) = layout_outcome.app_rules.into_parts();
        assert_eq!(placements, Vec::new());
        assert_eq!(workspace_focus, None);
        assert_eq!(resizes.len(), 1);
        assert_eq!(resizes[0].size.w, Some(234.0));
        assert_eq!(resizes[0].size.h, None);
        let old_frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(500.0, 500.0));
        let new_frame = CGRect::new(old_frame.origin, CGSize::new(234.0, 500.0));
        engine.apply_app_rule_resize(resizes[0], old_frame, new_frame, screen, None);

        let frames = engine.calculate_layout(
            space,
            screen,
            &layout_settings.gaps,
        );
        let frame = frames.iter().find(|(wid, _)| *wid == window).unwrap().1;
        assert_eq!(frame.size.width, 234.0);

        let user_frame = CGRect::new(new_frame.origin, CGSize::new(400.0, new_frame.size.height));
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowResized {
                wid: window,
                old_frame: new_frame,
                new_frame: user_frame,
                screens: vec![(space, screen, None)],
            },
        );
        let frames = engine.calculate_layout(
            space,
            screen,
            &layout_settings.gaps,
        );
        let frame = frames.iter().find(|(wid, _)| *wid == window).unwrap().1;
        assert_eq!(frame.size.width, 400.0);
    }

    #[test]
    fn tiled_membership_sync_does_not_rebalance_other_spaces() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space_a = SpaceId::new(101);
        let space_b = SpaceId::new(202);
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 800.0));
        let visible_spaces = vec![space_a, space_b];
        let visible_space_centers = HashMap::default();
        let window_a = WindowId::new(1, 1);
        let window_b = WindowId::new(1, 2);
        let window_c = WindowId::new(2, 1);
        let window_info = |wid| (wid, None, None, None, false, true, CGSize::new(0.0, 0.0), None, None);

        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::SpaceExposed(space_a, screen.size),
        );
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(
                space_a,
                1,
                vec![window_info(window_a), window_info(window_b)],
                None,
            ),
        );
        let _ = engine.handle_command(
            &mut window_store,
            Some(space_a),
            &visible_spaces,
            &visible_space_centers,
            LayoutCommand::ResizeWindowBy { amount: 0.2 },
        );

        let resized_layout = engine.calculate_layout(
            space_a,
            screen,
            &LayoutSettings::default().gaps,
        );

        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::SpaceExposed(space_b, screen.size),
        );
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(space_b, 2, vec![window_info(window_c)], None),
        );

        let after_other_space_sync = engine.calculate_layout(
            space_a,
            screen,
            &LayoutSettings::default().gaps,
        );
        assert_eq!(
            resized_layout, after_other_space_sync,
            "membership sync on one space must not rebalance saved layouts on another space"
        );
    }

    #[test]
    fn window_removed_preserve_floating_keeps_workspace_assignment() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space = SpaceId::new(303);
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 800.0));
        let pid: pid_t = 42;
        let wid = WindowId::new(pid, 1);
        let window_info = |wid| (wid, None, None, None, false, true, CGSize::new(0.0, 0.0), None, None);

        let _ =
            engine.handle_event(&mut window_store, LayoutEvent::SpaceExposed(space, screen.size));
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(space, pid, vec![window_info(wid)], None),
        );

        let assigned_workspace = engine
            .virtual_workspace_manager()
            .workspace_for_window(&window_store, space, wid)
            .expect("window should have a workspace assignment");

        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowRemovedPreserveFloating(wid),
        );

        assert_eq!(
            engine
                .virtual_workspace_manager()
                .workspace_for_window(&window_store, space, wid),
            Some(assigned_workspace),
            "temporary layout removal must not clear workspace ownership"
        );

        let _ = engine.handle_event(&mut window_store, LayoutEvent::WindowAdded(space, wid));

        assert_eq!(
            engine
                .virtual_workspace_manager()
                .workspace_for_window(&window_store, space, wid),
            Some(assigned_workspace),
            "window should reappear in the same workspace after a temporary hide"
        );
    }

    #[test]
    fn moving_floating_window_to_space_clears_source_floating_state() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let source_space = SpaceId::new(304);
        let target_space = SpaceId::new(305);
        let source_screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 800.0));
        let target_screen = CGRect::new(CGPoint::new(1000.0, 0.0), CGSize::new(1000.0, 800.0));
        let pid: pid_t = 43;
        let wid = WindowId::new(pid, 1);
        let source_position = CGRect::new(CGPoint::new(120.0, 140.0), CGSize::new(260.0, 220.0));
        let window_info = |wid| (wid, None, None, None, false, true, CGSize::new(0.0, 0.0), None, None);

        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::SpaceExposed(source_space, source_screen.size),
        );
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::SpaceExposed(target_space, target_screen.size),
        );
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(source_space, pid, vec![window_info(wid)], None),
        );

        let source_workspace = engine
            .virtual_workspace_manager()
            .active_workspace(source_space)
            .expect("source workspace");
        let target_workspace = engine
            .virtual_workspace_manager()
            .active_workspace(target_space)
            .expect("target workspace");

        engine.remove_window_from_all_tiling_trees(wid);
        engine.floating.add_floating(wid);
        engine.floating.add_active(source_space, pid, wid);
        engine.store_floating_position(source_space, source_workspace, wid, source_position);

        let response = engine.move_window_to_space(
            &mut window_store,
            source_space,
            target_space,
            target_screen.size,
            wid,
        );

        assert_eq!(response.focus_window, Some(wid));
        assert_eq!(
            engine.virtual_workspace_manager().workspace_for_window(
                &window_store,
                target_space,
                wid
            ),
            Some(target_workspace)
        );
        assert_eq!(
            engine.get_floating_position(source_space, source_workspace, wid),
            None,
            "cross-space moves must clear the source workspace's saved floating frame"
        );
        assert!(
            !engine
                .calculate_layout_for_workspace(
                    &window_store,
                    source_space,
                    source_workspace,
                    source_screen,
                    &LayoutSettings::default().gaps,
                )
                .into_iter()
                .any(|(window_id, _)| window_id == wid),
            "source workspace layout must not keep emitting the moved floating window"
        );
    }

    /// Horizontal focus must be able to leave the floating layer; a modulo cycle over the floating
    /// set never reached the strip.
    #[test]
    fn horizontal_focus_escapes_the_floating_layer_into_the_strip() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space = SpaceId::new(410);
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1200.0, 800.0));
        let pid: pid_t = 71;
        let tiled = WindowId::new(pid, 1);
        let float_a = WindowId::new(pid, 2);
        let float_b = WindowId::new(pid, 3);
        let info = |wid| (wid, None, None, None, false, true, CGSize::new(0.0, 0.0), None, None);

        let _ =
            engine.handle_event(&mut window_store, LayoutEvent::SpaceExposed(space, screen.size));
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(
                space,
                pid,
                vec![info(tiled), info(float_a), info(float_b)],
                None,
            ),
        );

        // Float two of the three windows, leaving one in the strip.
        for wid in [float_a, float_b] {
            engine.remove_window_from_all_tiling_trees(wid);
            engine.floating.add_floating(wid);
            engine.floating.add_active(space, pid, wid);
        }

        let visible_spaces = vec![space];
        let mut centers = HashMap::default();
        centers.insert(space, CGPoint::new(0.0, 0.0));

        // Walk right repeatedly from a floating window. Without the fix this only
        // ever alternates between float_a and float_b.
        engine.focused_window = Some(float_a);
        let mut reached_tiled = false;
        for _ in 0..6 {
            let response = engine.handle_command(
                &mut window_store,
                Some(space),
                &visible_spaces,
                &centers,
                LayoutCommand::MoveFocus(Direction::Right),
            );
            if let Some(next) = response.focus_window {
                engine.focused_window = Some(next);
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

    /// With isolate_displays set, horizontal focus must stop at the end of the
    /// current display's strip instead of continuing onto the adjacent display.
    #[test]
    fn isolate_displays_stops_horizontal_focus_at_the_strip_end() {
        for isolate in [false, true] {
            let mut window_store = WindowStore::default();
            let mut engine = test_engine();
            let mut settings = LayoutSettings::default();
            settings.scrolling.isolate_displays = isolate;
            engine.set_layout_settings(&settings);

            let left = SpaceId::new(520);
            let right = SpaceId::new(521);
            let size = CGSize::new(1000.0, 800.0);
            let pid: pid_t = 73;
            let on_left = WindowId::new(pid, 1);
            let on_right = WindowId::new(pid, 2);
            let info = |wid| (wid, None, None, None, false, true, CGSize::new(0.0, 0.0), None, None);

            for (space, wid) in [(left, on_left), (right, on_right)] {
                let _ =
                    engine.handle_event(&mut window_store, LayoutEvent::SpaceExposed(space, size));
                let _ = engine.handle_event(
                    &mut window_store,
                    LayoutEvent::WindowsOnScreenUpdated(space, pid, vec![info(wid)], None),
                );
            }

            let visible_spaces = vec![left, right];
            let mut centers = HashMap::default();
            centers.insert(left, CGPoint::new(0.0, 0.0));
            centers.insert(right, CGPoint::new(1000.0, 0.0));

            // Sitting on the only window of the LEFT display, walk right: there is
            // no further column on this display, so the adjacent display is the
            // only place focus could go.
            engine.focused_window = Some(on_left);
            let response = engine.handle_command(
                &mut window_store,
                Some(left),
                &visible_spaces,
                &centers,
                LayoutCommand::MoveFocus(Direction::Right),
            );

            if isolate {
                assert_ne!(
                    response.focus_window,
                    Some(on_right),
                    "isolate_displays = true must not move focus to the adjacent display"
                );
                assert_eq!(
                    response.edge_hit,
                    Some(Direction::Right),
                    "the strip stopped at its end: the reactor bounces it"
                );
            } else {
                assert_eq!(
                    response.focus_window,
                    Some(on_right),
                    "isolate_displays = false should still cross to the adjacent display"
                );
                assert_eq!(response.edge_hit, None, "focus moved on: no edge");
            }
            // Up at the top of a one-window column is not a strip edge.
            engine.focused_window = Some(on_left);
            let up = engine.handle_command(
                &mut window_store,
                Some(left),
                &visible_spaces,
                &centers,
                LayoutCommand::MoveFocus(Direction::Up),
            );
            assert_eq!(up.edge_hit, None);
        }
    }

    #[test]
    fn move_focus_to_uninitialized_adjacent_space_does_not_panic() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let current_space = SpaceId::new(50);
        let adjacent_space = SpaceId::new(51);
        let screen_size = CGSize::new(1920.0, 1080.0);
        let visible_spaces = vec![current_space, adjacent_space];
        let mut visible_space_centers = HashMap::default();
        visible_space_centers.insert(current_space, CGPoint::new(0.0, 0.0));
        visible_space_centers.insert(adjacent_space, CGPoint::new(1920.0, 0.0));

        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::SpaceExposed(current_space, screen_size),
        );

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            engine.handle_command(
                &mut window_store,
                Some(current_space),
                &visible_spaces,
                &visible_space_centers,
                LayoutCommand::MoveFocus(Direction::Right),
            )
        }));

        assert!(
            result.is_ok(),
            "cross-space move focus should not panic when adjacent space is not initialized"
        );
    }

    #[test]
    fn workspace_switch_response_reports_whether_workspace_changed() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space = SpaceId::new(81);

        let workspaces = engine.virtual_workspace_manager_mut().list_workspaces(space).to_vec();
        assert!(
            engine
                .virtual_workspace_manager_mut()
                .set_active_workspace(space, workspaces[0].0)
        );

        let already_active = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::SwitchToWorkspace(0),
        );
        assert!(!already_active.changed);

        let missing = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::SwitchToWorkspace(usize::MAX),
        );
        assert!(!missing.changed);

        let switched = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::SwitchToWorkspace(1),
        );
        assert!(switched.changed);
    }

    #[test]
    fn workspace_navigation_no_ops_report_unchanged() {
        let mut window_store = WindowStore::default();
        let space = SpaceId::new(82);
        let mut settings = VirtualWorkspaceSettings::default();
        settings.prevent_wrapping = true;
        let mut engine = LayoutEngine::new(&settings, &LayoutSettings::default(), None);

        let workspaces = engine.virtual_workspace_manager_mut().list_workspaces(space).to_vec();
        assert!(
            engine
                .virtual_workspace_manager_mut()
                .set_active_workspace(space, workspaces.last().unwrap().0)
        );
        let prevented_wrap = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::NextWorkspace(None),
        );
        assert!(!prevented_wrap.changed);
        assert_eq!(prevented_wrap.edge_hit, Some(Direction::Down), "the bottom of the stack");

        assert!(
            engine
                .virtual_workspace_manager_mut()
                .set_active_workspace(space, workspaces[0].0)
        );
        let no_eligible_workspace = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::NextWorkspace(Some(true)),
        );
        assert!(!no_eligible_workspace.changed);
        assert_eq!(no_eligible_workspace.edge_hit, Some(Direction::Down), "nothing further to skip to");
        let top = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::PrevWorkspace(None),
        );
        assert!(!top.changed);
        assert_eq!(top.edge_hit, Some(Direction::Up), "the top of the stack");

        // With wrapping the step always lands somewhere: no edge.
        let mut engine = LayoutEngine::new(
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
            None,
        );
        let workspaces = engine.virtual_workspace_manager_mut().list_workspaces(space).to_vec();
        assert!(
            engine
                .virtual_workspace_manager_mut()
                .set_active_workspace(space, workspaces.last().unwrap().0)
        );
        let wrapped = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::NextWorkspace(None),
        );
        assert!(wrapped.changed);
        assert_eq!(wrapped.edge_hit, None);
    }

    #[test]
    fn partial_windows_on_screen_update_preserves_assigned_tiled_windows() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space = SpaceId::new(94);
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 1000.0));
        let pid: pid_t = 5153;
        let w1 = WindowId::new(pid, 1);
        let w2 = WindowId::new(pid, 2);
        let info = |wid| {
            (
                wid,
                None,
                None,
                None,
                false,
                true,
                CGSize::new(500.0, 500.0),
                None,
                None,
            )
        };

        let _ =
            engine.handle_event(&mut window_store, LayoutEvent::SpaceExposed(space, screen.size));
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(space, pid, vec![info(w1), info(w2)], None),
        );
        let _ = engine.handle_event(&mut window_store, LayoutEvent::WindowFocused(space, w1));
        let _ = engine.handle_command(
            &mut window_store,
            Some(space),
            &[space],
            &HashMap::default(),
            LayoutCommand::ResizeWindowBy { amount: 0.2 },
        );

        let gaps = engine.layout_settings.gaps.clone();
        let before = engine.calculate_layout(
            space,
            screen,
            &gaps,
        );

        // Simulate a discovery snapshot that temporarily omitted w2.
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(space, pid, vec![info(w1)], None),
        );

        assert_eq!(
            engine.calculate_layout(
                space,
                screen,
                &gaps,
            ),
            before,
            "partial discovery must not remove an assigned window or reset its split"
        );
    }

    #[test]
    fn removing_a_window_does_not_rebalance_other_workspaces() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space_a = SpaceId::new(95);
        let space_b = SpaceId::new(96);
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 1000.0));
        let info = |wid| {
            (
                wid,
                None,
                None,
                None,
                false,
                true,
                CGSize::new(500.0, 500.0),
                None,
                None,
            )
        };
        let a1 = WindowId::new(5154, 1);
        let a2 = WindowId::new(5154, 2);
        let b1 = WindowId::new(5155, 1);

        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::SpaceExposed(space_a, screen.size),
        );
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::SpaceExposed(space_b, screen.size),
        );
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(space_a, a1.pid, vec![info(a1), info(a2)], None),
        );
        let _ = engine.handle_event(&mut window_store, LayoutEvent::WindowFocused(space_a, a1));
        let _ = engine.handle_command(
            &mut window_store,
            Some(space_a),
            &[space_a, space_b],
            &HashMap::default(),
            LayoutCommand::ResizeWindowBy { amount: 0.2 },
        );

        let gaps = engine.layout_settings.gaps.clone();
        let before = engine.calculate_layout(
            space_a,
            screen,
            &gaps,
        );

        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(space_b, b1.pid, vec![info(b1)], None),
        );
        let _ = window_store.remove_window_assignment(b1);
        let _ = engine.handle_event(&mut window_store, LayoutEvent::WindowRemoved(b1));

        assert_eq!(
            engine.calculate_layout(
                space_a,
                screen,
                &gaps,
            ),
            before,
            "removing a window must not rebalance layouts in other workspaces"
        );
    }

    #[test]
    fn removing_unknown_window_does_not_rebalance_layout() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space = SpaceId::new(92);
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 1000.0));
        let pid: pid_t = 5151;

        let windows = vec![
            (
                WindowId::new(pid, 1),
                None,
                None,
                None,
                false,
                true,
                CGSize::new(500.0, 500.0),
                None,
                None,
            ),
            (
                WindowId::new(pid, 2),
                None,
                None,
                None,
                false,
                true,
                CGSize::new(500.0, 500.0),
                None,
                None,
            ),
            (
                WindowId::new(pid, 3),
                None,
                None,
                None,
                false,
                true,
                CGSize::new(500.0, 500.0),
                None,
                None,
            ),
        ];

        let _ =
            engine.handle_event(&mut window_store, LayoutEvent::SpaceExposed(space, screen.size));
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(space, pid, windows, None),
        );
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowFocused(space, WindowId::new(pid, 1)),
        );
        let gaps = engine.layout_settings.gaps.clone();

        let _ = engine.handle_command(
            &mut window_store,
            Some(space),
            &[space],
            &HashMap::default(),
            LayoutCommand::MoveNode(Direction::Up),
        );

        let modified = engine.calculate_layout(
            space,
            screen,
            &gaps,
        );

        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowRemoved(WindowId::new(9999, 1)),
        );

        assert_eq!(
            engine.calculate_layout(
                space,
                screen,
                &gaps,
            ),
            modified
        );
    }

    #[test]
    fn duplicate_window_added_is_treated_as_noop_for_active_layout() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space = SpaceId::new(93);
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 1000.0));
        let pid: pid_t = 5152;
        let wid = WindowId::new(pid, 1);

        let _ =
            engine.handle_event(&mut window_store, LayoutEvent::SpaceExposed(space, screen.size));
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(
                space,
                pid,
                vec![(
                    wid,
                    None,
                    None,
                    None,
                    false,
                    true,
                    CGSize::new(500.0, 500.0),
                    None,
                    None,
                )],
                None,
            ),
        );
        let gaps = engine.layout_settings.gaps.clone();
        let before = engine.calculate_layout(
            space,
            screen,
            &gaps,
        );

        assert!(!engine.add_window_to_layout(&mut window_store, space, wid));
        assert_eq!(
            engine.calculate_layout(
                space,
                screen,
                &gaps,
            ),
            before
        );
    }

    #[test]
    fn workspace_switch_only_commits_focus_after_authoritative_commit() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space = SpaceId::new(94);
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 1000.0));
        let pid: pid_t = 5153;
        let wid1 = WindowId::new(pid, 1);
        let wid2 = WindowId::new(pid, 2);

        let _ =
            engine.handle_event(&mut window_store, LayoutEvent::SpaceExposed(space, screen.size));
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(
                space,
                pid,
                vec![
                    (
                        wid1,
                        None,
                        None,
                        None,
                        false,
                        true,
                        CGSize::new(500.0, 500.0),
                        None,
                        None,
                    ),
                    (
                        wid2,
                        None,
                        None,
                        None,
                        false,
                        true,
                        CGSize::new(500.0, 500.0),
                        None,
                        None,
                    ),
                ],
                None,
            ),
        );
        let _ = engine.handle_event(&mut window_store, LayoutEvent::WindowFocused(space, wid1));

        let _ = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::CreateWorkspace,
        );
        let workspaces = engine.virtual_workspace_manager_mut().list_workspaces(space).to_vec();
        let workspace_two = workspaces[1].0;

        let _ = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::MoveWindowToWorkspace {
                workspace: WorkspaceSelector::Index(1),
                follow: false,
                window_id: Some(wid2.idx.get()),
            },
        );

        let response = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::SwitchToWorkspace(1),
        );

        assert_eq!(engine.active_workspace(space), Some(workspace_two));
        assert_eq!(response.focus_window, Some(wid2));
        assert_ne!(engine.focused_window, Some(wid2));

        engine.commit_workspace_focus(&mut window_store, space, response.focus_window);

        assert_eq!(engine.focused_window, Some(wid2));
        assert_eq!(
            engine.virtual_workspace_manager().last_focused_window(space, workspace_two),
            Some(wid2)
        );
    }

    #[test]
    fn move_window_to_workspace_updates_authoritative_workspace_membership() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space = SpaceId::new(95);
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 1000.0));
        let pid: pid_t = 6001;
        let wid = WindowId::new(pid, 1);

        let _ =
            engine.handle_event(&mut window_store, LayoutEvent::SpaceExposed(space, screen.size));
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(
                space,
                pid,
                vec![(
                    wid,
                    None,
                    None,
                    None,
                    false,
                    true,
                    CGSize::new(500.0, 500.0),
                    None,
                    None,
                )],
                None,
            ),
        );

        let _ = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::CreateWorkspace,
        );
        let workspaces = engine.virtual_workspace_manager_mut().list_workspaces(space).to_vec();
        let ws1 = workspaces[0].0;
        let ws2 = workspaces[1].0;

        let _ = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::MoveWindowToWorkspace {
                workspace: WorkspaceSelector::Index(1),
                follow: false,
                window_id: Some(wid.idx.get()),
            },
        );

        assert!(
            engine
                .virtual_workspace_manager
                .workspace_windows(&window_store, space, ws1)
                .is_empty(),
            "source workspace must be empty after a same-space workspace move"
        );
        assert_eq!(
            engine.virtual_workspace_manager.workspace_for_window(&window_store, space, wid),
            Some(ws2)
        );
        assert_eq!(
            engine.virtual_workspace_manager.workspace_windows(&window_store, space, ws2),
            vec![wid]
        );

        for (target, expected) in [("next", workspaces[2].0), ("prev", ws2)] {
            let _ = engine.handle_virtual_workspace_command(
                &mut window_store,
                space,
                &LayoutCommand::MoveWindowToWorkspace {
                    workspace: WorkspaceSelector::Name(target.into()),
                    follow: false,
                    window_id: Some(wid.idx.get()),
                },
            );
            assert_eq!(
                engine.virtual_workspace_manager.workspace_for_window(&window_store, space, wid),
                Some(expected)
            );
        }
    }

    #[test]
    fn move_window_to_workspace_can_follow_the_window() {
        let mut window_store = WindowStore::default();
        let mut engine = test_engine();
        let space = SpaceId::new(96);
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 1000.0));
        let pid: pid_t = 6002;
        let wid = WindowId::new(pid, 1);

        let _ =
            engine.handle_event(&mut window_store, LayoutEvent::SpaceExposed(space, screen.size));
        let _ = engine.handle_event(
            &mut window_store,
            LayoutEvent::WindowsOnScreenUpdated(
                space,
                pid,
                vec![(
                    wid,
                    None,
                    None,
                    None,
                    false,
                    true,
                    CGSize::new(500.0, 500.0),
                    None,
                    None,
                )],
                None,
            ),
        );
        let _ = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::CreateWorkspace,
        );
        let target_workspace = engine.virtual_workspace_manager_mut().list_workspaces(space)[1].0;

        let response = engine.handle_virtual_workspace_command(
            &mut window_store,
            space,
            &LayoutCommand::MoveWindowToWorkspace {
                workspace: WorkspaceSelector::Name("next".into()),
                follow: true,
                window_id: Some(wid.idx.get()),
            },
        );

        assert_eq!(engine.active_workspace(space), Some(target_workspace));
        assert_eq!(response.focus_window, Some(wid));
        assert_eq!(
            engine.virtual_workspace_manager.workspace_for_window(&window_store, space, wid),
            Some(target_workspace)
        );
    }
}
