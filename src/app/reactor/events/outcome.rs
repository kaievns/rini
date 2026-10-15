use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{SpaceId, WindowId, WindowServerId, pid_t};

use crate::app::config::Config;
use crate::app::controller::WmEvent;
use crate::windows::domain::info::{AppInfo, WindowInfo, WindowServerInfo};
use crate::windows::domain::raise;
use crate::windows::domain::request::Request;
use crate::workspaces::{EventResponse, LayoutEvent};

#[derive(Debug)]
pub(crate) struct WindowDiscoveryRequest {
    pub(crate) pid: pid_t,
    pub(crate) new: Vec<(WindowId, WindowInfo)>,
    pub(crate) known_visible: Vec<WindowId>,
    pub(crate) app_info: Option<AppInfo>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WindowFrameWriteRequest {
    pub(crate) window: WindowId,
    pub(crate) frame: CGRect,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TopologyReassignment {
    pub(crate) window: WindowId,
    pub(crate) space: SpaceId,
}

/// Follow-up work requested by an event workflow.
///
/// Workflows mutate reactor-owned domain state synchronously, then describe the
/// ordered integration work which must happen after the mutation.  Keeping the
/// description small and concrete makes it possible to test policy without
/// turning platform operations into a generic effect system.
#[derive(Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each asks for a follow-up of its own, any of them together"
)]
pub(crate) struct EventOutcome {
    pub(crate) window_server_updates: Vec<WindowServerInfo>,
    pub(crate) discoveries: Vec<WindowDiscoveryRequest>,
    pub(crate) recompute_active_spaces: bool,
    pub(crate) repair_spaces_after_mission_control: bool,
    pub(crate) refresh_after_mission_control: bool,
    pub(crate) force_refresh_all_windows: bool,
    pub(crate) wm_events: Vec<WmEvent>,
    pub(crate) app_requests: Vec<(pid_t, Request)>,
    pub(crate) topology_reassignments: Vec<TopologyReassignment>,
    /// Windows a workflow removed, whose pictures the pictures service forgets.
    pub(crate) forgotten_windows: Vec<WindowId>,
    pub(crate) confirmed_window_spaces: Vec<(WindowServerId, SpaceId)>,
    pub(crate) fullscreen_restorations: Vec<(WindowServerId, SpaceId, WindowId)>,
    pub(crate) raise_requests: Vec<raise::Event>,
    pub(crate) make_key_windows: Vec<(pid_t, WindowServerId)>,
    pub(crate) mouse_warps: Vec<CGPoint>,
    pub(crate) pre_layout_window_frame_writes: Vec<WindowFrameWriteRequest>,
    pub(crate) drag_swap_evaluations: Vec<(WindowId, CGRect)>,
    pub(crate) dispatch_mouse_up: bool,
    pub(crate) close_window: Option<Option<WindowServerId>>,
    pub(crate) service_config_update: Option<Config>,
    pub(crate) stdout_lines: Vec<String>,
    pub(crate) reapply_app_rules: Vec<WindowId>,
    pub(crate) finalize_created_windows: Vec<WindowId>,
    pub(crate) layout_events: Vec<LayoutEvent>,
    pub(crate) layout_responses: Vec<(EventResponse, Option<SpaceId>)>,
    pub(crate) arrange: ArrangeRequest,
    pub(crate) focused_window: Option<WindowId>,
    /// A layout event of the work already taken out before the arrange focused a window.
    pub(crate) focus_landed_before_arrange: bool,
    pub(crate) refresh_window_notifications: bool,
    pub(crate) refresh_focus_follows_mouse: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ArrangeRequest {
    pub(crate) requested: bool,
    pub(crate) passes: u8,
    pub(crate) is_resize: bool,
    pub(crate) window_was_destroyed: bool,
    pub(crate) space_scope: Option<SpaceId>,
}

impl ArrangeRequest {
    /// How many arrange passes to run, or `None` for none.
    ///
    /// A drag suppresses arranging: the user is holding the window and a pass would fight them. The
    /// exception is a window that was DESTROYED mid-drag, where the drag is over as far as the strip
    /// is concerned and the gap it left has to close.
    ///
    /// A requested arrange always runs at least once. `passes` is 0 on the constructors that set
    /// `requested` directly, and taking that literally would ask for an arrange and then not do one.
    pub(crate) fn passes_to_run(&self, in_drag: bool) -> Option<u8> {
        if !self.requested || (in_drag && !self.window_was_destroyed) {
            return None;
        }
        Some(self.passes.max(1))
    }
}

impl EventOutcome {
    /// The event was observed, but it does not require any follow-up work.
    pub(crate) fn no_change() -> Self { Self::default() }

    /// Whether focus came to rest somewhere as a result of this event.
    ///
    /// Either the outcome names the window directly, or a layout event does. Both count: a click
    /// reports focus with no layout change at all, and macOS raised only the window that was clicked,
    /// so the strip still needs regrouping over whatever the click put in front of it.
    pub(crate) fn focus_landed(&self) -> bool {
        self.focused_window.is_some()
            || self.focus_landed_before_arrange
            || self
                .layout_events
                .iter()
                .any(|event| matches!(event, LayoutEvent::WindowFocused(..)))
    }

    /// Combines follow-up work produced by nested reducers while preserving
    /// reducer order for every queued operation.
    pub(crate) fn absorb(&mut self, other: Self) {
        // Destructured exhaustively and deliberately. This is the one function that has to see every
        // field, and a field left out of it is follow-up work a nested workflow asked for and silently
        // did not get. Adding a field to `EventOutcome` breaks this until someone says how two of them
        // combine — a question with no safe default, since appending, OR-ing and last-one-wins are each
        // right for different fields here.
        let Self {
            mut window_server_updates,
            mut discoveries,
            recompute_active_spaces,
            repair_spaces_after_mission_control,
            refresh_after_mission_control,
            force_refresh_all_windows,
            mut wm_events,
            mut app_requests,
            mut topology_reassignments,
            mut forgotten_windows,
            mut confirmed_window_spaces,
            mut fullscreen_restorations,
            mut raise_requests,
            mut make_key_windows,
            mut mouse_warps,
            mut pre_layout_window_frame_writes,
            mut drag_swap_evaluations,
            dispatch_mouse_up,
            close_window,
            service_config_update,
            mut stdout_lines,
            mut reapply_app_rules,
            mut finalize_created_windows,
            mut layout_events,
            mut layout_responses,
            arrange,
            focused_window,
            focus_landed_before_arrange,
            refresh_window_notifications,
            refresh_focus_follows_mouse,
        } = other;

        self.window_server_updates.append(&mut window_server_updates);
        self.discoveries.append(&mut discoveries);
        self.recompute_active_spaces |= recompute_active_spaces;
        self.repair_spaces_after_mission_control |= repair_spaces_after_mission_control;
        self.refresh_after_mission_control |= refresh_after_mission_control;
        self.force_refresh_all_windows |= force_refresh_all_windows;
        self.wm_events.append(&mut wm_events);
        self.app_requests.append(&mut app_requests);
        self.topology_reassignments.append(&mut topology_reassignments);
        self.forgotten_windows.append(&mut forgotten_windows);
        self.confirmed_window_spaces.append(&mut confirmed_window_spaces);
        self.fullscreen_restorations.append(&mut fullscreen_restorations);
        self.raise_requests.append(&mut raise_requests);
        self.make_key_windows.append(&mut make_key_windows);
        self.mouse_warps.append(&mut mouse_warps);
        self.pre_layout_window_frame_writes.append(&mut pre_layout_window_frame_writes);
        self.drag_swap_evaluations.append(&mut drag_swap_evaluations);
        self.dispatch_mouse_up |= dispatch_mouse_up;
        self.close_window = close_window.or(self.close_window);
        self.service_config_update = service_config_update.or(self.service_config_update.take());
        self.stdout_lines.append(&mut stdout_lines);
        self.reapply_app_rules.append(&mut reapply_app_rules);
        self.finalize_created_windows.append(&mut finalize_created_windows);
        self.layout_events.append(&mut layout_events);
        self.layout_responses.append(&mut layout_responses);
        if arrange.requested {
            self.arrange.space_scope = if self.arrange.requested {
                match (self.arrange.space_scope, arrange.space_scope) {
                    (Some(existing), Some(incoming)) if existing == incoming => Some(existing),
                    _ => None,
                }
            } else {
                arrange.space_scope
            };
            self.arrange.requested = true;
            self.arrange.passes = self.arrange.passes.saturating_add(arrange.passes).max(1);
            self.arrange.is_resize |= arrange.is_resize;
            self.arrange.window_was_destroyed |= arrange.window_was_destroyed;
        }
        self.focused_window = focused_window.or(self.focused_window);
        self.focus_landed_before_arrange |= focus_landed_before_arrange;
        self.refresh_window_notifications |= refresh_window_notifications;
        self.refresh_focus_follows_mouse |= refresh_focus_follows_mouse;
    }

    /// Takes in work a step of carrying this outcome out handed back, AHEAD of what is left of this
    /// one: the step that asked for it has already been taken, so its queued requests go first and
    /// a single-valued request of this outcome keeps the last word.
    pub(crate) fn absorb_ahead(&mut self, earlier: Self) {
        let rest = std::mem::replace(self, earlier);
        self.absorb(rest);
    }

    /// Moves the work carried out before the arrange into an outcome of its own, or `None` when none
    /// is queued; the arrange and what follows it stay in `self`.
    pub(crate) fn take_work_before_arrange(&mut self) -> Option<Self> {
        use std::mem::take;
        // Exhaustive for the reason `absorb` is: a new field has to say which side of the arrange
        // it is carried out on.
        let Self {
            window_server_updates,
            discoveries,
            recompute_active_spaces,
            repair_spaces_after_mission_control,
            refresh_after_mission_control,
            force_refresh_all_windows,
            wm_events: _,
            app_requests: _,
            topology_reassignments,
            forgotten_windows,
            confirmed_window_spaces,
            fullscreen_restorations,
            raise_requests: _,
            make_key_windows: _,
            mouse_warps: _,
            pre_layout_window_frame_writes,
            drag_swap_evaluations,
            dispatch_mouse_up,
            close_window: _,
            service_config_update: _,
            stdout_lines: _,
            reapply_app_rules,
            finalize_created_windows,
            layout_events,
            layout_responses,
            arrange: _,
            focused_window: _,
            focus_landed_before_arrange,
            refresh_window_notifications: _,
            refresh_focus_follows_mouse: _,
        } = self;
        let queued = *recompute_active_spaces
            || *repair_spaces_after_mission_control
            || *refresh_after_mission_control
            || *force_refresh_all_windows
            || *dispatch_mouse_up
            || !window_server_updates.is_empty()
            || !discoveries.is_empty()
            || !topology_reassignments.is_empty()
            || !forgotten_windows.is_empty()
            || !confirmed_window_spaces.is_empty()
            || !fullscreen_restorations.is_empty()
            || !pre_layout_window_frame_writes.is_empty()
            || !drag_swap_evaluations.is_empty()
            || !reapply_app_rules.is_empty()
            || !finalize_created_windows.is_empty()
            || !layout_events.is_empty()
            || !layout_responses.is_empty();
        if !queued {
            return None;
        }
        *focus_landed_before_arrange |= layout_events
            .iter()
            .any(|event| matches!(event, LayoutEvent::WindowFocused(..)));
        Some(Self {
            window_server_updates: take(window_server_updates),
            discoveries: take(discoveries),
            recompute_active_spaces: take(recompute_active_spaces),
            repair_spaces_after_mission_control: take(repair_spaces_after_mission_control),
            refresh_after_mission_control: take(refresh_after_mission_control),
            force_refresh_all_windows: take(force_refresh_all_windows),
            topology_reassignments: take(topology_reassignments),
            forgotten_windows: take(forgotten_windows),
            confirmed_window_spaces: take(confirmed_window_spaces),
            fullscreen_restorations: take(fullscreen_restorations),
            pre_layout_window_frame_writes: take(pre_layout_window_frame_writes),
            drag_swap_evaluations: take(drag_swap_evaluations),
            dispatch_mouse_up: take(dispatch_mouse_up),
            reapply_app_rules: take(reapply_app_rules),
            finalize_created_windows: take(finalize_created_windows),
            layout_events: take(layout_events),
            layout_responses: take(layout_responses),
            ..Self::default()
        })
    }

    /// The event changed geometry or layout state and requires one arrange pass.
    pub(crate) fn layout_changed(is_resize: bool) -> Self {
        Self {
            window_server_updates: Vec::new(),
            discoveries: Vec::new(),
            recompute_active_spaces: false,
            repair_spaces_after_mission_control: false,
            refresh_after_mission_control: false,
            force_refresh_all_windows: false,
            wm_events: Vec::new(),
            app_requests: Vec::new(),
            topology_reassignments: Vec::new(),
            forgotten_windows: Vec::new(),
            confirmed_window_spaces: Vec::new(),
            fullscreen_restorations: Vec::new(),
            raise_requests: Vec::new(),
            make_key_windows: Vec::new(),
            mouse_warps: Vec::new(),
            pre_layout_window_frame_writes: Vec::new(),
            drag_swap_evaluations: Vec::new(),
            dispatch_mouse_up: false,
            close_window: None,
            service_config_update: None,
            stdout_lines: Vec::new(),
            reapply_app_rules: Vec::new(),
            finalize_created_windows: Vec::new(),
            layout_events: Vec::new(),
            layout_responses: Vec::new(),
            arrange: ArrangeRequest {
                requested: true,
                passes: 1,
                is_resize,
                window_was_destroyed: false,
                space_scope: None,
            },
            focused_window: None,
            focus_landed_before_arrange: false,
            refresh_window_notifications: false,
            refresh_focus_follows_mouse: false,
        }
    }

    /// A window entered, left, or changed its membership in the managed set.
    pub(crate) fn window_membership_changed() -> Self { Self::layout_changed(false) }

    /// The arrange closes a destroyed window's gap, even mid-drag.
    pub(crate) fn with_window_destroyed(mut self) -> Self {
        self.arrange.window_was_destroyed = true;
        self
    }

    /// The windows with notifications changed, so they are subscribed again.
    pub(crate) fn with_window_notification_refresh(mut self) -> Self {
        self.refresh_window_notifications = true;
        self
    }

    /// Focus changed without changing window membership.
    pub(crate) fn focus_changed(
        focused_window: Option<WindowId>,
        refresh_window_notifications: bool,
    ) -> Self {
        Self {
            focused_window,
            refresh_window_notifications,
            ..Self::default()
        }
    }

    pub(crate) fn with_focus_follows_mouse_refresh(mut self) -> Self {
        self.refresh_focus_follows_mouse = true;
        self
    }

    pub(crate) fn window_notification_refresh() -> Self {
        Self {
            refresh_window_notifications: true,
            ..Self::default()
        }
    }

    pub(crate) fn with_layout_event(mut self, event: LayoutEvent) -> Self {
        self.layout_events.push(event);
        self
    }

    pub(crate) fn with_layout_response(
        mut self,
        response: EventResponse,
        workspace_switch_space: Option<SpaceId>,
    ) -> Self {
        self.layout_responses.push((response, workspace_switch_space));
        self
    }

    pub(crate) fn with_active_space_recompute(mut self) -> Self {
        self.recompute_active_spaces = true;
        self
    }

    pub(crate) fn with_mission_control_recovery(mut self) -> Self {
        self.repair_spaces_after_mission_control = true;
        self.refresh_after_mission_control = true;
        self
    }

    pub(crate) fn with_force_window_refresh(mut self) -> Self {
        self.force_refresh_all_windows = true;
        self
    }

    pub(crate) fn with_arrange_passes(mut self, passes: u8) -> Self {
        self.arrange.requested = passes > 0;
        self.arrange.passes = passes;
        self
    }

    pub(crate) fn with_arrange_space_scope(mut self, space_scope: Option<SpaceId>) -> Self {
        self.arrange.space_scope = space_scope;
        self
    }

    pub(crate) fn with_window_server_updates(mut self, updates: Vec<WindowServerInfo>) -> Self {
        self.window_server_updates = updates;
        self
    }

    pub(crate) fn with_discovery(mut self, request: WindowDiscoveryRequest) -> Self {
        self.discoveries.push(request);
        self
    }

    pub(crate) fn with_wm_event(mut self, event: WmEvent) -> Self {
        self.wm_events.push(event);
        self
    }

    pub(crate) fn with_app_request(mut self, pid: pid_t, request: Request) -> Self {
        self.app_requests.push((pid, request));
        self
    }

    pub(crate) fn with_topology_reassignment(mut self, window: WindowId, space: SpaceId) -> Self {
        self.topology_reassignments.push(TopologyReassignment { window, space });
        self
    }

    pub(crate) fn with_forgotten_window(mut self, window: WindowId) -> Self {
        self.forgotten_windows.push(window);
        self
    }

    pub(crate) fn with_confirmed_window_space(
        mut self,
        window_server_id: WindowServerId,
        space: SpaceId,
    ) -> Self {
        self.confirmed_window_spaces.push((window_server_id, space));
        self
    }

    pub(crate) fn with_fullscreen_restoration(
        mut self,
        window_server_id: WindowServerId,
        space: SpaceId,
        window: WindowId,
    ) -> Self {
        self.fullscreen_restorations.push((window_server_id, space, window));
        self
    }

    pub(crate) fn with_raise_request(mut self, request: raise::Event) -> Self {
        self.raise_requests.push(request);
        self
    }

    pub(crate) fn with_make_key_window(mut self, pid: pid_t, window: WindowServerId) -> Self {
        self.make_key_windows.push((pid, window));
        self
    }

    pub(crate) fn with_mouse_warp(mut self, point: CGPoint) -> Self {
        self.mouse_warps.push(point);
        self
    }

    pub(crate) fn with_pre_layout_window_frame_write(
        mut self,
        window: WindowId,
        frame: CGRect,
    ) -> Self {
        self.pre_layout_window_frame_writes
            .push(WindowFrameWriteRequest { window, frame });
        self
    }

    pub(crate) fn with_drag_swap_evaluation(mut self, window: WindowId, frame: CGRect) -> Self {
        self.drag_swap_evaluations.push((window, frame));
        self
    }

    pub(crate) fn with_mouse_up_dispatch(mut self) -> Self {
        self.dispatch_mouse_up = true;
        self
    }

    pub(crate) fn with_close_window(mut self, window_server_id: Option<WindowServerId>) -> Self {
        self.close_window = Some(window_server_id);
        self
    }

    pub(crate) fn with_service_config_update(mut self, config: Config) -> Self {
        self.service_config_update = Some(config);
        self
    }

    pub(crate) fn with_stdout_line(mut self, line: String) -> Self {
        self.stdout_lines.push(line);
        self
    }

    pub(crate) fn with_app_rule_reapply(mut self, window: WindowId) -> Self {
        self.reapply_app_rules.push(window);
        self
    }

    pub(crate) fn with_created_window_finalization(mut self, window: WindowId) -> Self {
        self.finalize_created_windows.push(window);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_no_change_request_no_follow_up_work() {
        for outcome in [EventOutcome::default(), EventOutcome::no_change()] {
            assert!(!outcome.arrange.requested);
            assert_eq!(outcome.arrange.passes, 0);
            assert!(!outcome.refresh_window_notifications);
        }
    }

    #[test]
    fn explicit_change_constructors_request_their_follow_up_work() {
        let outcome = EventOutcome::layout_changed(true);

        assert!(outcome.arrange.requested);
        assert!(outcome.arrange.is_resize);
        assert!(!outcome.arrange.window_was_destroyed);
        assert!(!outcome.refresh_window_notifications);
        assert!(!outcome.refresh_focus_follows_mouse);

        let outcome = EventOutcome::window_membership_changed();
        assert!(outcome.arrange.requested);
        assert!(!outcome.arrange.window_was_destroyed);
        assert!(!outcome.refresh_window_notifications);

        let outcome = EventOutcome::window_membership_changed()
            .with_window_destroyed()
            .with_window_notification_refresh();
        assert!(outcome.arrange.requested);
        assert!(outcome.arrange.window_was_destroyed);
        assert!(outcome.refresh_window_notifications);

        let focused = WindowId::new(42, 7);
        let outcome = EventOutcome::focus_changed(Some(focused), false);
        assert!(!outcome.arrange.requested);
        assert_eq!(outcome.focused_window, Some(focused));
    }

    #[test]
    fn absorbed_arrange_requests_keep_only_a_common_space_scope() {
        let first_space = SpaceId::new(1);
        let second_space = SpaceId::new(2);
        let mut outcome =
            EventOutcome::layout_changed(false).with_arrange_space_scope(Some(first_space));

        outcome.absorb(
            EventOutcome::layout_changed(false).with_arrange_space_scope(Some(first_space)),
        );
        assert_eq!(outcome.arrange.space_scope, Some(first_space));

        outcome.absorb(
            EventOutcome::layout_changed(false).with_arrange_space_scope(Some(second_space)),
        );
        assert_eq!(outcome.arrange.space_scope, None);
    }

    fn requested(passes: u8) -> ArrangeRequest {
        ArrangeRequest {
            requested: true,
            passes,
            ..ArrangeRequest::default()
        }
    }

    #[test]
    fn an_arrange_that_was_not_asked_for_runs_no_passes() {
        assert_eq!(
            ArrangeRequest {
                passes: 3,
                ..ArrangeRequest::default()
            }
            .passes_to_run(false),
            None
        );
    }

    /// A requested arrange always runs at least once. Several constructors set `requested` without
    /// setting `passes`, and taking that literally would ask for an arrange and then not do one.
    #[test]
    fn a_requested_arrange_with_no_pass_count_still_runs_once() {
        assert_eq!(requested(0).passes_to_run(false), Some(1));
    }

    #[test]
    fn a_pass_count_is_taken_as_given() {
        assert_eq!(requested(3).passes_to_run(false), Some(3));
    }

    /// A drag suppresses arranging: the user is holding the window and a pass would fight them.
    #[test]
    fn a_drag_suppresses_the_arrange() {
        assert_eq!(requested(2).passes_to_run(true), None);
    }

    /// Unless the window was destroyed mid-drag. The drag is over as far as the strip is concerned
    /// and the gap it left has to close.
    #[test]
    fn a_window_destroyed_mid_drag_arranges_anyway() {
        assert_eq!(
            ArrangeRequest {
                window_was_destroyed: true,
                ..requested(2)
            }
            .passes_to_run(true),
            Some(2)
        );
    }

    #[test]
    fn a_named_focused_window_is_focus_landing() {
        let outcome = EventOutcome::focus_changed(Some(WindowId::new(1, 1)), false);
        assert!(outcome.focus_landed());
    }

    /// A click reports focus with no layout change at all, and macOS raised only the window that was
    /// clicked, so the strip still needs regrouping over whatever the click put in front of it.
    #[test]
    fn a_layout_focus_event_alone_is_focus_landing() {
        let outcome = EventOutcome::default()
            .with_layout_event(LayoutEvent::WindowFocused(SpaceId::new(1), WindowId::new(1, 1)));
        assert!(outcome.focus_landed());
    }

    #[test]
    fn a_layout_event_that_is_not_a_focus_is_not_focus_landing() {
        let outcome = EventOutcome::default()
            .with_layout_event(LayoutEvent::WindowAdded(SpaceId::new(1), WindowId::new(1, 1)));
        assert!(!outcome.focus_landed());
    }

    #[test]
    fn nothing_happening_is_not_focus_landing() {
        assert!(!EventOutcome::no_change().focus_landed());
    }

    // --- absorb: how two outcomes combine ------------------------------------------------------

    /// Queued work appends in order. A nested workflow's requests come AFTER the outer one's, because
    /// the outer workflow mutated the model first and its follow-ups assume that order.
    #[test]
    fn queued_work_from_a_nested_outcome_comes_after_the_outers() {
        let mut outer = EventOutcome::default();
        outer.stdout_lines.push("outer".into());
        let mut inner = EventOutcome::default();
        inner.stdout_lines.push("inner".into());

        outer.absorb(inner);

        assert_eq!(outer.stdout_lines, ["outer", "inner"]);
    }

    /// A flag is a request, so either side asking is enough. Overwriting instead of OR-ing would let a
    /// nested outcome that asked for nothing cancel what the outer one asked for.
    #[test]
    fn a_flag_either_side_set_stays_set() {
        for (outer_flag, inner_flag) in [(true, false), (false, true), (true, true)] {
            let mut outer = EventOutcome {
                recompute_active_spaces: outer_flag,
                ..EventOutcome::default()
            };
            let inner = EventOutcome {
                recompute_active_spaces: inner_flag,
                ..EventOutcome::default()
            };

            outer.absorb(inner);

            assert!(outer.recompute_active_spaces, "{outer_flag} + {inner_flag}");
        }
    }

    #[test]
    fn a_flag_neither_side_set_stays_unset() {
        let mut outer = EventOutcome::default();
        outer.absorb(EventOutcome::default());
        assert!(!outer.recompute_active_spaces);
        assert!(!outer.refresh_window_notifications);
        assert!(!outer.dispatch_mouse_up);
    }

    /// For a single-valued request the INNER one wins, because it was decided later and with the outer
    /// one's model changes already applied.
    #[test]
    fn a_nested_outcome_overrides_a_single_valued_request() {
        let outer_window = WindowId::new(1, 1);
        let inner_window = WindowId::new(2, 1);
        let mut outer = EventOutcome::focus_changed(Some(outer_window), false);
        let inner = EventOutcome::focus_changed(Some(inner_window), false);

        outer.absorb(inner);

        assert_eq!(outer.focused_window, Some(inner_window));
    }

    /// But a nested outcome that says NOTHING does not erase what the outer one said.
    #[test]
    fn a_nested_outcome_with_nothing_to_say_does_not_erase_the_outers_choice() {
        let window = WindowId::new(1, 1);
        let mut outer = EventOutcome::focus_changed(Some(window), false);

        outer.absorb(EventOutcome::default());

        assert_eq!(outer.focused_window, Some(window));
    }

    // --- absorb_ahead: work handed back mid-drain -----------------------------------------------

    /// Work a step handed back was asked for by a step already taken, so it queues ahead of the
    /// rest of the outcome being carried out.
    #[test]
    fn work_handed_back_queues_ahead_of_the_rest() {
        let mut rest = EventOutcome::default().with_stdout_line("rest".into());
        rest.absorb_ahead(EventOutcome::default().with_stdout_line("handed back".into()));
        assert_eq!(rest.stdout_lines, ["handed back", "rest"]);
    }

    /// The outcome being carried out decided its single-valued requests after the step that handed
    /// work back, so its choice stands; where it made none, the handed-back one does.
    #[test]
    fn the_rest_keeps_the_last_word_on_a_single_valued_request() {
        let (outer, inner) = (WindowId::new(1, 1), WindowId::new(2, 1));
        let mut rest = EventOutcome::focus_changed(Some(outer), false);
        rest.absorb_ahead(EventOutcome::focus_changed(Some(inner), false));
        assert_eq!(rest.focused_window, Some(outer));

        let mut rest = EventOutcome::default();
        rest.absorb_ahead(EventOutcome::focus_changed(Some(inner), false));
        assert_eq!(rest.focused_window, Some(inner));
    }

    /// Both arranges are one: their passes add, as two absorbed requests' do.
    #[test]
    fn work_handed_back_merges_its_arrange_into_the_rest() {
        let mut rest = EventOutcome::layout_changed(false);
        rest.absorb_ahead(EventOutcome::layout_changed(true));
        assert_eq!(rest.arrange.passes_to_run(false), Some(2));
        assert!(rest.arrange.is_resize);
    }

    /// What two steps of one round leave is collected in the order they were handed back, then
    /// taken in ahead of the rest: the first's work goes first and the second's focus beats it.
    #[test]
    fn work_handed_back_twice_in_a_round_keeps_the_order_it_was_handed_back_in() {
        let (first, second, own) = (WindowId::new(1, 1), WindowId::new(2, 1), WindowId::new(3, 1));
        let mut handed_back = EventOutcome::default();
        handed_back.absorb(
            EventOutcome::focus_changed(Some(first), false).with_stdout_line("first".into()),
        );
        handed_back.absorb(
            EventOutcome::focus_changed(Some(second), false).with_stdout_line("second".into()),
        );

        let mut rest = EventOutcome::default().with_stdout_line("rest".into());
        rest.absorb_ahead(handed_back);
        assert_eq!(rest.stdout_lines, ["first", "second", "rest"]);
        assert_eq!(rest.focused_window, Some(second));

        let mut handed_back = EventOutcome::default();
        handed_back.absorb(EventOutcome::focus_changed(Some(first), false));
        handed_back.absorb(EventOutcome::focus_changed(Some(second), false));
        let mut rest = EventOutcome::focus_changed(Some(own), false);
        rest.absorb_ahead(handed_back);
        assert_eq!(rest.focused_window, Some(own));
    }

    // --- take_work_before_arrange --------------------------------------------------------------

    #[test]
    fn nothing_queued_is_no_work_before_the_arrange() {
        assert!(EventOutcome::default().take_work_before_arrange().is_none());
    }

    /// Each request carried out before the arrange is taken, whole, so a step handing one back has
    /// it carried out before the arrange, once.
    #[test]
    fn each_request_carried_out_before_the_arrange_is_taken() {
        let window = WindowId::new(1, 1);
        let space = SpaceId::new(1);
        let wsid = WindowServerId::new(7);
        let requests = [
            EventOutcome::default().with_window_server_updates(vec![WindowServerInfo {
                pid: 1,
                id: wsid,
                layer: 0,
                frame: CGRect::default(),
                min_frame: Default::default(),
                max_frame: Default::default(),
            }]),
            EventOutcome::default().with_discovery(WindowDiscoveryRequest {
                pid: 1,
                new: Vec::new(),
                known_visible: Vec::new(),
                app_info: None,
            }),
            EventOutcome::default().with_active_space_recompute(),
            EventOutcome::default().with_mission_control_recovery(),
            EventOutcome::default().with_force_window_refresh(),
            EventOutcome::default().with_topology_reassignment(window, space),
            EventOutcome::default().with_forgotten_window(window),
            EventOutcome::default().with_confirmed_window_space(wsid, space),
            EventOutcome::default().with_fullscreen_restoration(wsid, space, window),
            EventOutcome::default().with_pre_layout_window_frame_write(window, CGRect::default()),
            EventOutcome::default().with_drag_swap_evaluation(window, CGRect::default()),
            EventOutcome::default().with_mouse_up_dispatch(),
            EventOutcome::default().with_app_rule_reapply(window),
            EventOutcome::default().with_created_window_finalization(window),
            EventOutcome::default().with_layout_event(LayoutEvent::WindowAdded(space, window)),
            EventOutcome::default().with_layout_response(EventResponse::default(), None),
        ];
        for (index, mut outcome) in requests.into_iter().enumerate() {
            assert!(outcome.take_work_before_arrange().is_some(), "request {index}");
            assert!(
                outcome.take_work_before_arrange().is_none(),
                "request {index} again"
            );
        }
    }

    /// The arrange, and what follows it, is carried out once, so asking for it is no work before.
    #[test]
    fn what_follows_the_arrange_is_no_work_before_it() {
        let window = WindowId::new(1, 1);
        let requests = [
            EventOutcome::layout_changed(false),
            EventOutcome::focus_changed(Some(window), true).with_focus_follows_mouse_refresh(),
            EventOutcome::default().with_app_request(1, Request::GetVisibleWindows),
            EventOutcome::default().with_make_key_window(1, WindowServerId::new(7)),
            EventOutcome::default().with_mouse_warp(CGPoint::default()),
            EventOutcome::default().with_close_window(None),
            EventOutcome::default().with_stdout_line("line".into()),
        ];
        for (index, mut outcome) in requests.into_iter().enumerate() {
            assert!(outcome.take_work_before_arrange().is_none(), "request {index}");
        }
    }

    /// Taking the work before the arrange leaves the arrange and what follows it where they were.
    #[test]
    fn taking_the_work_before_the_arrange_leaves_the_rest() {
        let (space, window) = (SpaceId::new(1), WindowId::new(1, 1));
        let mut outcome = EventOutcome::layout_changed(false)
            .with_layout_event(LayoutEvent::WindowAdded(space, window))
            .with_app_request(1, Request::GetVisibleWindows);

        let work = outcome.take_work_before_arrange().expect("a layout event is queued");

        assert_eq!(work.layout_events.len(), 1);
        assert!(!work.arrange.requested && work.app_requests.is_empty());
        assert!(outcome.layout_events.is_empty());
        assert!(outcome.arrange.requested);
        assert_eq!(outcome.app_requests.len(), 1);
    }

    /// A focus in the work taken before the arrange still lands: the regroup after the arrange
    /// reads it from what is left.
    #[test]
    fn a_focus_in_the_work_taken_before_the_arrange_still_lands() {
        let focus = LayoutEvent::WindowFocused(SpaceId::new(1), WindowId::new(1, 1));
        let mut outcome = EventOutcome::layout_changed(false).with_layout_event(focus);

        outcome.take_work_before_arrange().expect("a layout event is queued");

        assert!(outcome.layout_events.is_empty());
        assert!(outcome.focus_landed());
        let mut rest = EventOutcome::default();
        rest.absorb_ahead(outcome);
        assert!(rest.focus_landed());
    }

    #[test]
    fn work_taken_before_the_arrange_with_no_focus_lands_none() {
        let added = LayoutEvent::WindowAdded(SpaceId::new(1), WindowId::new(1, 1));
        let mut outcome = EventOutcome::layout_changed(false).with_layout_event(added);
        outcome.take_work_before_arrange().expect("a layout event is queued");
        assert!(!outcome.focus_landed());
    }

    // --- absorb: the arrange request -----------------------------------------------------------

    #[test]
    fn absorbing_an_arrange_request_asks_for_one() {
        let mut outer = EventOutcome::default();
        outer.absorb(EventOutcome::layout_changed(false));
        assert!(outer.arrange.requested);
        assert_eq!(outer.arrange.passes_to_run(false), Some(1));
    }

    /// Two arrange requests add their passes rather than taking the larger, because each was asked for
    /// by a workflow that changed something and wants its own pass.
    #[test]
    fn two_arrange_requests_add_their_passes() {
        let mut outer = EventOutcome::default().with_arrange_passes(2);
        outer.absorb(EventOutcome::default().with_arrange_passes(3));
        assert_eq!(outer.arrange.passes, 5);
    }

    /// A resize anywhere makes the combined pass a resize: the arrange has to be told, or it animates
    /// a resize as a move and the window stretches on the way.
    #[test]
    fn a_resize_on_either_side_makes_the_combined_pass_a_resize() {
        let mut outer = EventOutcome::layout_changed(false);
        outer.absorb(EventOutcome::layout_changed(true));
        assert!(outer.arrange.is_resize);
    }

    /// Two requests scoped to the SAME space keep the scope: a narrower pass is cheaper and the answer
    /// is the same.
    #[test]
    fn two_requests_for_one_space_keep_that_scope() {
        let space = SpaceId::new(7);
        let mut outer = EventOutcome::default()
            .with_arrange_passes(1)
            .with_arrange_space_scope(Some(space));
        let inner = EventOutcome::default()
            .with_arrange_passes(1)
            .with_arrange_space_scope(Some(space));

        outer.absorb(inner);

        assert_eq!(outer.arrange.space_scope, Some(space));
    }

    /// Two requests for DIFFERENT spaces widen to every space. Keeping either one would leave the other
    /// space unarranged, and arranging one space twice is cheaper than a window left in the wrong place.
    #[test]
    fn requests_for_two_different_spaces_widen_to_all_of_them() {
        let mut outer = EventOutcome::default()
            .with_arrange_passes(1)
            .with_arrange_space_scope(Some(SpaceId::new(1)));
        let inner = EventOutcome::default()
            .with_arrange_passes(1)
            .with_arrange_space_scope(Some(SpaceId::new(2)));

        outer.absorb(inner);

        assert_eq!(outer.arrange.space_scope, None, "None means every space");
    }

    /// An outcome that did not ask for an arrange contributes no scope, so absorbing one into a scoped
    /// request must not widen it to everything.
    #[test]
    fn absorbing_no_arrange_request_leaves_the_scope_alone() {
        let space = SpaceId::new(7);
        let mut outer = EventOutcome::default()
            .with_arrange_passes(1)
            .with_arrange_space_scope(Some(space));

        outer.absorb(EventOutcome::default());

        assert_eq!(outer.arrange.space_scope, Some(space));
        assert_eq!(outer.arrange.passes, 1, "and adds no passes");
    }
}
