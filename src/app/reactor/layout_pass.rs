//! A layout event handed to the engine and its response carried out, and the layout pass that
//! places every active space's windows after it.

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{SpaceId, WindowId, pid_t};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use tracing::{debug, trace};

use super::events::ArrangeRequest;
use super::{Reactor, RefocusState, WorkspaceSwitchOrigin, WorkspaceSwitchState};
use crate::displays::domain::screen::order_visible_spaces_by_position;
use crate::windows::domain::raise::{self, RaiseRequest};
use crate::windows::domain::raise_order;
use crate::windows::domain::request::{AppThreadHandle, Quiet};
use crate::workspaces::{self, LayoutEngine, LayoutEvent};

impl Reactor {
    pub(super) fn send_layout_event(&mut self, event: LayoutEvent) {
        let event_space = match &event {
            LayoutEvent::WindowFocused(space, _) => Some(*space),
            _ => None,
        };
        let focus_desktop = matches!(
            event,
            LayoutEvent::WindowRemoved(wid)
                if self.layout_engine.focused_window() == Some(wid)
        );
        let event_clone = event.clone();
        let layout_outcome = self.layout_engine.handle_event(
            &mut self.state.windows,
            &mut self.state.display_memory,
            event,
        );
        let mut response = layout_outcome.response;
        let (placements, resizes, workspace_focus) = layout_outcome.app_rules.into_parts();
        self.apply_app_rule_placements(placements);
        self.apply_app_rule_resizes(resizes);
        let workspace_switch_space = workspace_focus.map(|request| request.space);
        if let Some(request) = workspace_focus {
            self.store_current_floating_positions(request.space);
            self.workspace_switch_manager
                .start_workspace_switch(WorkspaceSwitchOrigin::Auto);
            response = self.layout_engine.switch_to_workspace_with_focus(
                &self.state.windows,
                request.space,
                request.workspace_index,
                request.window,
            );
        }
        let geometry_changed = response.changed;
        self.prepare_refocus_after_layout_event(&event_clone);
        self.handle_layout_response(response, workspace_switch_space);
        if geometry_changed {
            let pass = if workspace_switch_space.is_some() {
                Pass::WorkspaceSwitch
            } else {
                Pass::Plain
            };
            let layout_changed = self.update_layout(pass, workspace_switch_space.or(event_space));
            // A focus report onto a parked window (cmd-tab) scrolls it in here; if it was behind a
            // floating window it has to be lifted now that it is on screen.
            if layout_changed {
                self.regroup_after_layout();
            }
        }
        if focus_desktop && let Some(space) = self.workspace_command_space() {
            self.focus_desktop_if_active_workspace_empty(space);
        }
        for space in self.space_state.iter_known_spaces() {
            self.layout_engine.debug_tree_desc(space, "after event");
        }
    }

    pub(super) fn handle_layout_response(
        &mut self,
        response: workspaces::EventResponse,
        workspace_switch_space: Option<SpaceId>,
    ) {
        if self.is_in_drag() {
            self.workspace_switch_manager.mark_workspace_switch_inactive();
            return;
        }

        let mut pending_refocus_space =
            match std::mem::replace(&mut self.refocus_manager.refocus_state, RefocusState::None) {
                RefocusState::Pending(space) => Some(space),
                RefocusState::None => None,
            };
        let workspaces::EventResponse {
            changed,
            raise_windows,
            mut focus_window,
            edge_hit,
            moved,
        } = response;
        if moved.is_some() {
            self.moved_along_strip = moved;
        }

        let blocked = !changed && raise_windows.is_empty() && focus_window.is_none();
        if self.bounce_at_edge(edge_hit, workspace_switch_space, blocked) {
            return;
        }

        let switch_target = self.switch_target(workspace_switch_space, &mut focus_window);
        let original_focus = focus_window;
        let focus_quiet = workspace_switch_space.map_or(Quiet::No, |_| Quiet::Yes);

        let handled_without_raise = raise_windows.is_empty()
            && focus_window.is_none()
            && self.focus_without_raise(
                workspace_switch_space,
                &mut pending_refocus_space,
                &mut focus_window,
            );
        self.drop_focus_out_of_sight(switch_target, workspace_switch_space, &mut focus_window);

        if raise_windows.is_empty() && focus_window.is_none() {
            if handled_without_raise {
                self.workspace_switch_manager.mark_workspace_switch_inactive();
            }
            if handled_without_raise
                || matches!(
                    self.workspace_switch_manager.workspace_switch_state,
                    WorkspaceSwitchState::Inactive
                )
            {
                return;
            }
        }

        if let Some(space) = pending_refocus_space {
            // Preserve the pending refocus request if it was not consumed above.
            if matches!(self.refocus_manager.refocus_state, RefocusState::None) {
                self.refocus_manager.refocus_state = RefocusState::Pending(space);
            }
        }

        let (raise_windows, app_handles) =
            self.raise_list(raise_windows, original_focus, focus_window);
        let focus_window = focus_window.filter(|wid| self.is_window_on_active_space(*wid));
        if let Some(space) = workspace_switch_space {
            self.layout_engine
                .commit_workspace_focus(&mut self.state.windows, space, focus_window);
        }
        let batches = raise_order::group_by_app_and_space(&raise_windows, |wid| {
            self.affinity().best_space_for_window_id(wid)
        });
        let focus_window_with_warp = focus_window.map(|wid| (wid, self.warp_for_focus(wid)));

        self.dispatch_raise(raise::Event::RaiseRequest(RaiseRequest {
            raise_windows: batches,
            focus_window: focus_window_with_warp,
            app_handles,
            focus_quiet,
        }));
    }

    /// The view bounced when the command ran into an end of the strip or of the workspace stack, so
    /// the stop reads as an edge, not a dropped keypress. Whether the response ends there: a blocked
    /// workspace step changed nothing and names no window, and the switch-active focus fallbacks
    /// would otherwise re-focus whatever sits under the cursor.
    fn bounce_at_edge(
        &mut self,
        edge_hit: Option<workspaces::Direction>,
        workspace_switch_space: Option<SpaceId>,
        blocked: bool,
    ) -> bool {
        let Some(direction) = edge_hit else { return false };
        let space = workspace_switch_space.or_else(|| self.command_context_space());
        if let Some(space) = space {
            self.start_edge_bounce(space, direction);
        }
        if blocked {
            self.workspace_switch_manager.mark_workspace_switch_inactive();
        }
        blocked
    }

    /// The window a workspace switch is FOR, with `focus_window` moved onto the active workspace's
    /// visible candidate; `None` outside an active switch.
    ///
    /// It is parked until the layout lands, so the window server may well report it off screen right
    /// now. The visibility filters after this must not trade it for whichever window of the
    /// workspace happens to be visible (an auto-switch onto Kiro landed on a Ghostty window that
    /// way, and remembered Ghostty for the next time).
    fn switch_target(
        &mut self,
        workspace_switch_space: Option<SpaceId>,
        focus_window: &mut Option<WindowId>,
    ) -> Option<WindowId> {
        let space = workspace_switch_space?;
        if !matches!(
            self.workspace_switch_manager.workspace_switch_state,
            WorkspaceSwitchState::Active
        ) {
            return None;
        }
        *focus_window = self.visible_focus_candidate_in_active_workspace(space, *focus_window);
        *focus_window
    }

    /// Where focus goes when the response names no window and raises none: under an active switch
    /// the window under the cursor, the empty workspace's desktop or a warp; otherwise a pending
    /// refocus's last focused window. Whether focus was settled without a raise.
    fn focus_without_raise(
        &mut self,
        workspace_switch_space: Option<SpaceId>,
        pending_refocus_space: &mut Option<SpaceId>,
        focus_window: &mut Option<WindowId>,
    ) -> bool {
        if matches!(
            self.workspace_switch_manager.workspace_switch_state,
            WorkspaceSwitchState::Active
        ) && !self.is_in_drag()
        {
            if let Some(wid) = self.window_id_under_cursor() {
                // Avoid duplicate focus events for the already focused window.
                if self.main_window() != Some(wid) {
                    *focus_window = Some(wid);
                }
                false
            } else {
                let skip_center_warp = workspace_switch_space
                    .map(|space| {
                        self.layout_engine
                            .windows_in_active_workspace(&self.state.windows, space)
                            .is_empty()
                    })
                    .unwrap_or(false);
                if skip_center_warp {
                    workspace_switch_space
                        .is_some_and(|space| self.focus_desktop_if_active_workspace_empty(space))
                } else {
                    let space = workspace_switch_space.or_else(|| self.command_context_space());
                    self.try_focus_or_warp_without_raise(space, focus_window)
                }
            }
        } else if let Some(space) = pending_refocus_space.take() {
            if let Some(wid) = self.last_focused_window_in_space(space) {
                *focus_window = Some(wid);
                false
            } else if !self.is_in_drag() {
                self.try_focus_or_warp_without_raise(Some(space), focus_window)
            } else {
                false
            }
        } else {
            false
        }
    }

    /// `focus_window` dropped when it is out of sight, the switch's own target excepted, or off
    /// every active space; under a switch, a warp tried in its place.
    fn drop_focus_out_of_sight(
        &mut self,
        switch_target: Option<WindowId>,
        workspace_switch_space: Option<SpaceId>,
        focus_window: &mut Option<WindowId>,
    ) {
        let Some(wid) = *focus_window else { return };
        let Some(state) = self.state.windows.window(wid) else {
            return;
        };
        let Some(wsid) = state.info.sys_id else { return };
        let is_visible = self.state.windows.is_window_visible(wsid) || switch_target == Some(wid);
        let best_space = self.affinity().best_space_for_window_state(state);
        if !is_visible {
            *focus_window = None;
            if let Some(space) = workspace_switch_space
                && !self.is_in_drag()
            {
                let _ = self.try_focus_or_warp_without_raise(Some(space), focus_window);
            }
        } else if !best_space.is_some_and(|space| self.is_space_active(space)) {
            *focus_window = None;
        }
    }

    /// The windows to raise, in raise order, and the handles of every application they and the
    /// focus belong to: those on an active space and not parked, led by the strip group to lift
    /// when focus lands behind a floating window.
    fn raise_list(
        &mut self,
        raise_windows: Vec<WindowId>,
        original_focus: Option<WindowId>,
        focus_window: Option<WindowId>,
    ) -> (Vec<WindowId>, HashMap<pid_t, AppThreadHandle>) {
        let mut app_handles = HashMap::default();
        for &wid in raise_windows.iter() {
            self.insert_app_handle_for_window(&mut app_handles, wid);
        }

        if let Some(wid) = original_focus {
            self.insert_app_handle_for_window(&mut app_handles, wid);
        }

        let raise_windows: Vec<WindowId> = raise_windows
            .into_iter()
            .filter(|wid| self.is_window_on_active_space(*wid))
            .collect();

        let mut raise_windows = raise_order::drop_parked(raise_windows, |wid| {
            self.affinity().is_window_parked_offscreen(wid)
        });

        // The strip goes up as one group. A focus move raises only the window it lands on, which leaves a
        // floating window in front of the columns beside it. See `raise_order::regroup_tiled`.
        if let Some(target) = focus_window
            && let Some(space) = self.affinity().best_space_for_window_id(target)
        {
            let regroup = self.strip_group_to_lift(space, target);
            if !regroup.is_empty() {
                debug!(
                    focused = target.idx.get(),
                    windows = regroup.len(),
                    "a floating window was in front of the strip; lifting the whole strip over it"
                );
                for wid in &regroup {
                    self.insert_app_handle_for_window(&mut app_handles, *wid);
                }
                self.regroup_raised = regroup.clone();
                raise_windows = raise_order::lead_with_regroup(raise_windows, &regroup);
            }
        }
        (raise_windows, app_handles)
    }

    /// Where the mouse goes with focus on `wid`: its centre when the mouse follows focus, except
    /// during a workspace switch, which holds the warp until its layout lands.
    pub(super) fn warp_for_focus(&mut self, wid: WindowId) -> Option<CGPoint> {
        if !self.config.settings.warp_mouse_to_focus {
            return None;
        }
        if self.workspace_switch_manager.workspace_switch_state == WorkspaceSwitchState::Active {
            self.workspace_switch_manager.pending_workspace_mouse_warp = Some(wid);
            return None;
        }
        self.affinity().window_center_on_known_screen(wid)
    }

    pub(crate) fn visible_spaces_for_layout(
        &self,
        include_inactive: bool,
    ) -> (Vec<SpaceId>, HashMap<SpaceId, CGPoint>) {
        let visible_spaces_input: Vec<(SpaceId, CGPoint)> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| {
                let space = screen.space?;
                if !include_inactive && !self.is_space_active(space) {
                    return None;
                }
                Some((space, screen.frame.mid()))
            })
            .collect();

        let mut visible_space_centers = HashMap::default();
        for (space, center) in &visible_spaces_input {
            visible_space_centers.insert(*space, *center);
        }

        let visible_spaces = order_visible_spaces_by_position(visible_spaces_input.iter().cloned());

        (visible_spaces, visible_space_centers)
    }

    pub(super) fn store_current_floating_positions(&mut self, space: SpaceId) {
        let floating_windows_in_workspace = self
            .layout_engine
            .windows_in_active_workspace(&self.state.windows, space)
            .into_iter()
            .filter(|&wid| self.layout_engine.is_window_floating(wid))
            .filter_map(|wid| {
                self.state
                    .windows
                    .window(wid)
                    .map(|window_state| (wid, window_state.frame_monotonic))
            })
            .collect::<Vec<_>>();

        if !floating_windows_in_workspace.is_empty() {
            self.layout_engine
                .store_floating_window_positions(space, &floating_windows_in_workspace);
        }
    }

    pub(crate) fn update_layout(&mut self, pass: Pass, space_scope: Option<SpaceId>) -> bool {
        self.refresh_borders();
        let layout_result = self.calculate_layout(space_scope);
        let changed = self.apply_layout(layout_result, pass);
        if changed {
            // Re-observe affinity before saving, so the file records the arrangement that
            // was just applied. This is the path that catches a window dragged between
            // displays, a new window opened, and any reshuffle of the strip — none of which
            // produce a display snapshot, so none of which used to update affinity at all.
            self.sync_display_affinity_from_live_layout();
            self.autosave_layout();
        }
        changed
    }

    fn calculate_layout(&mut self, space_scope: Option<SpaceId>) -> LayoutResult {
        if self.state.windows.tracked_window_count() == 0 {
            return LayoutResult::new();
        }
        let screens = self.space_state.screens.clone();
        let all_screen_frames: Vec<CGRect> = screens.iter().map(|s| s.frame).collect();
        let active_space_count = screens
            .iter()
            .filter_map(|screen| screen.space)
            .filter(|space| self.is_space_active(*space))
            .count();
        let mut layout_result = LayoutResult::new();

        for screen in screens {
            let Some(space) = screen.space else {
                continue;
            };
            if space_scope.is_some_and(|scope| scope != space) {
                continue;
            }
            if !self.is_space_active(space) {
                continue;
            }
            let display_uuid_opt = screen.display_uuid_owned();
            let gaps = self
                .config
                .settings
                .layout
                .gaps
                .effective_for_display(display_uuid_opt.as_ref());
            // A reconnected display's layout is NOT reattached here any more.
            //
            // This ran a whole-space remap on every layout pass, from the display's last
            // known space id onto its current one. Two problems: remap_space deletes the
            // workspaces already on the target id and drops their window assignments, and a
            // layout pass is far too hot a path to be mutating space identity from — it fired
            // on ordinary space switches too. Window placement across a display change is now
            // decided by per-window affinity in Reactor::repatriate_windows_to_display, which
            // runs once per topology change.
            self.layout_engine.update_space_display(
                &mut self.state.display_memory,
                space,
                display_uuid_opt.clone(),
            );
            let mut layout = self.layout_engine.calculate_layout_with_virtual_workspaces(
                &self.state.windows,
                space,
                screen.frame,
                &gaps,
                |wid| self.state.windows.window(wid).map(|w| w.frame_monotonic),
                &all_screen_frames,
            );
            if active_space_count > 1 {
                let active_workspace_windows: HashSet<WindowId> = self
                    .layout_engine
                    .windows_in_active_workspace(&self.state.windows, space)
                    .into_iter()
                    .collect();
                bound_scrolling_tiled_frames_to_screen(
                    &self.layout_engine,
                    &mut layout,
                    screen.frame,
                    &active_workspace_windows,
                );
            }
            layout_result.push((space, layout));
        }

        layout_result
    }

    fn apply_layout(&mut self, layout_result: LayoutResult, pass: Pass) -> bool {
        let main_window = self.main_window();
        trace!(?main_window);
        // Skipped for the whole drag, not one pass: a drag produces many passes, each of which would
        // reassert the window's stored frame, which for a floating window is its saved position, so
        // releasing the mouse snapped it back. The marker is cleared where the drag ends: on mouse-up
        // (`events/drag.rs`) and when the window stops being dragged (`drag_swap.rs`).
        let skip_wid = self
            .drag_manager
            .skip_layout_for_window
            .or(self.drag_manager.drag_swap.dragged());
        let mut any_frame_changed = false;

        for (space, layout) in layout_result {
            let movement = self.take_workspace_movement(space);
            any_frame_changed |= match pass {
                Pass::WorkspaceSwitch => {
                    self.workspace_switch_layout(space, movement, &layout, skip_wid)
                }
                _ if self.workspace_switch_manager.active_workspace_switch.is_some() => {
                    self.instant_layout(space, &layout, skip_wid)
                }
                _ => self.animate_layout(space, &layout, pass == Pass::Resize, skip_wid),
            };
        }

        any_frame_changed
    }
}

type LayoutResult = Vec<(SpaceId, Vec<(WindowId, CGRect)>)>;

/// What a layout pass is for, which decides how its frames reach the windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pass {
    /// Frames animate, or land at once while a workspace switch is still in flight.
    Plain,
    /// After a window reported a size change: as `Plain`, except that frames land at once while
    /// the user drags an edge.
    Resize,
    /// The workspace switch's own layout, a reported size change or not.
    WorkspaceSwitch,
}

impl Pass {
    /// The pass `arrange` runs while the workspace switch is in `switch`.
    pub(crate) fn of(arrange: &ArrangeRequest, switch: &WorkspaceSwitchState) -> Self {
        match (switch, arrange.is_resize) {
            (WorkspaceSwitchState::Active, _) => Self::WorkspaceSwitch,
            (WorkspaceSwitchState::Inactive, true) => Self::Resize,
            (WorkspaceSwitchState::Inactive, false) => Self::Plain,
        }
    }
}

/// How much of a fully-scrolled-away window stays on screen, in points.
///
/// macOS will not let a window be positioned entirely outside every display — it
/// clamps it back — so a scrolling layout has to leave a deliberate sliver
/// visible at the edge. 1pt is the smallest value that still satisfies the window
/// server while being visually negligible.
///
/// Was 10pt, which is individually small but stacks: with a dozen scrolled-away
/// columns parked at the same edge the result is a conspicuous ~10pt band of
/// overlapping window edges on both sides of the screen, and it is worse on a
/// wide external display where more columns are off-strip.
const WINDOW_HIDDEN_THRESHOLD: f64 = 1.0;

fn bound_frame_to_screen(frame: CGRect, screen: CGRect) -> CGRect {
    let screen_left = screen.origin.x;
    let screen_top = screen.origin.y;
    let screen_right = screen.max().x;
    let screen_bottom = screen.max().y;
    let max_y = (screen_bottom - frame.size.height).max(screen_top);
    let x = if frame.max().x <= screen_left {
        screen_left - frame.size.width + WINDOW_HIDDEN_THRESHOLD
    } else if frame.origin.x >= screen_right {
        screen_right - WINDOW_HIDDEN_THRESHOLD
    } else {
        frame.origin.x
    };

    CGRect::new(
        CGPoint::new(x, frame.origin.y.clamp(screen_top, max_y)),
        frame.size,
    )
}

// A scrolled-away column keeps the stacking it had, so a parked sliver can sit on top of a window
// on screen. AX has AXRaise and no lower, and the sliver is another app's window: SkyLight orders
// and levels only the caller's own windows, and AppKit orders rini's own against another
// process's (src/decorations/docs/rest-windows.md, "Ordering a rest window"), which cannot lower that
// app's. Keeping the sliver small is the mitigation.

fn bound_scrolling_tiled_frames_to_screen(
    engine: &LayoutEngine,
    layout: &mut [(WindowId, CGRect)],
    screen: CGRect,
    active_workspace_windows: &HashSet<WindowId>,
) {
    for (wid, frame) in layout.iter_mut() {
        if !active_workspace_windows.contains(wid) || engine.is_window_floating(*wid) {
            continue;
        }
        *frame = bound_frame_to_screen(*frame, screen);
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};

    use super::{
        ArrangeRequest, Pass, WINDOW_HIDDEN_THRESHOLD, WorkspaceSwitchState, bound_frame_to_screen,
    };

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    #[test]
    fn a_switch_outranks_a_resize_and_a_resize_outranks_a_plain_pass() {
        use WorkspaceSwitchState::{Active, Inactive};
        for (is_resize, switch, pass) in [
            (false, Inactive, Pass::Plain),
            (true, Inactive, Pass::Resize),
            (false, Active, Pass::WorkspaceSwitch),
            (true, Active, Pass::WorkspaceSwitch),
        ] {
            let arrange = ArrangeRequest {
                is_resize,
                ..ArrangeRequest::default()
            };
            assert_eq!(Pass::of(&arrange, &switch), pass, "{is_resize}, {switch:?}");
        }
    }

    #[test]
    fn bound_frame_to_screen_keeps_partial_overlap_for_strip_behavior() {
        let screen = rect(2000.0, 0.0, 1000.0, 800.0);
        let frame = rect(1500.0, 50.0, 700.0, 400.0);
        let bounded = bound_frame_to_screen(frame, screen);
        assert_eq!(bounded.origin.x, 1500.0);
        assert_eq!(bounded.size.width, 700.0);
    }

    #[test]
    fn bound_frame_to_screen_parks_fully_offscreen_windows_to_hidden_sliver() {
        let screen = rect(2000.0, 0.0, 1000.0, 800.0);
        let frame = rect(1200.0, 80.0, 600.0, 300.0);
        let bounded = bound_frame_to_screen(frame, screen);
        // Derived from WINDOW_HIDDEN_THRESHOLD rather than hardcoded, so tuning the
        // sliver width does not require editing the expectations.
        assert_eq!(bounded.origin.x, 2000.0 - 600.0 + WINDOW_HIDDEN_THRESHOLD);
        assert_eq!(bounded.size.width, 600.0);
    }

    #[test]
    fn bound_frame_to_screen_parks_right_offscreen_windows_to_hidden_sliver() {
        let screen = rect(2000.0, 0.0, 1000.0, 800.0);
        let frame = rect(3200.0, 80.0, 600.0, 300.0);
        let bounded = bound_frame_to_screen(frame, screen);
        assert_eq!(bounded.origin.x, 3000.0 - WINDOW_HIDDEN_THRESHOLD);
        assert_eq!(bounded.size.width, 600.0);
    }

    /// The sliver must stay small enough not to read as a visible band of window
    /// edges when many columns are parked at the same screen edge, but non-zero:
    /// macOS refuses to leave a window entirely outside every display.
    #[test]
    fn hidden_sliver_is_minimal_but_nonzero() {
        const {
            assert!(
                WINDOW_HIDDEN_THRESHOLD > 0.0 && WINDOW_HIDDEN_THRESHOLD <= 2.0,
                "sliver must be >0 (macOS clamps fully off-screen windows back) and \
                 small enough to be visually negligible"
            )
        };
    }

    #[test]
    fn bound_frame_to_screen_does_not_park_partially_visible_right_windows() {
        let screen = rect(2000.0, 0.0, 1000.0, 800.0);
        let frame = rect(2998.0, 80.0, 600.0, 300.0);
        let bounded = bound_frame_to_screen(frame, screen);
        assert_eq!(bounded.origin.x, 2998.0);
        assert_eq!(bounded.size.width, 600.0);
    }
}
