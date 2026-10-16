use rini_core::ids::{SpaceId, WindowId, WindowServerId};
use rustc_hash::FxHashSet as HashSet;
use tracing::{debug, trace};

use crate::app::controller::WmEvent;
use crate::app::reactor::events::{EventOutcome, window};
use crate::app::reactor::managers::{DragManager, MissionControlManager};
use crate::app::reactor::state::RiniState;
use crate::app::reactor::{DragState, LayoutEvent, MissionControlState};
use crate::displays::domain::space_activation::SpaceActivationPolicy;
use crate::displays::domain::topology::SpaceEventKind;
use crate::windows::domain::info::AppInfo;
use crate::windows::domain::request::Request;

// spacewindowappeared/destroyed happen a lot when a display is connected/disconnected
// since they are literally when a window enters or leaves a space and each display has its own space(s)
// this is functionally a connection dropping to the window server
#[derive(Debug, Clone, Copy)]
pub struct WindowServerLifecyclePayload {
    pub window_server_id: WindowServerId,
    pub space: SpaceId,
    pub kind: SpaceEventKind,
}

#[derive(Debug)]
pub struct WindowServerDestroyedObservations {
    pub resolved_space: Option<SpaceId>,
    pub active_spaces: HashSet<SpaceId>,
    pub mission_control_active: bool,
    pub ordered_in: Option<bool>,
    pub assigned_space: Option<SpaceId>,
}

#[derive(Debug)]
pub struct WindowServerAppearedObservations {
    pub resolved_space: Option<SpaceId>,
    /// True when this window is one rini itself parked off-screen because its workspace is
    /// not the one its display is showing.
    pub is_parked_by_rini: bool,
    pub active_spaces: HashSet<SpaceId>,
    pub mission_control_active: bool,
    pub assigned_space: Option<SpaceId>,
    pub last_known_user_space: Option<SpaceId>,
    pub window_server_info: Option<crate::windows::domain::info::WindowServerInfo>,
    pub app_known: bool,
    pub running_app_info: Option<AppInfo>,
}

pub fn handle_window_server_destroyed(
    state: &mut RiniState,
    transactions: &crate::windows::domain::transaction::WindowTxStore,
    drag: &mut DragManager,
    payload: WindowServerLifecyclePayload,
    observations: WindowServerDestroyedObservations,
) -> anyhow::Result<EventOutcome> {
    let WindowServerLifecyclePayload {
        window_server_id: wsid,
        space: sid,
        kind,
    } = payload;
    let mut outcome = EventOutcome::default();
    match kind {
        SpaceEventKind::Fullscreen => {
            // Leaving a fullscreen space is not entering one: macOS can report it after the window
            // is already back on its user space. The window may be closing, so its app is asked.
            if let Some(wid) = state.windows.tracked_window_id(wsid) {
                outcome = outcome.with_app_request(wid.pid, Request::WindowMaybeDestroyed(wid));
            }
        }
        SpaceEventKind::User => {
            if let Some(current_space) = observations.resolved_space.filter(|&space| space != sid) {
                outcome = reassign_to_resolved_space(
                    state,
                    wsid,
                    current_space,
                    &observations.active_spaces,
                    observations.mission_control_active,
                    outcome,
                );
                debug!(
                    ?wsid,
                    reported_space = ?sid,
                    resolved_space = ?current_space,
                    "Resolved user-space disappearance to newer native membership"
                );
            } else {
                outcome = user_space_disappearance(
                    state,
                    transactions,
                    drag,
                    payload,
                    &observations,
                    outcome,
                );
            }
        }
    }
    Ok(outcome)
}

/// The window server's window moved to the space native membership resolves it to, visible as that
/// space is active, and a tracked window reassigned there.
fn reassign_to_resolved_space(
    state: &mut RiniState,
    wsid: WindowServerId,
    resolved_space: SpaceId,
    active_spaces: &HashSet<SpaceId>,
    mission_control_active: bool,
    outcome: EventOutcome,
) -> EventOutcome {
    state.windows.set_window_server_space(wsid, Some(resolved_space));
    if active_spaces.contains(&resolved_space) {
        state.windows.mark_window_visible(wsid);
    } else {
        state.windows.mark_window_hidden(wsid);
    }
    match state.windows.tracked_window_id(wsid) {
        Some(wid) => outcome
            .with_topology_reassignment(wid, resolved_space)
            .with_arrange_passes((!mission_control_active) as u8),
        None => outcome,
    }
}

/// A window leaving the user space it is reported on: destroyed at once when the window server
/// says it is ordered out, otherwise hidden, out of the layout of the space it was assigned, and
/// its app asked whether it is gone.
fn user_space_disappearance(
    state: &mut RiniState,
    transactions: &crate::windows::domain::transaction::WindowTxStore,
    drag: &mut DragManager,
    payload: WindowServerLifecyclePayload,
    observations: &WindowServerDestroyedObservations,
    mut outcome: EventOutcome,
) -> EventOutcome {
    let WindowServerLifecyclePayload {
        window_server_id: wsid,
        space: sid,
        ..
    } = payload;
    let Some(wid) = state.windows.tracked_window_id(wsid) else {
        state.windows.set_window_server_space(wsid, Some(sid));
        state.windows.mark_window_hidden(wsid);
        debug!(
            ?wsid,
            "Received WindowServerDestroyed for unknown window - ignoring"
        );
        return outcome;
    };
    if matches!(observations.ordered_in, Some(false)) {
        // since the connection has dropped it wont be shown in space_windows_list
        // so ordered in can be authorative because it doesnt consider
        // ghost windows that sometimes remain
        debug!(
            ?wid,
            ?wsid,
            reported_space = ?sid,
            "Promoting WindowServer disappearance to immediate WindowDestroyed"
        );
        if let Ok(destroyed_outcome) = window::handle_window_destroyed(
            state,
            transactions,
            drag,
            window::WindowDestroyedPayload { window: wid },
        ) {
            outcome.absorb(destroyed_outcome);
        }
        return outcome;
    }

    state.windows.set_window_server_space(wsid, Some(sid));
    state.windows.mark_window_hidden(wsid);
    let layout_changed = observations.assigned_space == Some(sid);
    if layout_changed {
        outcome = outcome.with_layout_event(LayoutEvent::WindowRemovedPreserveFloating(wid));
    }
    if layout_changed && !observations.mission_control_active {
        outcome = outcome.with_arrange_passes(1);
    }
    outcome.with_app_request(wid.pid, Request::WindowMaybeDestroyed(wid))
}

pub fn handle_window_server_appeared(
    state: &mut RiniState,
    payload: WindowServerLifecyclePayload,
    observations: WindowServerAppearedObservations,
) -> anyhow::Result<EventOutcome> {
    let WindowServerLifecyclePayload {
        window_server_id: wsid, kind, ..
    } = payload;
    let mut outcome = EventOutcome::default();
    if matches!(kind, SpaceEventKind::User)
        && let Some(resolved_space) = observations.resolved_space
    {
        if let Some(settled) = user_space_appearance(state, payload, resolved_space, &observations)
        {
            return Ok(settled);
        }
        outcome = outcome.with_confirmed_window_space(wsid, resolved_space);
    }

    if state.windows.knows_window_server_id(wsid) || state.windows.is_window_server_observed(wsid) {
        if !observations.mission_control_active {
            outcome = known_window_appeared(state, payload, &observations, outcome);
        }
        debug!(
            ?wsid,
            "Received WindowServerAppeared for known window - ignoring"
        );
        return Ok(outcome);
    }

    state.windows.mark_window_server_observed(wsid);
    // A window can arrive before its application is taken on: src/windows/docs/launch-notifications.md.
    if let Some(window_server_info) = observations.window_server_info {
        outcome = new_window_appeared(state, payload, window_server_info, observations, outcome);
    }
    Ok(outcome)
}

/// A window appearing on the user space native membership resolves it to: the outcome when that
/// settles it, a window rini parked or one on another space; `None` once the window is confirmed
/// on the space it is reported on.
fn user_space_appearance(
    state: &mut RiniState,
    payload: WindowServerLifecyclePayload,
    resolved_space: SpaceId,
    observations: &WindowServerAppearedObservations,
) -> Option<EventOutcome> {
    let WindowServerLifecyclePayload {
        window_server_id: wsid,
        space: sid,
        ..
    } = payload;
    let assigned_space = observations.assigned_space;
    // Never infer a display change for a window rini parked itself.
    //
    // Windows whose workspace is not the one their display is showing are moved
    // off-screen, and macOS will not keep a window entirely outside every display —
    // so those coordinates land inside the NEIGHBOURING display. WindowServer then
    // reports the window as belonging to that display, rini believed it, and the
    // window changed display. The next workspace switch parked it off the new
    // display, and so on: a feedback loop that walked every window onto one display.
    //
    // Measured from the logs, the loop is exactly:
    //   cmd=SwitchToWorkspace(1)
    //   -> WindowServerAppeared(wsid, SpaceId(980), User)
    //   -> reassigned to 980
    //
    // A parked window reassigned across displays MUST stay on its assigned space, or
    // the discovery cycle walks it onto one display. This mirrors
    // `keep_assigned_for_scrolling` on the WindowFrameChanged path, which exists for
    // the same reason.
    if observations.is_parked_by_rini && Some(resolved_space) != assigned_space {
        debug!(
            ?wsid,
            reported_space = ?sid,
            ?resolved_space,
            ?assigned_space,
            "Ignoring appearance for a window rini parked off-screen; its position is \
             not evidence of a display change"
        );
        return Some(EventOutcome::default());
    }
    if resolved_space != sid {
        let outcome = reassign_to_resolved_space(
            state,
            wsid,
            resolved_space,
            &observations.active_spaces,
            observations.mission_control_active,
            EventOutcome::default(),
        );
        debug!(
            ?wsid,
            reported_space = ?sid,
            resolved_space = ?resolved_space,
            "Resolved user-space appearance to stronger native membership"
        );
        return Some(outcome);
    }

    state.windows.set_window_server_space(wsid, Some(resolved_space));
    state.windows.mark_window_visible(wsid);
    None
}

/// A window rini already knows appearing: on a user space, a fullscreen window coming back or one
/// whose app is asked for its windows; on a fullscreen space, recorded there and out of the strip.
fn known_window_appeared(
    state: &mut RiniState,
    payload: WindowServerLifecyclePayload,
    observations: &WindowServerAppearedObservations,
    mut outcome: EventOutcome,
) -> EventOutcome {
    let WindowServerLifecyclePayload {
        window_server_id: wsid,
        space: sid,
        kind,
    } = payload;
    match kind {
        SpaceEventKind::User => {
            if let Some(wid) = state.windows.tracked_window_id(wsid) {
                outcome =
                    outcome.with_fullscreen_restoration(wsid, sid, wid).with_arrange_passes(1);
            } else if let Some(pid) =
                state.windows.pending_native_fullscreen_pid_for_window_server_id(wsid)
            {
                outcome = outcome.with_app_request(pid, Request::GetVisibleWindows);
            }
        }
        SpaceEventKind::Fullscreen => {
            let last_known_user_space = observations.last_known_user_space;
            let mut layout_changed = false;
            let tracked_window_id = state.windows.tracked_window_id(wsid);
            let owner_pid = tracked_window_id
                .map(|wid| wid.pid)
                .or_else(|| state.windows.get_window_server_info(wsid).map(|info| info.pid));
            record_fullscreen_window(
                state,
                sid,
                owner_pid,
                tracked_window_id,
                Some(wsid),
                last_known_user_space,
            );
            if tracked_window_id.is_none()
                && let Some(pid) = owner_pid
            {
                outcome = outcome.with_app_request(pid, Request::GetVisibleWindows);
            }
            if let Some((wid, from_space)) = fullscreen_departure(
                tracked_window_id,
                observations.assigned_space,
                last_known_user_space,
            ) {
                outcome =
                    outcome.with_layout_event(LayoutEvent::WindowRemovedPreserveFloating(wid));
                layout_changed =
                    from_space.is_some_and(|space| observations.active_spaces.contains(&space));
            }
            if layout_changed {
                outcome = outcome.with_arrange_passes(1);
            }
        }
    }
    outcome
}

/// A window the window server reports before rini knows it: a normal window big enough to manage
/// is recorded as fullscreen on a fullscreen space and otherwise taken in, and its app launched or
/// asked for its windows.
fn new_window_appeared(
    state: &mut RiniState,
    payload: WindowServerLifecyclePayload,
    window_server_info: crate::windows::domain::info::WindowServerInfo,
    observations: WindowServerAppearedObservations,
    outcome: EventOutcome,
) -> EventOutcome {
    let WindowServerLifecyclePayload {
        window_server_id: wsid,
        space: sid,
        kind,
    } = payload;
    if window_server_info.layer != 0 {
        trace!(
            ?wsid,
            layer = window_server_info.layer,
            "Ignoring non-normal window"
        );
        return outcome;
    }

    // Filter out very small windows (likely tooltips or similar UI elements)
    // that shouldn't be managed by the window manager
    const MIN_MANAGEABLE_WINDOW_SIZE: f64 = 50.0;
    if window_server_info.frame.size.width < MIN_MANAGEABLE_WINDOW_SIZE
        || window_server_info.frame.size.height < MIN_MANAGEABLE_WINDOW_SIZE
    {
        trace!(
            ?wsid,
            "Ignoring tiny window ({}x{}) - likely tooltip",
            window_server_info.frame.size.width,
            window_server_info.frame.size.height
        );
        return outcome;
    }

    if matches!(kind, SpaceEventKind::Fullscreen) {
        let window_id = state.windows.tracked_window_id(wsid);
        record_fullscreen_window(
            state,
            sid,
            Some(window_server_info.pid),
            window_id,
            Some(wsid),
            observations.last_known_user_space,
        );
        return outcome.with_app_request(window_server_info.pid, Request::GetVisibleWindows);
    }

    let outcome = outcome.with_window_server_updates(vec![window_server_info]);
    if observations.app_known {
        return outcome.with_app_request(window_server_info.pid, Request::GetVisibleWindows);
    }
    match observations.running_app_info {
        Some(app_info) => {
            outcome.with_wm_event(WmEvent::AppLaunch(window_server_info.pid, app_info))
        }
        None => outcome,
    }
}

pub fn handle_mission_control_native_entered(
    mission_control: &mut MissionControlManager,
    drag: &mut DragManager,
) -> anyhow::Result<EventOutcome> {
    drag.reset();
    drag.drag_state = DragState::Inactive;
    drag.skip_layout_for_window = None;
    let changed = !matches!(
        mission_control.mission_control_state,
        MissionControlState::Active
    );
    mission_control.mission_control_state = MissionControlState::Active;
    let outcome = EventOutcome::focus_changed(None, false);
    Ok(if changed {
        outcome.with_focus_follows_mouse_refresh()
    } else {
        outcome
    })
}

pub fn handle_mission_control_native_exited(
    mission_control: &mut MissionControlManager,
) -> anyhow::Result<EventOutcome> {
    let changed = matches!(
        mission_control.mission_control_state,
        MissionControlState::Active
    );
    mission_control.mission_control_state = MissionControlState::Inactive;
    let outcome = EventOutcome::layout_changed(false).with_mission_control_recovery();
    Ok(if changed {
        outcome.with_focus_follows_mouse_refresh()
    } else {
        outcome
    })
}

#[derive(Debug, Clone, Copy)]
pub struct SpaceLifecyclePayload {
    pub space: SpaceId,
    pub created: bool,
}

pub fn handle_space_lifecycle(
    policy: &mut SpaceActivationPolicy,
    payload: SpaceLifecyclePayload,
) -> anyhow::Result<EventOutcome> {
    if payload.created {
        policy.on_space_created(payload.space);
    } else {
        policy.on_space_destroyed(payload.space);
    }
    Ok(EventOutcome::layout_changed(false).with_active_space_recompute())
}
/// Whether a window macOS has taken fullscreen leaves the strip, and from which space.
///
/// It always leaves, for any window rini tracks: a fullscreen window sits on a space of its own and
/// is not managed while it is there, so a column reserved for it is a column reserved for a window
/// that is not on screen.
///
/// rini's own assignment need not AGREE with the last user space it has seen, and no user space need
/// have been observed at all: requiring either keeps the window in the strip and a gap in the layout
/// for it. `from_space` is only for deciding whether the layout of a VISIBLE space changed and needs
/// a pass; it is not a condition on leaving.
pub(crate) fn fullscreen_departure(
    tracked_window: Option<WindowId>,
    assigned_space: Option<SpaceId>,
    last_known_user_space: Option<SpaceId>,
) -> Option<(WindowId, Option<SpaceId>)> {
    let window = tracked_window?;
    Some((window, assigned_space.or(last_known_user_space)))
}

pub(crate) fn resolve_last_known_user_space(
    window_space: Option<SpaceId>,
    fallback_space: Option<SpaceId>,
) -> Option<SpaceId> {
    window_space.or(fallback_space)
}

fn record_fullscreen_window(
    state: &mut RiniState,
    sid: SpaceId,
    pid: Option<i32>,
    window_id: Option<WindowId>,
    window_server_id: Option<WindowServerId>,
    last_known_user_space: Option<SpaceId>,
) {
    let resolved_window_id = window_id
        .or_else(|| window_server_id.and_then(|wsid| state.windows.tracked_window_id(wsid)));
    if let Some(window_id) = resolved_window_id {
        let _ = state.windows.suspend_window_to_native_fullscreen(
            window_id,
            window_server_id,
            last_known_user_space,
            sid,
        );
    } else if let (Some(pid), Some(wsid)) = (pid, window_server_id) {
        let _ = state.windows.suspend_window_server_to_native_fullscreen(
            pid,
            wsid,
            last_known_user_space,
            sid,
        );
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::*;

    /// rini's assignment and the last user space it saw disagree. The window still leaves the strip,
    /// because macOS having the window is not a claim rini's bookkeeping gets to veto.
    #[test]
    fn a_fullscreen_window_leaves_the_strip_even_when_the_spaces_disagree() {
        let window = WindowId::new(7, 1);
        let assigned = SpaceId::new(3);
        let last_seen = SpaceId::new(9);
        assert_eq!(
            fullscreen_departure(Some(window), Some(assigned), Some(last_seen)),
            Some((window, Some(assigned)))
        );
    }

    #[test]
    fn a_fullscreen_window_leaves_the_strip_with_no_user_space_ever_observed() {
        let window = WindowId::new(7, 1);
        assert_eq!(
            fullscreen_departure(Some(window), None, None),
            Some((window, None))
        );
    }

    #[test]
    fn the_space_reported_is_the_assignment_first_then_the_last_seen_one() {
        let window = WindowId::new(7, 1);
        assert_eq!(
            fullscreen_departure(Some(window), None, Some(SpaceId::new(9))),
            Some((window, Some(SpaceId::new(9)))),
            "with no assignment, the last space it was seen on is what may need a layout pass"
        );
    }

    /// A window rini does not track has nothing to remove from the strip.
    #[test]
    fn an_untracked_fullscreen_window_departs_nothing() {
        assert_eq!(
            fullscreen_departure(None, Some(SpaceId::new(3)), Some(SpaceId::new(3))),
            None
        );
    }

    fn appearing(layer: i32, size: f64) -> crate::windows::domain::info::WindowServerInfo {
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
        crate::windows::domain::info::WindowServerInfo {
            id: WindowServerId::new(40),
            pid: 7,
            layer,
            frame: CGRect::new(CGPoint::new(0., 0.), CGSize::new(size, size)),
            min_frame: CGSize::new(0., 0.),
            max_frame: CGSize::new(0., 0.),
        }
    }

    fn observations(
        info: crate::windows::domain::info::WindowServerInfo,
        app_known: bool,
        running_app_info: Option<AppInfo>,
    ) -> WindowServerAppearedObservations {
        WindowServerAppearedObservations {
            resolved_space: None,
            is_parked_by_rini: false,
            active_spaces: HashSet::default(),
            mission_control_active: false,
            assigned_space: None,
            last_known_user_space: None,
            window_server_info: Some(info),
            app_known,
            running_app_info,
        }
    }

    fn appeared(
        info: crate::windows::domain::info::WindowServerInfo,
        kind: SpaceEventKind,
        app_known: bool,
        running_app_info: Option<AppInfo>,
    ) -> EventOutcome {
        let payload = WindowServerLifecyclePayload {
            window_server_id: info.id,
            space: SpaceId::new(1),
            kind,
        };
        handle_window_server_appeared(
            &mut RiniState::default(),
            payload,
            observations(info, app_known, running_app_info),
        )
        .unwrap()
    }

    /// A window the window server shows before rini knows it is taken in when it is a normal
    /// window of a manageable size, and nothing is asked of a layer above it or a tooltip.
    #[test]
    fn a_new_window_is_taken_in_only_on_the_normal_layer_and_at_a_manageable_size() {
        let taken = appeared(appearing(0, 400.), SpaceEventKind::User, true, None);
        assert_eq!(taken.window_server_updates.len(), 1);
        assert!(matches!(taken.app_requests[..], [(
            7,
            Request::GetVisibleWindows
        )]));

        for ignored in [appearing(3, 400.), appearing(0, 49.)] {
            let outcome = appeared(ignored, SpaceEventKind::User, true, None);
            assert!(outcome.window_server_updates.is_empty());
            assert!(outcome.app_requests.is_empty());
        }
    }

    /// An app not yet known is launched when it is running, and asked for nothing either way; a
    /// new window on a fullscreen space has its app asked for its windows and is not taken in.
    #[test]
    fn a_new_windows_app_is_launched_when_unknown_and_a_fullscreen_one_is_asked() {
        let app = AppInfo {
            bundle_id: Some("com.example".into()),
            localized_name: None,
        };
        let launched = appeared(appearing(0, 400.), SpaceEventKind::User, false, Some(app));
        assert!(matches!(launched.wm_events[..], [WmEvent::AppLaunch(7, _)]));
        assert!(launched.app_requests.is_empty());

        let unknown = appeared(appearing(0, 400.), SpaceEventKind::User, false, None);
        assert!(unknown.wm_events.is_empty());
        assert!(unknown.app_requests.is_empty());
        assert_eq!(unknown.window_server_updates.len(), 1);

        let fullscreen = appeared(appearing(0, 400.), SpaceEventKind::Fullscreen, true, None);
        assert!(fullscreen.window_server_updates.is_empty());
        assert!(matches!(fullscreen.app_requests[..], [(
            7,
            Request::GetVisibleWindows
        )]));
    }

    #[test]
    fn last_known_user_space_prefers_window_observation() {
        let observed = SpaceId::new(2);
        let fallback = SpaceId::new(1);
        assert_eq!(
            resolve_last_known_user_space(Some(observed), Some(fallback)),
            Some(observed)
        );
        assert_eq!(
            resolve_last_known_user_space(None, Some(fallback)),
            Some(fallback)
        );
    }
}
