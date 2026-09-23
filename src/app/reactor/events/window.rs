use objc2_core_foundation::CGRect;
use tracing::{debug, trace};

use crate::app::reactor::events::EventOutcome;
use crate::app::reactor::managers::DragManager;
use crate::app::reactor::{DragState, Quiet};
use crate::windows::domain::info::WindowInfo as Window;
use crate::windows::domain::info::WindowServerInfo;
use crate::windows::domain::state::WindowFilter;
use crate::windows::domain::state::WindowState;
use crate::windows::domain::transaction::{TransactionId, TransactionManager};
use crate::windows::platform::mouse::MouseState;
use crate::windows::platform::window_server::compute_window_manageability;
use crate::workspaces::LayoutEvent;
use crate::workspaces::WindowVisibility;
use rini_core::ids::SpaceId;
use rini_core::ids::WindowId;
use rini_geometry::SameAs;

#[derive(Debug)]
pub struct WindowCreatedPayload {
    pub window_id: WindowId,
    pub window: Window,
    pub window_server_info: Option<WindowServerInfo>,
}

pub fn handle_window_created(
    state: &mut crate::app::reactor::state::RiniState,
    transactions: &TransactionManager,
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
    let is_manageable = compute_window_manageability(
        window_state.info.sys_id,
        window_state.info.is_minimized,
        window_state.info.is_standard,
        window_state.info.is_root,
        |wsid| state.windows.get_window_server_info(wsid),
    );
    window_state.is_manageable = is_manageable;
    if let Some(wsid) = window_state.info.sys_id {
        transactions.store_txid(
            wsid,
            transactions.get_last_sent_txid(wsid),
            window_state.frame_monotonic,
        );
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
    transactions: &TransactionManager,
    drag: &mut DragManager,
    payload: WindowDestroyedPayload,
) -> anyhow::Result<EventOutcome> {
    let wid = payload.window;
    let window_server_id = match state.windows.record(wid) {
        Some(record) => record.window_server_id(),
        None => return Ok(EventOutcome::no_change()),
    };

    if let Some(ws_id) = window_server_id {
        transactions.remove_for_window(ws_id);
        state.windows.remove_window_server_state(ws_id);
    } else {
        debug!(?wid, "Received WindowDestroyed for unknown window - ignoring");
    }
    state.windows.remove_window(wid);

    if let DragState::PendingSwap { session, target } = &drag.drag_state {
        if session.window == wid || *target == wid {
            trace!(
                ?wid,
                "Clearing pending drag swap because a participant window was destroyed"
            );
            drag.drag_state = DragState::Inactive;
        }
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
    state.windows.set_visibility(wid, WindowVisibility::Minimized);
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
    payload: WindowDeminiaturizedPayload,
) -> anyhow::Result<crate::app::reactor::events::EventOutcome> {
    let WindowDeminiaturizedPayload { window: wid, active_space } = payload;
    let (server_id, is_ax_standard, is_ax_root) = match state.windows.window_mut(wid) {
        Some(window) => {
            if !window.info.is_minimized {
                return Ok(crate::app::reactor::events::EventOutcome::no_change());
            }
            window.info.is_minimized = false;
            (window.info.sys_id, window.info.is_standard, window.info.is_root)
        }
        None => {
            debug!(
                ?wid,
                "Received WindowDeminiaturized for unknown window - ignoring"
            );
            return Ok(crate::app::reactor::events::EventOutcome::no_change());
        }
    };
    let is_manageable =
        compute_window_manageability(server_id, false, is_ax_standard, is_ax_root, |wsid| {
            state.windows.get_window_server_info(wsid)
        });
    if let Some(window) = state.windows.window_mut(wid) {
        window.is_manageable = is_manageable;
    }
    state.windows.set_visibility(wid, WindowVisibility::Visible);

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
}

pub enum FrameChangeDisposition {
    Handled,
    NeedsGeometryAnalysis,
}

pub fn classify_window_frame_change(
    state: &mut crate::app::reactor::state::RiniState,
    transactions: &TransactionManager,
    drag: &mut DragManager,
    wid: WindowId,
    new_frame: CGRect,
    last_seen: Option<TransactionId>,
    requested: bool,
    mouse_state: &mut Option<MouseState>,
    mission_control_active: bool,
) -> FrameChangeDisposition {
    let Some(window) = state.windows.window(wid) else {
        query_mouse_for_active_drag(drag, mouse_state);
        return FrameChangeDisposition::Handled;
    };
    let server_id = window.info.sys_id;

    if mission_control_active {
        drag.reset();
        drag.drag_state = DragState::Inactive;
        drag.skip_layout_for_window = None;
        return FrameChangeDisposition::Handled;
    }

    if let Some(server) = server_id
        && let Some(target) = transactions.get_target_frame(server)
        && let Some(seen) = last_seen
    {
        if seen != transactions.get_last_sent_txid(server) {
            query_mouse_for_active_drag(drag, mouse_state);
            return FrameChangeDisposition::Handled;
        }
        if mouse_state.is_none() {
            *mouse_state = crate::windows::platform::mouse::get_mouse_state();
        }
        if *mouse_state == Some(MouseState::Down) {
            transactions.clear_target_for_window(server);
        } else {
            if new_frame.same_as(target) {
                transactions.clear_target_for_window(server);
            }
            if let Some(window) = state.windows.window_mut(wid) {
                window.frame_monotonic = new_frame;
            }
            return FrameChangeDisposition::Handled;
        }
    }
    if requested {
        query_mouse_for_active_drag(drag, mouse_state);
        if let Some(window) = state.windows.window_mut(wid) {
            window.frame_monotonic = new_frame;
        }
        if let Some(server) = server_id {
            transactions.clear_target_for_window(server);
        }
        return FrameChangeDisposition::Handled;
    }

    if mouse_state.is_none() {
        *mouse_state = crate::windows::platform::mouse::get_mouse_state();
    }
    FrameChangeDisposition::NeedsGeometryAnalysis
}

fn query_mouse_for_active_drag(drag: &DragManager, mouse_state: &mut Option<MouseState>) {
    if mouse_state.is_none()
        && matches!(
            drag.drag_state,
            DragState::Active { .. } | DragState::PendingSwap { .. }
        )
    {
        *mouse_state = crate::windows::platform::mouse::get_mouse_state();
    }
}

pub fn handle_window_frame_changed(
    state: &mut crate::app::reactor::state::RiniState,
    layout: &mut crate::app::reactor::managers::LayoutManager,
    drag: &mut DragManager,
    payload: WindowFrameChangedPayload,
) -> anyhow::Result<EventOutcome> {
    let WindowFrameChangedPayload {
        window: wid,
        new_frame,
        mouse_state,
        old_space,
        new_space,
        old_space_active,
        new_space_active,
        active_resize_space,
        pending_target_space,
        assigned_space,
        keep_assigned_for_scrolling,
        screens,
    } = payload;
    let mut outcome = EventOutcome::default();
    let Some(window) = state.windows.window(wid) else {
        return Ok(outcome);
    };
    let server_id = window.info.sys_id;
    let old_frame = window.frame_monotonic;
    let manageable = window.matches_filter(WindowFilter::EffectivelyManageable);

    if !old_space_active && !new_space_active {
        return Ok(outcome);
    }
    if old_frame.same_as(new_frame) {
        return Ok(outcome);
    }
    if let Some(window) = state.windows.window_mut(wid) {
        window.frame_monotonic = new_frame;
    }
    outcome = EventOutcome::layout_changed(false);

    let dragging = mouse_state == Some(MouseState::Down)
        || matches!(
            drag.drag_state,
            DragState::Active { .. } | DragState::PendingSwap { .. }
        );
    if dragging {
        let needs_session = !matches!(
            &drag.drag_state,
            DragState::Active { session } | DragState::PendingSwap { session, .. }
                if session.window == wid
        );
        if needs_session {
            drag.drag_state = DragState::Active {
                session: crate::app::reactor::DragSession {
                    window: wid,
                    last_frame: old_frame,
                    origin_space: old_space,
                    settled_space: old_space,
                    layout_dirty: false,
                },
            };
        }
        if let DragState::Active { session } = &mut drag.drag_state {
            session.last_frame = new_frame;
            session.layout_dirty = true;
            if session.settled_space != new_space {
                session.settled_space = new_space;
            }
        }
        drag.skip_layout_for_window = Some(wid);
        if !old_frame.size.same_as(new_frame.size) {
            if active_resize_space.is_some() {
                outcome = outcome.with_layout_event(LayoutEvent::WindowResized {
                    wid,
                    old_frame,
                    new_frame,
                    screens,
                });
            }
        } else {
            outcome = outcome.with_drag_swap_evaluation(wid, new_frame);
        }
    } else {
        drag.skip_layout_for_window = Some(wid);
        if old_space != new_space {
            if pending_target_space.is_some()
                && assigned_space == pending_target_space
                && new_space != pending_target_space
            {
                return Ok(outcome);
            }
            if keep_assigned_for_scrolling {
                return Ok(outcome);
            }
            outcome = outcome.with_layout_event(LayoutEvent::WindowRemovedPreserveFloating(wid));
            if let Some(space) = new_space {
                if let Some(server) = server_id {
                    state.windows.set_window_server_space(server, Some(space));
                    state.windows.mark_window_visible(server);
                }
                // Only a manageable window joins the layout here. A window whose model frame
                // sat wholly off screen (a switch's departing row) maps to no space; when the
                // app then reports its real frame this reads as a space change, and Zoom's
                // 301x45 meeting toolbar was tiled as a column of the active workspace and the
                // strip scrolled to show it, pushing the focused window half off screen.
                if new_space_active && manageable {
                    if let Some(workspace) = layout.layout_engine.active_workspace(space) {
                        let _ = layout
                            .layout_engine
                            .virtual_workspace_manager_mut()
                            .assign_window_to_workspace(&mut state.windows, space, wid, workspace);
                    }
                    outcome = outcome.with_layout_event(LayoutEvent::WindowAdded(space, wid));
                }
            } else if let Some(server) = server_id {
                state.windows.set_window_server_space(server, None);
            }
        } else if !old_frame.size.same_as(new_frame.size) && old_space_active {
            outcome.arrange.is_resize = true;
            outcome = outcome.with_layout_event(LayoutEvent::WindowResized {
                wid,
                old_frame,
                new_frame,
                screens,
            });
        }
    }

    if handle_mouse_up_if_needed(drag, false, mouse_state) {
        outcome = outcome.with_mouse_up_dispatch();
    }
    Ok(outcome)
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
        let previous_title = window.info.title.clone();
        if previous_title == new_title {
            return Ok(crate::app::reactor::events::EventOutcome::no_change());
        }
        window.info.title = new_title.clone();
        return Ok(crate::app::reactor::events::EventOutcome::no_change()
            .with_app_rule_reapply(wid)
            .with_window_title_broadcast(wid, previous_title, new_title));
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
    apps: &crate::app::reactor::managers::AppManager,
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
        if let Some(app) = apps.apps.get(&window.pid) {
            app_handles.insert(window.pid, app.handle.clone());
        }
        outcome = outcome.with_raise_request(crate::windows::domain::raise::Event::RaiseRequest(
            crate::windows::domain::raise::RaiseRequest {
                raise_windows: vec![vec![window]],
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
fn handle_mouse_up_if_needed(
    drag: &mut DragManager,
    mission_control_active: bool,
    mouse_state: Option<MouseState>,
) -> bool {
    if mission_control_active {
        drag.reset();
        drag.drag_state = DragState::Inactive;
        drag.skip_layout_for_window = None;
        return false;
    }

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

    use crate::app::reactor::DragState;
    use crate::app::reactor::managers::DragManager;
    use crate::app::reactor::state::RiniState;
    use crate::app::reactor::testing::make_window_info;
    use crate::windows::domain::state::WindowState;
    use crate::windows::domain::transaction::WindowTxStore;
    use rini_core::ids::WindowServerId;

    use super::*;

    const WSID: u32 = 7;

    fn rect(x: f64, y: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(100.0, 100.0))
    }

    fn wid() -> WindowId {
        WindowId::new(1, 1)
    }

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
        }
    }

    /// `mouse_state` is always passed in, because a `None` makes the classifier read the live mouse.
    fn classify(
        state: &mut RiniState,
        transactions: &TransactionManager,
        drag: &mut DragManager,
        new_frame: CGRect,
        last_seen: Option<TransactionId>,
        requested: bool,
        mouse: MouseState,
        mission_control: bool,
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
            mission_control,
        )
    }

    fn handled(d: &FrameChangeDisposition) -> bool {
        matches!(d, FrameChangeDisposition::Handled)
    }

    #[test]
    fn a_frame_report_for_an_unknown_window_is_nothing_to_analyse() {
        let mut empty = RiniState::default();
        let tx = TransactionManager::new(WindowTxStore::new());
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
        let tx = TransactionManager::new(WindowTxStore::new());
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
        let tx = TransactionManager::new(WindowTxStore::new());
        // One record per window holds both the txid and the target, so the write rini is waiting for
        // is stored last and the report claims an earlier one.
        let stale = TransactionId::default().next();
        let current = stale.next();
        tx.store_txid(wsid, current, rect(50.0, 50.0));

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
        let tx = TransactionManager::new(WindowTxStore::new());
        let txid = TransactionId::default().next();
        tx.store_txid(wsid, txid, rect(50.0, 50.0));

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
        assert_eq!(tx.get_target_frame(wsid), None, "the write is acknowledged");
    }

    // A held mouse means the user is dragging, so rini's pending target is abandoned rather than
    // waited for, and the move goes on to geometry analysis.
    #[test]
    fn a_held_mouse_abandons_rinis_pending_target_and_analyses_the_move() {
        let mut s = state();
        let wsid = WindowServerId::new(WSID);
        let tx = TransactionManager::new(WindowTxStore::new());
        let txid = TransactionId::default().next();
        tx.store_txid(wsid, txid, rect(50.0, 50.0));

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
        assert_eq!(
            tx.get_target_frame(wsid),
            None,
            "rini stops waiting for its own write"
        );
    }

    #[test]
    fn a_frame_rini_asked_for_lands_without_analysis() {
        let mut s = state();
        let tx = TransactionManager::new(WindowTxStore::new());
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
        let tx = TransactionManager::new(WindowTxStore::new());
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
}
