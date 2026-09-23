use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{SpaceId, WindowId, WindowServerId, pid_t};

use crate::app::config::Config;
use crate::app::hotkeys::WmEvent;
use crate::windows::domain::info::{AppInfo, WindowInfo, WindowServerInfo};
use crate::windows::domain::raise as raise_manager;
use crate::windows::domain::request::Request;
use crate::workspaces::{Direction, EventResponse, LayoutEvent};

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
    pub(crate) requested: bool,
}

#[derive(Debug)]
pub(crate) struct WindowTitleBroadcast {
    pub(crate) window: WindowId,
    pub(crate) previous_title: String,
    pub(crate) new_title: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TopologyReassignment {
    pub(crate) window: WindowId,
    pub(crate) space: SpaceId,
    pub(crate) preserve_workspace_ordinal: bool,
}

/// Follow-up work requested by an event workflow.
///
/// Workflows mutate reactor-owned domain state synchronously, then describe the
/// ordered integration work which must happen after the mutation.  Keeping the
/// description small and concrete makes it possible to test policy without
/// turning platform operations into a generic effect system.
#[derive(Debug, Default)]
pub(crate) struct EventOutcome {
    pub(crate) window_server_updates: Vec<WindowServerInfo>,
    pub(crate) discoveries: Vec<WindowDiscoveryRequest>,
    pub(crate) recompute_active_spaces: bool,
    pub(crate) repair_spaces_after_mission_control: bool,
    pub(crate) refresh_after_mission_control: bool,
    pub(crate) force_refresh_all_windows: bool,
    pub(crate) switch_native_space: Option<Direction>,
    pub(crate) wm_events: Vec<WmEvent>,
    pub(crate) app_requests: Vec<(pid_t, Request)>,
    pub(crate) topology_reassignments: Vec<TopologyReassignment>,
    /// Windows a workflow removed whose overlay picture the animation cache should drop.
    pub(crate) forgotten_windows: Vec<WindowId>,
    pub(crate) confirmed_window_spaces: Vec<(WindowServerId, SpaceId)>,
    pub(crate) fullscreen_restorations: Vec<(WindowServerId, SpaceId, WindowId)>,
    pub(crate) raise_requests: Vec<raise_manager::Event>,
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
    pub(crate) window_title_broadcasts: Vec<WindowTitleBroadcast>,
    pub(crate) focused_window_broadcast: Option<WindowId>,
    pub(crate) layout_events: Vec<LayoutEvent>,
    pub(crate) layout_responses: Vec<(EventResponse, Option<SpaceId>)>,
    pub(crate) arrange: ArrangeRequest,
    pub(crate) focused_window: Option<WindowId>,
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
    pub(crate) fn no_change() -> Self {
        Self::default()
    }

    /// Whether focus came to rest somewhere as a result of this event.
    ///
    /// Either the outcome names the window directly, or a layout event does. Both count: a click
    /// reports focus with no layout change at all, and macOS raised only the window that was clicked,
    /// so the strip still needs regrouping over whatever the click put in front of it.
    pub(crate) fn focus_landed(&self) -> bool {
        self.focused_window.is_some()
            || self
                .layout_events
                .iter()
                .any(|event| matches!(event, LayoutEvent::WindowFocused(..)))
    }

    /// Combines follow-up work produced by nested reducers while preserving
    /// reducer order for every queued operation.
    pub(crate) fn absorb(&mut self, mut other: Self) {
        self.window_server_updates.append(&mut other.window_server_updates);
        self.discoveries.append(&mut other.discoveries);
        self.recompute_active_spaces |= other.recompute_active_spaces;
        self.repair_spaces_after_mission_control |= other.repair_spaces_after_mission_control;
        self.refresh_after_mission_control |= other.refresh_after_mission_control;
        self.force_refresh_all_windows |= other.force_refresh_all_windows;
        self.switch_native_space = other.switch_native_space.or(self.switch_native_space);
        self.wm_events.append(&mut other.wm_events);
        self.app_requests.append(&mut other.app_requests);
        self.topology_reassignments.append(&mut other.topology_reassignments);
        self.forgotten_windows.append(&mut other.forgotten_windows);
        self.confirmed_window_spaces.append(&mut other.confirmed_window_spaces);
        self.fullscreen_restorations.append(&mut other.fullscreen_restorations);
        self.raise_requests.append(&mut other.raise_requests);
        self.make_key_windows.append(&mut other.make_key_windows);
        self.mouse_warps.append(&mut other.mouse_warps);
        self.pre_layout_window_frame_writes
            .append(&mut other.pre_layout_window_frame_writes);
        self.drag_swap_evaluations.append(&mut other.drag_swap_evaluations);
        self.dispatch_mouse_up |= other.dispatch_mouse_up;
        self.close_window = other.close_window.or(self.close_window);
        self.service_config_update =
            other.service_config_update.or(self.service_config_update.take());
        self.stdout_lines.append(&mut other.stdout_lines);
        self.reapply_app_rules.append(&mut other.reapply_app_rules);
        self.finalize_created_windows.append(&mut other.finalize_created_windows);
        self.window_title_broadcasts.append(&mut other.window_title_broadcasts);
        self.focused_window_broadcast =
            other.focused_window_broadcast.or(self.focused_window_broadcast);
        self.layout_events.append(&mut other.layout_events);
        self.layout_responses.append(&mut other.layout_responses);
        if other.arrange.requested {
            self.arrange.space_scope = if self.arrange.requested {
                match (self.arrange.space_scope, other.arrange.space_scope) {
                    (Some(existing), Some(other)) if existing == other => Some(existing),
                    _ => None,
                }
            } else {
                other.arrange.space_scope
            };
            self.arrange.requested = true;
            self.arrange.passes = self.arrange.passes.saturating_add(other.arrange.passes).max(1);
            self.arrange.is_resize |= other.arrange.is_resize;
            self.arrange.window_was_destroyed |= other.arrange.window_was_destroyed;
        }
        self.focused_window = other.focused_window.or(self.focused_window);
        self.refresh_window_notifications |= other.refresh_window_notifications;
        self.refresh_focus_follows_mouse |= other.refresh_focus_follows_mouse;
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
            switch_native_space: None,
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
            window_title_broadcasts: Vec::new(),
            focused_window_broadcast: None,
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
            refresh_window_notifications: false,
            refresh_focus_follows_mouse: false,
        }
    }

    /// A window entered, left, or changed its membership in the managed set.
    pub(crate) fn window_membership_changed(
        window_was_destroyed: bool,
        refresh_window_notifications: bool,
    ) -> Self {
        let mut outcome = Self::layout_changed(false);
        outcome.arrange.window_was_destroyed = window_was_destroyed;
        outcome.refresh_window_notifications = refresh_window_notifications;
        outcome
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

    pub(crate) fn with_native_space_switch(mut self, direction: Direction) -> Self {
        self.switch_native_space = Some(direction);
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

    pub(crate) fn with_topology_reassignment(
        mut self,
        window: WindowId,
        space: SpaceId,
        preserve_workspace_ordinal: bool,
    ) -> Self {
        self.topology_reassignments.push(TopologyReassignment {
            window,
            space,
            preserve_workspace_ordinal,
        });
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

    pub(crate) fn with_raise_request(mut self, request: raise_manager::Event) -> Self {
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
        requested: bool,
    ) -> Self {
        self.pre_layout_window_frame_writes.push(WindowFrameWriteRequest {
            window,
            frame,
            requested,
        });
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

    pub(crate) fn with_window_title_broadcast(
        mut self,
        window: WindowId,
        previous_title: String,
        new_title: String,
    ) -> Self {
        self.window_title_broadcasts.push(WindowTitleBroadcast {
            window,
            previous_title,
            new_title,
        });
        self
    }

    pub(crate) fn with_focused_window_broadcast(mut self, window: WindowId) -> Self {
        self.focused_window_broadcast = Some(window);
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

        let outcome = EventOutcome::window_membership_changed(true, true);
        assert!(outcome.arrange.requested);
        assert!(outcome.arrange.window_was_destroyed);
        assert!(outcome.refresh_window_notifications);

        let focused = WindowId::new(42, 7);
        let outcome = EventOutcome::focus_changed(Some(focused), false);
        assert!(!outcome.arrange.requested);
        assert_eq!(outcome.focused_window, Some(focused));

        let outcome = EventOutcome::no_change().with_focused_window_broadcast(focused);
        assert_eq!(outcome.focused_window_broadcast, Some(focused));
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

    fn arrange(requested: bool, passes: u8, window_was_destroyed: bool) -> ArrangeRequest {
        ArrangeRequest {
            requested,
            passes,
            window_was_destroyed,
            ..ArrangeRequest::default()
        }
    }

    #[test]
    fn an_arrange_that_was_not_asked_for_runs_no_passes() {
        assert_eq!(arrange(false, 3, false).passes_to_run(false), None);
    }

    /// A requested arrange always runs at least once. Several constructors set `requested` without
    /// setting `passes`, and taking that literally would ask for an arrange and then not do one.
    #[test]
    fn a_requested_arrange_with_no_pass_count_still_runs_once() {
        assert_eq!(arrange(true, 0, false).passes_to_run(false), Some(1));
    }

    #[test]
    fn a_pass_count_is_taken_as_given() {
        assert_eq!(arrange(true, 3, false).passes_to_run(false), Some(3));
    }

    /// A drag suppresses arranging: the user is holding the window and a pass would fight them.
    #[test]
    fn a_drag_suppresses_the_arrange() {
        assert_eq!(arrange(true, 2, false).passes_to_run(true), None);
    }

    /// Unless the window was destroyed mid-drag. The drag is over as far as the strip is concerned
    /// and the gap it left has to close.
    #[test]
    fn a_window_destroyed_mid_drag_arranges_anyway() {
        assert_eq!(arrange(true, 2, true).passes_to_run(true), Some(2));
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
}
