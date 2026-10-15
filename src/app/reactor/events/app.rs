use rini_core::ids::{WindowId, pid_t};
use rustc_hash::FxHashMap as HashMap;
use tracing::debug;

use crate::app::reactor::AppState;
use crate::app::reactor::events::{EventOutcome, WindowDiscoveryRequest, window};
use crate::app::reactor::managers::DragManager;
use crate::app::reactor::state::RiniState;
use crate::windows::domain::info::{AppInfo, WindowInfo, WindowServerInfo};
use crate::windows::domain::request::{AppThreadHandle, Quiet};
use crate::windows::domain::transaction::WindowTxStore;
use crate::workspaces::LayoutEvent;

#[derive(Debug)]
pub struct ApplicationLaunchedPayload {
    pub pid: i32,
    pub info: AppInfo,
    pub handle: AppThreadHandle,
    pub visible_windows: Vec<(WindowId, WindowInfo)>,
    pub window_server_info: Vec<WindowServerInfo>,
}

pub fn handle_application_launched(
    apps: &mut HashMap<pid_t, AppState>,
    payload: ApplicationLaunchedPayload,
) -> anyhow::Result<EventOutcome> {
    let ApplicationLaunchedPayload {
        pid,
        info,
        handle,
        visible_windows,
        window_server_info,
    } = payload;
    apps.insert(pid, AppState { info: info.clone(), handle });
    Ok(EventOutcome::window_membership_changed()
        .with_window_notification_refresh()
        .with_window_server_updates(window_server_info)
        .with_discovery(WindowDiscoveryRequest {
            pid,
            new: visible_windows,
            known_visible: Vec::new(),
            app_info: Some(info),
        }))
}

/// An application rini holds a thread for answers through that thread. One it never watched has no
/// thread to answer, so the native-fullscreen records the window server alone gave it go here.
pub fn handle_application_terminated(
    apps: &HashMap<pid_t, AppState>,
    state: &mut RiniState,
    pid: i32,
) -> anyhow::Result<EventOutcome> {
    if !apps.contains_key(&pid) {
        state.windows.remove_windows_for_app(pid);
        return Ok(EventOutcome::no_change());
    }
    Ok(EventOutcome::no_change()
        .with_app_request(pid, crate::windows::domain::request::Request::Terminate))
}

/// No Accessibility destruction follows once the thread has stopped, so each window still held goes
/// here, and with them the application's native-fullscreen records.
pub fn handle_application_thread_terminated(
    apps: &mut HashMap<pid_t, AppState>,
    state: &mut RiniState,
    transactions: &WindowTxStore,
    drag: &mut DragManager,
    pid: i32,
) -> anyhow::Result<EventOutcome> {
    apps.remove(&pid);
    let mut outcome = EventOutcome::window_membership_changed().with_window_notification_refresh();
    let windows: Vec<WindowId> = state.windows.window_ids_for_pid(pid).collect();
    for window in windows {
        outcome.absorb(window::handle_window_destroyed(
            state,
            transactions,
            drag,
            window::WindowDestroyedPayload { window },
        )?);
    }
    state.windows.remove_windows_for_app(pid);
    Ok(outcome.with_arrange_passes(1).with_layout_event(LayoutEvent::AppClosed(pid)))
}

#[derive(Debug, Clone, Copy)]
pub struct ApplicationActivatedPayload {
    pub pid: i32,
    pub quiet: Quiet,
}

pub fn handle_application_activated(
    payload: ApplicationActivatedPayload,
) -> anyhow::Result<EventOutcome> {
    let ApplicationActivatedPayload { pid, quiet } = payload;
    if quiet == Quiet::Yes {
        debug!(
            pid,
            "Skipping auto workspace switch for quiet app activation (initiated by Rini)"
        );
        return Ok(EventOutcome::no_change());
    }

    Ok(EventOutcome::focus_changed(None, false))
}

#[derive(Debug)]
pub struct WindowsDiscoveredPayload {
    pub pid: i32,
    pub new: Vec<(WindowId, WindowInfo)>,
    pub known_visible: Vec<WindowId>,
}

pub fn handle_windows_discovered(
    payload: WindowsDiscoveredPayload,
) -> anyhow::Result<EventOutcome> {
    let WindowsDiscoveredPayload { pid, new, known_visible } = payload;
    Ok(EventOutcome::window_membership_changed()
        .with_window_notification_refresh()
        .with_discovery(WindowDiscoveryRequest {
            pid,
            new,
            known_visible,
            app_info: None,
        }))
}
