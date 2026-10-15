//! The applications asked for their windows again, and what they report taken in: the window
//! server's info applied, stale windows cleaned up, and the refresh after Mission Control.

use rini_core::ids::{SpaceId, WindowId, WindowServerId, pid_t};
use rustc_hash::FxHashSet as HashSet;
use tracing::{debug, warn};

use super::events::window_discovery;
use super::{Reactor, StaleCleanupState, WindowFilter};
use crate::app::system_reads::server_facts;
use crate::windows::domain::info::{AppInfo, WindowInfo, WindowServerInfo};
use crate::windows::domain::manageable::{Facts, manageable};
use crate::windows::domain::request::Request;
use crate::workspaces::LayoutEvent;

impl Reactor {
    pub(super) fn update_complete_window_server_info(&mut self, ws_info: Vec<WindowServerInfo>) {
        self.state.windows.clear_visible_windows();
        self.update_partial_window_server_info(ws_info);
    }

    pub(super) fn update_partial_window_server_info(&mut self, ws_info: Vec<WindowServerInfo>) {
        // Mark visible windows and remove any corresponding observed WSID markers
        // for ids we now have server info for.
        self.state.windows.set_visible_windows(ws_info.iter().map(|info| info.id));
        for info in ws_info.iter() {
            // If we've been observing this server id from SLS callbacks, clear it.
            self.state.windows.clear_window_server_observed(info.id);
            self.state.windows.track_window_server_info(*info);

            if let Some(wid) = self.state.windows.tracked_window_id(info.id) {
                let (ax, server_id, was_manageable) =
                    if let Some(window) = self.state.windows.window_mut(wid) {
                        if info.layer == 0 {
                            window.frame_monotonic = info.frame;
                        }
                        (
                            Facts::of(&window.info),
                            window.info.sys_id,
                            window.matches_filter(WindowFilter::EffectivelyManageable),
                        )
                    } else {
                        continue;
                    };
                let ws = &*self.reads.window_server;
                let manageable = manageable(server_facts(ws, ax, server_id, |wsid| {
                    self.state.windows.get_window_server_info(wsid)
                }));
                if let Some(window) = self.state.windows.window_mut(wid) {
                    window.is_manageable = manageable;
                }

                if was_manageable && !manageable {
                    self.send_layout_event(LayoutEvent::WindowRemoved(wid));
                }
            }
        }
    }

    pub(super) fn check_for_new_windows(&mut self) {
        // AX discovery remains the source of truth for enumerating app windows.
        // Native-space membership/visibility is supplied separately by the spaces
        // actor; do not replace this with the global CG on-screen window list.
        self.request_visible_windows_for_apps(false);
    }

    pub(super) fn request_visible_windows_for_apps(&mut self, track_mission_control_refresh: bool) {
        if self.refreshes_blocked() {
            self.defer_visible_refresh(track_mission_control_refresh);
            return;
        }

        let mut refreshed_pids = Vec::new();
        for (&pid, app) in &self.apps {
            // Errors mean the app terminated (and a termination event is coming); ignore.
            if app.handle.send(Request::GetVisibleWindows).is_ok() {
                refreshed_pids.push(pid);
            }
        }

        if track_mission_control_refresh {
            self.mission_control_manager
                .pending_mission_control_refresh
                .extend(refreshed_pids);
        }
    }

    pub(super) fn refresh_windows_after_mission_control(&mut self) {
        debug!("Refreshing window state after Mission Control");
        // Skip when on a fullscreen space: kAXWindowsAttribute is space-filtered, so
        // apps omit their Desktop windows. check_for_new_windows sends an untracked
        // GetVisibleWindows whose response bypasses pending_mission_control_refresh,
        // causing those Desktop windows to be dropped from the layout, and other
        // windows in the layout to be incorrecctly resized.
        if !self.has_user_space_context() {
            return;
        }
        let active_windows = self.authoritative_active_space_windows();
        self.refresh_windows_after_mission_control_with_active_windows(active_windows);
    }

    pub(super) fn refresh_windows_after_mission_control_with_active_windows(
        &mut self,
        active_windows: Vec<(WindowServerId, Option<SpaceId>)>,
    ) {
        if self.refreshes_blocked() {
            self.defer_visible_refresh(true);
            return;
        }

        // Mission Control can move windows between native spaces without emitting a
        // matching destroy/appear pair for the origin space. Reconcile the active
        // spaces from the same space-aware WS-id list used everywhere else so we do
        // not depend on the global CG on-screen window list during recovery.
        self.reconcile_authoritative_active_window_snapshot(active_windows, false);
        self.mission_control_manager.pending_mission_control_refresh.clear();
        self.force_refresh_all_windows();
        self.check_for_new_windows();
        self.update_layout(false, false, None);
    }

    // Uses the same "pending refresh" path as Mission Control recovery so a bulk
    // visibility rediscovery can reconcile tracked windows without treating a
    // transient empty AX window list as authoritative removal.
    pub(super) fn force_refresh_all_windows(&mut self) {
        self.request_visible_windows_for_apps(true);
    }

    fn has_user_space_context(&self) -> bool {
        self.raw_command_space().is_some_and(|space| !self.is_fullscreen_space(space))
    }

    pub(super) fn on_windows_discovered_with_app_info(
        &mut self,
        pid: pid_t,
        new: Vec<(WindowId, WindowInfo)>,
        known_visible: Vec<WindowId>,
        app_info: Option<AppInfo>,
    ) {
        let app_info = app_info.or_else(|| self.apps.get(&pid).map(|app| app.info.clone()));
        let ws = self.reads.window_server.clone();
        let inactive_windows = self
            .state
            .windows
            .iter_windows()
            .filter_map(|(wid, _)| {
                (wid.pid == pid && self.affinity().is_window_on_known_inactive_space(wid))
                    .then_some(wid)
            })
            .collect();
        let server_observations = self
            .state
            .windows
            .iter_windows()
            .filter_map(|(wid, window)| (wid.pid == pid).then_some(window.info.sys_id).flatten())
            .map(|wsid| {
                let known = self.state.windows.get_window_server_info(wsid);
                (wsid, window_discovery::StaleWindowObservation {
                    info: known.or_else(|| ws.get_window(wsid)),
                    suitable: ws.app_window_suitability(wsid),
                    ordered_in: ws.window_ordered_in(wsid),
                })
            })
            .collect();
        let stale_snapshot = window_discovery::StaleCleanupSnapshot {
            pending_refresh: self
                .mission_control_manager
                .pending_mission_control_refresh
                .contains(&pid),
            suppressed: matches!(
                self.refocus_manager.stale_cleanup_state,
                StaleCleanupState::Suppressed
            ),
            mission_control_active: self.is_mission_control_active(),
            drag_active: self.is_in_drag(),
            inactive_windows,
            server_observations,
        };
        let (stale_windows, pending_refresh) = window_discovery::identify_stale_windows(
            &self.state,
            pid,
            &known_visible,
            &stale_snapshot,
        );
        let mut outcome = match window_discovery::cleanup_stale_windows(
            &mut self.state,
            &self.transaction_manager,
            &mut self.drag_manager,
            &mut self.mission_control_manager,
            pid,
            stale_windows,
            pending_refresh,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                warn!(%error, pid, "window discovery cleanup failed");
                return;
            }
        };
        let observed_windows = new
            .into_iter()
            .map(|(wid, info)| {
                let current_native_space =
                    info.sys_id.and_then(|wsid| self.affinity().resolve_native_space(wsid, None));
                let active_space = self
                    .affinity()
                    .best_space_for_window(&info.frame, info.sys_id)
                    .filter(|space| self.is_space_active(*space))
                    .or_else(|| {
                        info.sys_id.is_none().then(|| self.workspace_command_space()).flatten()
                    });
                window_discovery::ObservedWindow {
                    wid,
                    info,
                    current_native_space,
                    active_space,
                }
            })
            .collect();
        let (new_windows, process_outcome) = window_discovery::process_window_list(
            &mut self.state,
            &*ws,
            &mut self.layout_engine,
            &mut self.main_window_tracker,
            observed_windows,
            &app_info,
        );
        outcome.absorb(process_outcome);
        window_discovery::update_window_states(&mut self.state, &*ws, new_windows);

        let candidate_windows: HashSet<WindowId> = self
            .state
            .windows
            .iter_windows()
            .filter_map(|(wid, _)| (wid.pid == pid).then_some(wid))
            .chain(known_visible.iter().copied().filter(|wid| wid.pid == pid))
            .collect();
        let discovery_spaces = candidate_windows
            .iter()
            .filter_map(|wid| {
                self.affinity().discovery_space_for_window_id(*wid).map(|space| (*wid, space))
            })
            .collect();
        let authoritative_spaces = candidate_windows
            .iter()
            .filter_map(|wid| {
                self.affinity()
                    .authoritative_space_for_window_id(*wid)
                    .map(|space| (*wid, space))
            })
            .collect();
        let active_spaces = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.space)
            .filter(|space| self.is_space_active(*space))
            .collect();
        let focused_window = self.focused_window_for_discovery(pid);
        outcome.absorb(window_discovery::emit_layout_events(
            &mut self.state,
            &mut self.layout_engine,
            window_discovery::EmitLayoutPayload {
                pid,
                known_visible: &known_visible,
                app_info: &app_info,
                discovery_spaces,
                authoritative_spaces,
                active_spaces,
                focused_window,
            },
        ));
        self.apply_event_outcome(outcome);
    }
}
