//! Focus moving along a strip, out of the floating layer into it, and across to the display above
//! or below, which is where a strip ends and the next display's begins.

use std::cmp::Ordering;

use objc2_core_foundation::CGPoint;
use rini_core::ids::{SpaceId, WindowId};
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, warn};

use crate::layout::LayoutId;
use crate::layout::domain::boundary::strip_edge;
use crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId;
use crate::workspaces::engine::{EventResponse, LayoutEngine};
use crate::workspaces::{Direction, WindowStore};

impl LayoutEngine {
    pub(super) fn filter_active_workspace_windows(
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

    pub(super) fn filter_active_workspace_window(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        window: Option<WindowId>,
    ) -> Option<WindowId> {
        window.filter(|wid| self.is_window_in_active_workspace(window_store, space, *wid))
    }

    pub(super) fn apply_focus_response(
        &mut self,
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
            edge_hit: None,
            moved: None,
        };
        self.apply_focus_response(space, ws_id, layout, &response);
        response
    }

    pub(super) fn move_focus_internal(
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
            // own selection. See "Navigation" in `src/layout/docs/strip.md`.
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
                edge_hit: None,
                moved: None,
            };
            self.apply_focus_response(space, ws_id, layout, &response);
            return response;
        }

        if let Some(prev_wid) = previous_selection {
            let _ = self.workspace_tree_mut(ws_id).select_window(layout, prev_wid);
        }
        // A strip end stops here: each display is its own strip. Only a vertical command, which
        // is not along a strip at all, may look to the adjacent display.
        let adjacent_space = match strip_edge(direction) {
            Some(_) => None,
            None => self.next_space_for_direction(
                space,
                direction,
                visible_spaces,
                visible_space_centers,
            ),
        };
        if let Some(new_space) = adjacent_space
            && let Some(response) = self.focus_adjacent_space(window_store, new_space, direction)
        {
            return response;
        }

        // The strip stops at its edge; it does not fall into the floating layer (`src/layout/docs/strip.md`).
        self.focus_strip_end(window_store, space, ws_id, layout, previous_selection, direction)
    }

    /// Focus carried onto `new_space`, the display a vertical move reached: to its window that way,
    /// else its first visible one. `None` when it has no window to take focus, so the move stops
    /// at this strip's end instead, and a no-op answer when it has no layout at all.
    fn focus_adjacent_space(
        &mut self,
        window_store: &WindowStore,
        new_space: SpaceId,
        direction: Direction,
    ) -> Option<EventResponse> {
        let Some((new_ws_id, new_layout)) = self.workspace_and_layout(new_space) else {
            debug!(
                "No active workspace/layout for adjacent space {:?}; skipping cross-space focus",
                new_space
            );
            return Some(EventResponse::default());
        };
        let windows_in_new_space = self.filter_active_workspace_windows(
            window_store,
            new_space,
            self.workspace_tree(new_ws_id).visible_windows_in_layout(new_layout),
        );
        let target_window = self
            .filter_active_workspace_window(
                window_store,
                new_space,
                self.workspace_tree(new_ws_id).window_in_direction(new_layout, direction),
            )
            .or_else(|| windows_in_new_space.first().copied())?;
        let _ = self.workspace_tree_mut(new_ws_id).select_window(new_layout, target_window);
        let response = EventResponse {
            changed: true,
            focus_window: Some(target_window),
            raise_windows: windows_in_new_space,
            edge_hit: None,
            moved: None,
        };
        self.apply_focus_response(new_space, new_ws_id, new_layout, &response);
        Some(response)
    }

    /// Focus kept on this strip when a move found nowhere to go: on the selection it had, else its
    /// first visible window, reporting the edge it stopped at.
    fn focus_strip_end(
        &mut self,
        window_store: &WindowStore,
        space: SpaceId,
        ws_id: VirtualWorkspaceId,
        layout: LayoutId,
        previous_selection: Option<WindowId>,
        direction: Direction,
    ) -> EventResponse {
        let visible_windows = self.filter_active_workspace_windows(
            window_store,
            space,
            self.workspace_tree(ws_id).visible_windows_in_layout(layout),
        );
        let Some(fallback_focus) = self
            .filter_active_workspace_window(window_store, space, previous_selection)
            .or_else(|| visible_windows.first().copied())
        else {
            return EventResponse::default();
        };
        let response = EventResponse {
            changed: true,
            focus_window: Some(fallback_focus),
            raise_windows: vec![],
            edge_hit: strip_edge(direction),
            moved: None,
        };
        self.apply_focus_response(space, ws_id, layout, &response);
        response
    }

    pub(super) fn next_space_for_direction(
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
            if let Some(candidate_center) = space_centers.get(&candidate_space)
                && let Some(delta) =
                    Self::directional_delta(direction, current_center, candidate_center)
            {
                candidates.push((candidate_space, delta));
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

    /// Moves focus between the strip and the floating layer, without moving any window.
    pub(super) fn toggle_focus_floating(
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
                edge_hit: None,
                moved: None,
            };
            self.apply_focus_response(space, workspace_id, layout, &response);
            response
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
                edge_hit: None,
                moved: None,
            };
            self.apply_focus_response(space, workspace_id, layout, &response);
            response
        }
    }
}
