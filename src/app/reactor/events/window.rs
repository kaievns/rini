use objc2_core_foundation::CGRect;
use rini_core::ids::{SpaceId, WindowId, WindowServerId};
use rini_geometry::SameAs;
use tracing::{debug, trace};

use crate::app::reactor::events::EventOutcome;
use crate::app::reactor::managers::DragManager;
use crate::app::reactor::{DragState, Quiet};
use crate::app::system_reads::server_facts;
use crate::layout::domain::edge_drag::Drag;
use crate::windows::domain::info::{WindowInfo as Window, WindowServerInfo};
use crate::windows::domain::manageable::{Facts, manageable};
use crate::windows::domain::state::{WindowFilter, WindowState};
use crate::windows::domain::transaction::{TransactionId, WindowTxStore};
use crate::windows::domain::zoom::Zoom;
use crate::windows::platform::mouse::MouseState;
use crate::windows::platform::window_server::WindowServer;
use crate::workspaces::LayoutEvent;

#[derive(Debug)]
pub struct WindowCreatedPayload {
    pub window_id: WindowId,
    pub window: Window,
    pub window_server_info: Option<WindowServerInfo>,
}

pub fn handle_window_created(
    state: &mut crate::app::reactor::state::RiniState,
    transactions: &WindowTxStore,
    window_server: &dyn WindowServer,
    payload: WindowCreatedPayload,
) -> anyhow::Result<EventOutcome> {
    let WindowCreatedPayload {
        window_id: wid,
        window,
        window_server_info: ws_info,
    } = payload;
    if let Some(wsid) = window.sys_id {
        state.windows.track_window_server_id(wsid, wid);
        state.windows.clear_window_server_observed(wsid);
    }
    if let Some(info) = ws_info {
        state.windows.clear_window_server_observed(info.id);
        state.windows.track_window_server_info(info);
    }

    let mut window_state: WindowState = window.into();
    let is_manageable = manageable(server_facts(
        window_server,
        Facts::of(&window_state.info),
        window_state.info.sys_id,
        |wsid| state.windows.get_window_server_info(wsid),
    ));
    window_state.is_manageable = is_manageable;
    if let Some(wsid) = window_state.info.sys_id {
        transactions.insert(wsid, transactions.last_txid(&wsid), window_state.frame_monotonic);
    }

    state.windows.insert_window(wid, window_state);

    let outcome = EventOutcome::window_membership_changed(false, true);
    Ok(if is_manageable {
        outcome.with_created_window_finalization(wid)
    } else {
        outcome
    })
}

#[derive(Debug, Clone, Copy)]
pub struct WindowDestroyedPayload {
    pub window: WindowId,
}

pub fn handle_window_destroyed(
    state: &mut crate::app::reactor::state::RiniState,
    transactions: &WindowTxStore,
    drag: &mut DragManager,
    payload: WindowDestroyedPayload,
) -> anyhow::Result<EventOutcome> {
    let wid = payload.window;
    let window_server_id = match state.windows.record(wid) {
        Some(record) => record.window_server_id(),
        None => return Ok(EventOutcome::no_change()),
    };

    if let Some(ws_id) = window_server_id {
        transactions.remove(&ws_id);
        state.windows.remove_window_server_state(ws_id);
    } else {
        debug!(?wid, "Received WindowDestroyed for unknown window - ignoring");
    }
    state.windows.remove_window(wid);

    if let DragState::PendingSwap { session, target } = &drag.drag_state
        && (session.window == wid || *target == wid)
    {
        trace!(
            ?wid,
            "Clearing pending drag swap because a participant window was destroyed"
        );
        drag.drag_state = DragState::Inactive;
    }

    let dragged_window = drag.dragged();
    let last_target = drag.last_target();
    if dragged_window == Some(wid) || last_target == Some(wid) {
        drag.reset();
        if dragged_window == Some(wid) {
            drag.drag_state = DragState::Inactive;
        }
    }

    if drag.skip_layout_for_window == Some(wid) {
        drag.skip_layout_for_window = None;
    }
    Ok(EventOutcome::window_membership_changed(true, false)
        .with_forgotten_window(wid)
        .with_layout_event(LayoutEvent::WindowRemoved(wid)))
}

pub fn handle_window_minimized(
    state: &mut crate::app::reactor::state::RiniState,
    wid: WindowId,
) -> anyhow::Result<crate::app::reactor::events::EventOutcome> {
    let server_id = if let Some(window) = state.windows.window_mut(wid) {
        if window.info.is_minimized {
            return Ok(crate::app::reactor::events::EventOutcome::no_change());
        }
        window.info.is_minimized = true;
        window.is_manageable = false;
        window.info.sys_id
    } else {
        debug!(?wid, "Received WindowMinimized for unknown window - ignoring");
        return Ok(crate::app::reactor::events::EventOutcome::no_change());
    };
    if let Some(ws_id) = server_id {
        state.windows.mark_window_hidden(ws_id);
    }
    Ok(
        crate::app::reactor::events::EventOutcome::window_membership_changed(false, false)
            .with_layout_event(LayoutEvent::WindowRemoved(wid)),
    )
}

#[derive(Debug, Clone, Copy)]
pub struct WindowDeminiaturizedPayload {
    pub window: WindowId,
    pub active_space: Option<SpaceId>,
}

pub fn handle_window_deminiaturized(
    state: &mut crate::app::reactor::state::RiniState,
    window_server: &dyn WindowServer,
    payload: WindowDeminiaturizedPayload,
) -> anyhow::Result<crate::app::reactor::events::EventOutcome> {
    let WindowDeminiaturizedPayload { window: wid, active_space } = payload;
    let (server_id, ax) = match state.windows.window_mut(wid) {
        Some(window) => {
            if !window.info.is_minimized {
                return Ok(crate::app::reactor::events::EventOutcome::no_change());
            }
            window.info.is_minimized = false;
            (window.info.sys_id, Facts::of(&window.info))
        }
        None => {
            debug!(
                ?wid,
                "Received WindowDeminiaturized for unknown window - ignoring"
            );
            return Ok(crate::app::reactor::events::EventOutcome::no_change());
        }
    };
    let is_manageable = manageable(server_facts(window_server, ax, server_id, |wsid| {
        state.windows.get_window_server_info(wsid)
    }));
    if let Some(window) = state.windows.window_mut(wid) {
        window.is_manageable = is_manageable;
    }

    let mut outcome = crate::app::reactor::events::EventOutcome::no_change();
    if is_manageable && let Some(space) = active_space {
        outcome =
            crate::app::reactor::events::EventOutcome::window_membership_changed(false, false)
                .with_layout_event(LayoutEvent::WindowAdded(space, wid));
    }
    Ok(outcome)
}

#[derive(Debug)]
pub struct WindowFrameChangedPayload {
    pub window: WindowId,
    pub new_frame: CGRect,
    pub mouse_state: Option<MouseState>,
    pub old_space: Option<SpaceId>,
    pub new_space: Option<SpaceId>,
    pub old_space_active: bool,
    pub new_space_active: bool,
    pub active_resize_space: Option<SpaceId>,
    pub pending_target_space: Option<SpaceId>,
    pub assigned_space: Option<SpaceId>,
    pub keep_assigned_for_scrolling: bool,
    pub screens: Vec<(SpaceId, CGRect, Option<String>)>,
    /// The report read as a title-bar zoom of a column on this strip.
    pub zoom: Option<(SpaceId, Zoom)>,
}

pub enum FrameChangeDisposition {
    Handled,
    NeedsGeometryAnalysis,
}

#[allow(clippy::too_many_arguments, reason = "P4.8")]
pub fn classify_window_frame_change(
    state: &mut crate::app::reactor::state::RiniState,
    transactions: &WindowTxStore,
    drag: &mut DragManager,
    wid: WindowId,
    new_frame: CGRect,
    last_seen: Option<TransactionId>,
    requested: bool,
    mouse_state: &mut Option<MouseState>,
    pointer: Option<MouseState>,
    mission_control_active: bool,
    zoom: bool,
) -> FrameChangeDisposition {
    let Some(window) = state.windows.window(wid) else {
        query_mouse_for_active_drag(drag, mouse_state, pointer);
        return FrameChangeDisposition::Handled;
    };
    let server_id = window.info.sys_id;

    if mission_control_active {
        drag.reset();
        drag.drag_state = DragState::Inactive;
        drag.skip_layout_for_window = None;
        return FrameChangeDisposition::Handled;
    }

    // A zoom answers the double-click, not rini's last write, whatever write the report follows.
    if zoom {
        if let Some(server) = server_id {
            transactions.clear_target(&server);
        }
        return FrameChangeDisposition::NeedsGeometryAnalysis;
    }

    if let Some(server) = server_id
        && let Some(target) = transactions.target(&server)
        && let Some(seen) = last_seen
    {
        if seen != transactions.last_txid(&server) {
            query_mouse_for_active_drag(drag, mouse_state, pointer);
            return FrameChangeDisposition::Handled;
        }
        if mouse_state.is_none() {
            *mouse_state = pointer;
        }
        if *mouse_state == Some(MouseState::Down) {
            transactions.clear_target(&server);
        } else {
            if new_frame.same_as(target) {
                transactions.clear_target(&server);
            }
            if let Some(window) = state.windows.window_mut(wid) {
                window.frame_monotonic = new_frame;
            }
            return FrameChangeDisposition::Handled;
        }
    }
    if requested {
        query_mouse_for_active_drag(drag, mouse_state, pointer);
        if let Some(window) = state.windows.window_mut(wid) {
            window.frame_monotonic = new_frame;
        }
        if let Some(server) = server_id {
            transactions.clear_target(&server);
        }
        return FrameChangeDisposition::Handled;
    }

    if mouse_state.is_none() {
        *mouse_state = pointer;
    }
    FrameChangeDisposition::NeedsGeometryAnalysis
}

fn query_mouse_for_active_drag(
    drag: &DragManager,
    mouse_state: &mut Option<MouseState>,
    pointer: Option<MouseState>,
) {
    if mouse_state.is_none()
        && matches!(
            drag.drag_state,
            DragState::Active { .. } | DragState::PendingSwap { .. }
        )
    {
        *mouse_state = pointer;
    }
}

pub fn handle_window_frame_changed(
    state: &mut crate::app::reactor::state::RiniState,
    layout: &mut crate::workspaces::LayoutEngine,
    drag: &mut DragManager,
    payload: WindowFrameChangedPayload,
) -> anyhow::Result<EventOutcome> {
    let wid = payload.window;
    let new_frame = payload.new_frame;
    let Some(window) = state.windows.window(wid) else {
        return Ok(EventOutcome::default());
    };
    let server_id = window.info.sys_id;
    let old_frame = window.frame_monotonic;
    let manageable = window.matches_filter(WindowFilter::EffectivelyManageable);

    if !payload.old_space_active && !payload.new_space_active {
        return Ok(EventOutcome::default());
    }
    if old_frame.same_as(new_frame) {
        return Ok(EventOutcome::default());
    }
    if let Some(window) = state.windows.window_mut(wid) {
        window.frame_monotonic = new_frame;
    }
    match payload.zoom {
        Some((space, Zoom::Toggle)) => {
            end_drag_of(drag, wid);
            return Ok(EventOutcome::layout_changed(false)
                .with_layout_event(LayoutEvent::WindowZoomed(space, wid)));
        }
        Some((_, Zoom::Rest)) => {
            end_drag_of(drag, wid);
            return Ok(EventOutcome::layout_changed(false));
        }
        None => {}
    }

    let mouse_state = payload.mouse_state;
    let dragging = mouse_state == Some(MouseState::Down)
        || matches!(
            drag.drag_state,
            DragState::Active { .. } | DragState::PendingSwap { .. }
        );
    let mut outcome = if dragging {
        dragged_frame_changed(drag, old_frame, payload)
    } else {
        drag.skip_layout_for_window = Some(wid);
        if payload.old_space != payload.new_space {
            if stays_where_it_is_assigned(&payload) {
                return Ok(EventOutcome::layout_changed(false));
            }
            moved_to_another_space(state, layout, server_id, manageable, &payload)
        } else if !old_frame.size.same_as(new_frame.size) && payload.old_space_active {
            let mut outcome = EventOutcome::layout_changed(false);
            outcome.arrange.is_resize = true;
            outcome.with_layout_event(LayoutEvent::WindowResized {
                wid,
                drag: None,
                new_frame,
                screens: payload.screens,
            })
        } else {
            EventOutcome::layout_changed(false)
        }
    };

    if handle_mouse_up_if_needed(drag, mouse_state) {
        outcome = outcome.with_mouse_up_dispatch();
    }
    Ok(outcome)
}

/// A frame report while the mouse is down or a drag is on: the drag session started or carried on,
/// a resize reported to the layout with the edge being dragged, a move evaluated for a swap.
fn dragged_frame_changed(
    drag: &mut DragManager,
    old_frame: CGRect,
    payload: WindowFrameChangedPayload,
) -> EventOutcome {
    let WindowFrameChangedPayload {
        window: wid,
        new_frame,
        old_space,
        new_space,
        active_resize_space,
        screens,
        ..
    } = payload;
    let mut outcome = EventOutcome::layout_changed(false);
    let (start_frame, dragged_end) = match &drag.drag_state {
        DragState::Active { session } | DragState::PendingSwap { session, .. }
            if session.window == wid =>
        {
            (session.start_frame, session.dragged_end)
        }
        _ => {
            drag.drag_state = DragState::Active {
                session: crate::app::reactor::DragSession {
                    window: wid,
                    start_frame: old_frame,
                    dragged_end: None,
                    last_frame: old_frame,
                    origin_space: old_space,
                    settled_space: old_space,
                    layout_dirty: false,
                },
            };
            (old_frame, None)
        }
    };
    if let DragState::Active { session } = &mut drag.drag_state {
        session.last_frame = new_frame;
        session.layout_dirty = true;
        if session.settled_space != new_space {
            session.settled_space = new_space;
        }
    }
    drag.skip_layout_for_window = Some(wid);
    if !old_frame.size.same_as(new_frame.size) {
        let report = Drag::report(start_frame, new_frame, dragged_end);
        if let DragState::Active { session } | DragState::PendingSwap { session, .. } =
            &mut drag.drag_state
        {
            session.dragged_end = report.end;
        }
        if active_resize_space.is_some() {
            outcome = outcome.with_layout_event(LayoutEvent::WindowResized {
                wid,
                drag: Some(report),
                new_frame,
                screens,
            });
        }
    } else {
        outcome = outcome.with_drag_swap_evaluation(wid, new_frame);
    }
    outcome
}

/// A window reported on another space that stays on the one it is assigned: it is on its way to a
/// pending target, or scrolled off its display by the strip.
fn stays_where_it_is_assigned(payload: &WindowFrameChangedPayload) -> bool {
    let pending = payload.pending_target_space;
    (pending.is_some() && payload.assigned_space == pending && payload.new_space != pending)
        || payload.keep_assigned_for_scrolling
}

/// A window the window server now reports on another space: out of its old space's layout, and
/// onto the new one's active workspace when that space is active and the window manageable.
fn moved_to_another_space(
    state: &mut crate::app::reactor::state::RiniState,
    layout: &mut crate::workspaces::LayoutEngine,
    server_id: Option<WindowServerId>,
    manageable: bool,
    payload: &WindowFrameChangedPayload,
) -> EventOutcome {
    let wid = payload.window;
    let mut outcome = EventOutcome::layout_changed(false)
        .with_layout_event(LayoutEvent::WindowRemovedPreserveFloating(wid));
    if let Some(space) = payload.new_space {
        if let Some(server) = server_id {
            state.windows.set_window_server_space(server, Some(space));
            state.windows.mark_window_visible(server);
        }
        // Only a manageable window joins the layout here. A window whose model frame
        // sat wholly off screen (a switch's departing row) maps to no space; when the
        // app then reports its real frame this reads as a space change, and Zoom's
        // 301x45 meeting toolbar was tiled as a column of the active workspace and the
        // strip scrolled to show it, pushing the focused window half off screen.
        if payload.new_space_active && manageable {
            if let Some(workspace) = layout.active_workspace(space) {
                let manager = layout.virtual_workspace_manager_mut();
                let windows = &mut state.windows;
                let _ = manager.assign_window_to_workspace(windows, space, wid, workspace);
            }
            outcome = outcome.with_layout_event(LayoutEvent::WindowAdded(space, wid));
        }
    } else if let Some(server) = server_id {
        state.windows.set_window_server_space(server, None);
    }
    outcome
}

#[derive(Debug)]
pub struct WindowTitleChangedPayload {
    pub window: WindowId,
    pub title: String,
}

pub fn handle_window_title_changed(
    state: &mut crate::app::reactor::state::RiniState,
    payload: WindowTitleChangedPayload,
) -> anyhow::Result<crate::app::reactor::events::EventOutcome> {
    let WindowTitleChangedPayload { window: wid, title: new_title } = payload;
    if let Some(window) = state.windows.window_mut(wid) {
        if window.info.title == new_title {
            return Ok(crate::app::reactor::events::EventOutcome::no_change());
        }
        window.info.title = new_title;
        return Ok(
            crate::app::reactor::events::EventOutcome::no_change().with_app_rule_reapply(wid)
        );
    }
    Ok(crate::app::reactor::events::EventOutcome::no_change())
}

#[derive(Debug, Clone, Copy)]
pub struct MouseMovedPayload {
    pub window: Option<WindowId>,
    pub should_sync: bool,
    pub is_main: bool,
    pub needs_layout_sync: bool,
    pub active_space: Option<SpaceId>,
}

pub fn handle_mouse_moved_over_window(
    apps: &rustc_hash::FxHashMap<rini_core::ids::pid_t, crate::app::reactor::AppState>,
    payload: MouseMovedPayload,
) -> anyhow::Result<crate::app::reactor::events::EventOutcome> {
    let Some(window) = payload.window else {
        return Ok(crate::app::reactor::events::EventOutcome::default());
    };
    if !payload.should_sync || (payload.is_main && !payload.needs_layout_sync) {
        return Ok(crate::app::reactor::events::EventOutcome::default());
    }

    let mut outcome = crate::app::reactor::events::EventOutcome::default();
    if !payload.is_main {
        let mut app_handles = rustc_hash::FxHashMap::default();
        if let Some(app) = apps.get(&window.pid) {
            app_handles.insert(window.pid, app.handle.clone());
        }
        outcome = outcome.with_raise_request(crate::windows::domain::raise::Event::RaiseRequest(
            crate::windows::domain::raise::RaiseRequest {
                raise_windows: vec![crate::windows::domain::raise_order::AppBatch::new(window)],
                focus_window: Some((window, None)),
                app_handles,
                focus_quiet: Quiet::No,
            },
        ));
    }
    if let Some(space) = payload.active_space {
        outcome = outcome.with_layout_event(LayoutEvent::WindowFocused(space, window));
    }
    Ok(outcome)
}

/// Ends `wid`'s drag, so the layout pass the zoom asks for runs and writes it.
fn end_drag_of(drag: &mut DragManager, wid: WindowId) {
    if matches!(
        &drag.drag_state,
        DragState::Active { session } | DragState::PendingSwap { session, .. }
            if session.window == wid
    ) {
        drag.reset();
        drag.drag_state = DragState::Inactive;
    }
    if drag.skip_layout_for_window == Some(wid) {
        drag.skip_layout_for_window = None;
    }
}

fn handle_mouse_up_if_needed(drag: &DragManager, mouse_state: Option<MouseState>) -> bool {
    if mouse_state == Some(MouseState::Up)
        && matches!(
            drag.drag_state,
            DragState::Active { .. } | DragState::PendingSwap { .. }
        )
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;
    use crate::app::reactor::DragState;
    use crate::app::reactor::managers::DragManager;
    use crate::app::reactor::state::RiniState;
    use crate::app::reactor::testing::make_window_info;
    use crate::windows::domain::state::WindowState;

    const WSID: u32 = 7;

    fn reported(
        assigned: Option<u64>,
        pending: Option<u64>,
        new_space: Option<u64>,
        scrolling: bool,
    ) -> WindowFrameChangedPayload {
        WindowFrameChangedPayload {
            window: WindowId::new(1, 1),
            new_frame: CGRect::ZERO,
            mouse_state: None,
            old_space: assigned.map(SpaceId::new),
            new_space: new_space.map(SpaceId::new),
            old_space_active: true,
            new_space_active: true,
            active_resize_space: None,
            pending_target_space: pending.map(SpaceId::new),
            assigned_space: assigned.map(SpaceId::new),
            keep_assigned_for_scrolling: scrolling,
            screens: Vec::new(),
            zoom: None,
        }
    }

    /// A window reported on another space stays where it is assigned while it is on its way to a
    /// pending target elsewhere or scrolled off its display by the strip, and moves otherwise.
    #[test]
    fn a_window_on_another_space_stays_assigned_only_on_its_way_or_scrolled_off() {
        assert!(stays_where_it_is_assigned(&reported(
            Some(2),
            Some(2),
            Some(3),
            false
        )));
        assert!(stays_where_it_is_assigned(&reported(
            Some(2),
            None,
            Some(3),
            true
        )));
        assert!(!stays_where_it_is_assigned(&reported(
            Some(2),
            None,
            Some(3),
            false
        )));
        assert!(
            !stays_where_it_is_assigned(&reported(Some(2), Some(3), Some(4), false)),
            "a pending target it is not assigned to does not hold it"
        );
        assert!(
            !stays_where_it_is_assigned(&reported(Some(2), Some(2), Some(2), false)),
            "reported on its pending target, it has arrived"
        );
    }

    fn rect(x: f64, y: f64) -> CGRect { CGRect::new(CGPoint::new(x, y), CGSize::new(100.0, 100.0)) }

    fn wid() -> WindowId { WindowId::new(1, 1) }

    fn state() -> RiniState {
        let mut state = RiniState::default();
        let info = make_window_info(rect(0.0, 0.0), Some(WindowServerId::new(WSID)), "w", None);
        state.windows.insert_window(wid(), WindowState::from(info));
        state.windows.track_window_server_id(WindowServerId::new(WSID), wid());
        state
    }

    fn drag() -> DragManager {
        DragManager {
            drag_state: DragState::Inactive,
            drag_swap_manager: crate::input::domain::drag_swap::DragManager::new(
                crate::input::settings::WindowSnappingSettings::default(),
            ),
            skip_layout_for_window: None,
            title_bar_click: None,
        }
    }

    #[allow(clippy::too_many_arguments, reason = "P4.8")]
    fn classify(
        state: &mut RiniState,
        transactions: &WindowTxStore,
        drag: &mut DragManager,
        new_frame: CGRect,
        last_seen: Option<TransactionId>,
        requested: bool,
        mouse: MouseState,
        mission_control: bool,
    ) -> FrameChangeDisposition {
        classify_zoom(
            state,
            transactions,
            drag,
            new_frame,
            last_seen,
            requested,
            mouse,
            mission_control,
            false,
        )
    }

    #[allow(clippy::too_many_arguments, reason = "P4.8")]
    fn classify_zoom(
        state: &mut RiniState,
        transactions: &WindowTxStore,
        drag: &mut DragManager,
        new_frame: CGRect,
        last_seen: Option<TransactionId>,
        requested: bool,
        mouse: MouseState,
        mission_control: bool,
        zoom: bool,
    ) -> FrameChangeDisposition {
        let mut mouse = Some(mouse);
        classify_window_frame_change(
            state,
            transactions,
            drag,
            wid(),
            new_frame,
            last_seen,
            requested,
            &mut mouse,
            None,
            mission_control,
            zoom,
        )
    }

    fn handled(d: &FrameChangeDisposition) -> bool { matches!(d, FrameChangeDisposition::Handled) }

    #[test]
    fn a_frame_report_for_an_unknown_window_is_nothing_to_analyse() {
        let mut empty = RiniState::default();
        let tx = WindowTxStore::new();
        let d = classify(
            &mut empty,
            &tx,
            &mut drag(),
            rect(5.0, 5.0),
            None,
            false,
            MouseState::Up,
            false,
        );
        assert!(handled(&d));
    }

    #[test]
    fn mission_control_ends_any_drag_and_swallows_the_report() {
        let mut s = state();
        let tx = WindowTxStore::new();
        let mut dr = drag();
        dr.skip_layout_for_window = Some(wid());
        let d = classify(
            &mut s,
            &tx,
            &mut dr,
            rect(5.0, 5.0),
            None,
            false,
            MouseState::Up,
            true,
        );
        assert!(handled(&d));
        assert!(matches!(dr.drag_state, DragState::Inactive));
        assert_eq!(
            dr.skip_layout_for_window, None,
            "Mission Control moves every window"
        );
    }

    // The window server replays frames. A report carrying an older transaction id is an echo of a
    // write rini has already superseded, and acting on it walks the window backwards.
    #[test]
    fn a_report_from_a_superseded_write_is_discarded_without_moving_the_window() {
        let mut s = state();
        let wsid = WindowServerId::new(WSID);
        let tx = WindowTxStore::new();
        // One record per window holds both the txid and the target, so the write rini is waiting for
        // is stored last and the report claims an earlier one.
        let stale = TransactionId::default().next();
        let current = stale.next();
        tx.insert(wsid, current, rect(50.0, 50.0));

        let d = classify(
            &mut s,
            &tx,
            &mut drag(),
            rect(9.0, 9.0),
            Some(stale),
            false,
            MouseState::Up,
            false,
        );
        assert!(handled(&d));
        assert_eq!(
            s.windows.window(wid()).unwrap().frame_monotonic,
            rect(0.0, 0.0),
            "an echo must not update the monotonic frame"
        );
    }

    #[test]
    fn a_report_acknowledging_rinis_own_write_lands_and_clears_the_target() {
        let mut s = state();
        let wsid = WindowServerId::new(WSID);
        let tx = WindowTxStore::new();
        let txid = TransactionId::default().next();
        tx.insert(wsid, txid, rect(50.0, 50.0));

        let d = classify(
            &mut s,
            &tx,
            &mut drag(),
            rect(50.0, 50.0),
            Some(txid),
            false,
            MouseState::Up,
            false,
        );
        assert!(handled(&d));
        assert_eq!(
            s.windows.window(wid()).unwrap().frame_monotonic,
            rect(50.0, 50.0)
        );
        assert_eq!(tx.target(&wsid), None, "the write is acknowledged");
    }

    // A held mouse means the user is dragging, so rini's pending target is abandoned rather than
    // waited for, and the move goes on to geometry analysis.
    #[test]
    fn a_held_mouse_abandons_rinis_pending_target_and_analyses_the_move() {
        let mut s = state();
        let wsid = WindowServerId::new(WSID);
        let tx = WindowTxStore::new();
        let txid = TransactionId::default().next();
        tx.insert(wsid, txid, rect(50.0, 50.0));

        let d = classify(
            &mut s,
            &tx,
            &mut drag(),
            rect(9.0, 9.0),
            Some(txid),
            false,
            MouseState::Down,
            false,
        );
        assert!(!handled(&d), "the user's drag is real geometry");
        assert_eq!(tx.target(&wsid), None, "rini stops waiting for its own write");
    }

    /// A report that carries no mouse state takes the pointer's, which the reducer is handed.
    #[test]
    fn a_report_with_no_mouse_state_takes_the_pointer_it_is_handed() {
        let mut s = state();
        let wsid = WindowServerId::new(WSID);
        let tx = WindowTxStore::new();
        let txid = TransactionId::default().next();
        tx.insert(wsid, txid, rect(50.0, 50.0));
        let mut reported = None;

        let d = classify_window_frame_change(
            &mut s,
            &tx,
            &mut drag(),
            wid(),
            rect(9.0, 9.0),
            Some(txid),
            false,
            &mut reported,
            Some(MouseState::Down),
            false,
            false,
        );
        assert!(!handled(&d), "the held pointer makes it the user's drag");
        assert_eq!(reported, Some(MouseState::Down));
        assert_eq!(tx.target(&wsid), None);
    }

    #[test]
    fn a_frame_rini_asked_for_lands_without_analysis() {
        let mut s = state();
        let tx = WindowTxStore::new();
        let d = classify(
            &mut s,
            &tx,
            &mut drag(),
            rect(12.0, 12.0),
            None,
            true,
            MouseState::Up,
            false,
        );
        assert!(handled(&d));
        assert_eq!(
            s.windows.window(wid()).unwrap().frame_monotonic,
            rect(12.0, 12.0)
        );
    }

    #[test]
    fn an_unprompted_move_needs_geometry_analysis() {
        let mut s = state();
        let tx = WindowTxStore::new();
        let d = classify(
            &mut s,
            &tx,
            &mut drag(),
            rect(300.0, 300.0),
            None,
            false,
            MouseState::Up,
            false,
        );
        assert!(!handled(&d));
        assert_eq!(
            s.windows.window(wid()).unwrap().frame_monotonic,
            rect(0.0, 0.0),
            "the frame is not accepted until the analysis decides what the move means"
        );
    }

    // rini's last write can still be pending when the app zooms: the report follows that write, so
    // it would otherwise read as the write landing a few points off and be swallowed.
    #[test]
    fn a_zoom_is_analysed_even_while_rini_waits_on_its_own_write() {
        let mut s = state();
        let wsid = WindowServerId::new(WSID);
        let tx = WindowTxStore::new();
        let txid = TransactionId::default().next();
        tx.insert(wsid, txid, rect(50.0, 50.0));

        let d = classify_zoom(
            &mut s,
            &tx,
            &mut drag(),
            rect(9.0, 9.0),
            Some(txid),
            false,
            MouseState::Up,
            false,
            true,
        );
        assert!(!handled(&d), "the zoom is the app's, not an echo");
        assert_eq!(tx.target(&wsid), None, "nothing is waited for any more");

        let d = classify_zoom(
            &mut s,
            &tx,
            &mut drag(),
            rect(9.0, 9.0),
            None,
            false,
            MouseState::Up,
            true,
            true,
        );
        assert!(handled(&d), "Mission Control still moves every window");
    }
}
