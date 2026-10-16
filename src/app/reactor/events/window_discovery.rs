use std::collections::BTreeMap;

use rini_core::ids::{SpaceId, WindowId, WindowServerId, pid_t};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use tracing::{debug, trace};

use super::window;
use crate::app::reactor::LayoutEvent;
use crate::app::system_reads::server_facts;
use crate::windows::domain::focus::MainWindowTracker;
use crate::windows::domain::info::{AppInfo, WindowInfo};
use crate::windows::domain::manageable::{Facts, Liveness, looks_gone, manageable};
use crate::windows::domain::state::{WindowFilter, WindowState};
use crate::windows::domain::transaction::WindowTxStore;
use crate::windows::platform::window_server::WindowServer;
use crate::workspaces::AppRuleResult;
use crate::workspaces::domain::app_rules::AfterRules;
use crate::workspaces::domain::virtual_workspace::WorkspaceError;
use crate::workspaces::engine::{AppWindows, OnScreenEntry, on_screen_entry};

/// Handler for window discovery events, responsible for processing newly discovered windows
/// and managing the lifecycle of window state in the reactor.
fn sync_existing_window_state(
    state: &mut crate::app::reactor::state::RiniState,
    window_server: &dyn WindowServer,
    wid: WindowId,
    info: &WindowInfo,
    active_space: Option<SpaceId>,
) -> anyhow::Result<crate::app::reactor::events::EventOutcome> {
    let was_minimized = state.windows.window(wid).is_some_and(|window| window.info.is_minimized);
    let was_manageable = state
        .windows
        .window(wid)
        .is_some_and(|window| window.matches_filter(WindowFilter::EffectivelyManageable));

    if let Some(existing) = state.windows.window_mut(wid) {
        existing.info.title = info.title.clone();
        if info.frame.size.width != 0.0 || info.frame.size.height != 0.0 {
            existing.frame_monotonic = info.frame;
        }
        existing.info.is_standard = info.is_standard;
        existing.info.is_root = info.is_root;
        existing.info.is_resizable = info.is_resizable;
        existing.info.min_size = info.min_size;
        existing.info.max_size = info.max_size;
        existing.info.sys_id = info.sys_id;
        existing.info.bundle_id = info.bundle_id.clone();
        existing.info.path = info.path.clone();
        existing.info.ax_role = info.ax_role.clone();
        existing.info.ax_subrole = info.ax_subrole.clone();
    } else {
        return Ok(crate::app::reactor::events::EventOutcome::default());
    }

    let outcome = match (was_minimized, info.is_minimized) {
        (false, true) => window::handle_window_minimized(state, wid)?,
        (true, false) => window::handle_window_deminiaturized(
            state,
            window_server,
            window::WindowDeminiaturizedPayload { window: wid, active_space },
        )?,
        _ => {
            let manageable = manageable(server_facts(
                window_server,
                Facts::of(info),
                info.sys_id,
                |wsid| state.windows.get_window_server_info(wsid),
            ));
            if let Some(existing) = state.windows.window_mut(wid) {
                existing.info.is_minimized = info.is_minimized;
                existing.is_manageable = manageable;
            }
            if was_manageable && !manageable {
                crate::app::reactor::events::EventOutcome::default()
                    .with_layout_event(LayoutEvent::WindowRemoved(wid))
            } else {
                crate::app::reactor::events::EventOutcome::default()
            }
        }
    };

    if was_minimized != info.is_minimized {
        debug!(
            ?wid,
            was_minimized,
            is_minimized = info.is_minimized,
            "Window minimize state reconciled from discovery"
        );
    }
    Ok(outcome)
}

fn should_emit_window_for_space(
    state: &crate::app::reactor::state::RiniState,
    engine: &crate::workspaces::LayoutEngine,
    space: SpaceId,
    wid: WindowId,
) -> bool {
    let assigned_workspace =
        engine
            .virtual_workspace_manager()
            .workspace_for_window(&state.windows, space, wid);
    let active_workspace = engine.active_workspace(space);

    match (assigned_workspace, active_workspace) {
        (Some(assigned), Some(active)) => assigned == active,
        _ => true,
    }
}

fn sync_window_server_id_mapping(
    state: &mut crate::app::reactor::state::RiniState,
    layout: &mut crate::workspaces::LayoutEngine,
    focus: &mut MainWindowTracker,
    wid: WindowId,
    old_sys_id: Option<WindowServerId>,
    new_sys_id: Option<WindowServerId>,
    current_native_space: Option<SpaceId>,
) -> crate::app::reactor::events::EventOutcome {
    let mut outcome = crate::app::reactor::events::EventOutcome::default();
    if old_sys_id != new_sys_id
        && let Some(old_wsid) = old_sys_id
        && state.windows.tracked_window_id(old_wsid) == Some(wid)
    {
        state.windows.remove_window_server_state(old_wsid);
    }

    if let Some(new_wsid) = new_sys_id {
        if let Some(previous_wid) = state.windows.track_window_server_id(new_wsid, wid)
            && previous_wid != wid
        {
            layout.rekey_window_identity(
                &mut state.windows,
                &mut state.display_memory,
                previous_wid,
                wid,
            );
            focus.rekey_window(previous_wid, wid);
            outcome = outcome
                .with_forgotten_window(previous_wid)
                .with_layout_event(LayoutEvent::WindowRemovedPreserveFloating(previous_wid));
            state.windows.remove_window(previous_wid);
        }
        if let (Some(record), Some(current_space)) = (
            state.windows.native_fullscreen_record_for_window(wid),
            current_native_space,
        ) {
            let target_user_space = record.assigned_space.or(record.last_known_user_space);
            if current_space != record.fullscreen_space && Some(current_space) == target_user_space
            {
                let _ = state.windows.restore_window_from_native_fullscreen(wid);
            }
        }
    }
    outcome
}

/// Identify windows that should be removed as stale.
#[derive(Debug)]
pub(crate) struct StaleCleanupSnapshot {
    pub(crate) pending_refresh: bool,
    pub(crate) mid_change: Option<MidChange>,
    pub(crate) inactive_windows: HashSet<WindowId>,
    pub(crate) server_observations: HashMap<WindowServerId, StaleWindowObservation>,
}

/// Why the world is mid-change, which holds the stale cleanup back until it settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MidChange {
    /// A refocus suppressed the cleanup.
    Suppressed,
    MissionControl,
    Drag,
}

#[derive(Debug)]
pub(crate) struct StaleWindowObservation {
    pub(crate) info: Option<crate::windows::domain::info::WindowServerInfo>,
    pub(crate) liveness: Liveness,
}

pub(crate) fn identify_stale_windows(
    state: &crate::app::reactor::state::RiniState,
    pid: pid_t,
    known_visible: &[WindowId],
    snapshot: &StaleCleanupSnapshot,
) -> (Vec<WindowId>, bool) {
    let known_visible_set: HashSet<WindowId> = known_visible.iter().cloned().collect();
    let pending_refresh = snapshot.pending_refresh;

    let has_visible_window_server_ids = state
        .windows
        .iter_visible_window_server_ids()
        .any(|wsid| state.windows.tracked_window_id(wsid).is_some_and(|wid| wid.pid == pid));
    let skip_stale_cleanup = snapshot.mid_change.is_some()
        || (known_visible_set.is_empty() && !has_visible_window_server_ids);

    if skip_stale_cleanup {
        return (Vec::new(), false);
    }

    let stale_windows = state
        .windows
        .iter_windows()
        .filter_map(|(wid, window_state)| {
            if wid.pid != pid || known_visible_set.contains(&wid) {
                return None;
            }

            if window_state.info.is_minimized {
                return None;
            }

            let Some(ws_id) = window_state.info.sys_id else {
                trace!(
                    ?wid,
                    "Skipping stale cleanup for window without window server id"
                );
                return None;
            };

            if snapshot.inactive_windows.contains(&wid) {
                trace!(
                    ?wid,
                    ws_id = ?ws_id,
                    "Skipping stale cleanup; window is on a known inactive space"
                );
                return None;
            }

            let observation = snapshot.server_observations.get(&ws_id)?;
            let info = match observation.info.as_ref() {
                Some(info) => info,
                None => {
                    trace!(
                        ?wid,
                        ws_id = ?ws_id,
                        "Skipping stale cleanup for window without server info"
                    );
                    return None;
                }
            };

            looks_gone(info, observation.liveness).then_some(wid)
        })
        .collect();

    (stale_windows, pending_refresh)
}

/// Remove stale windows and send events.
pub(crate) fn cleanup_stale_windows(
    state: &mut crate::app::reactor::state::RiniState,
    transactions: &WindowTxStore,
    drag: &mut crate::app::reactor::managers::DragManager,
    mission_control: &mut crate::app::reactor::managers::MissionControlManager,
    pid: pid_t,
    stale_windows: Vec<WindowId>,
    pending_refresh: bool,
) -> anyhow::Result<crate::app::reactor::events::EventOutcome> {
    let mut outcome = crate::app::reactor::events::EventOutcome::default();
    for wid in stale_windows {
        outcome.absorb(window::handle_window_destroyed(
            state,
            transactions,
            drag,
            window::WindowDestroyedPayload { window: wid },
        )?);
    }
    if pending_refresh {
        mission_control.pending_mission_control_refresh.remove(&pid);
    }
    Ok(outcome)
}

/// Process new and updated windows, returning lists of new and updated windows.
#[derive(Debug)]
pub(crate) struct ObservedWindow {
    pub(crate) wid: WindowId,
    pub(crate) info: WindowInfo,
    pub(crate) current_native_space: Option<SpaceId>,
    pub(crate) active_space: Option<SpaceId>,
}

pub(crate) fn process_window_list(
    state: &mut crate::app::reactor::state::RiniState,
    window_server: &dyn WindowServer,
    layout: &mut crate::workspaces::LayoutEngine,
    focus: &mut MainWindowTracker,
    observed: Vec<ObservedWindow>,
    app_info: &Option<AppInfo>,
) -> (
    Vec<(WindowId, WindowInfo)>,
    crate::app::reactor::events::EventOutcome,
) {
    const APP_RULE_TTL_MS: u64 = 1000;

    let mut new_windows = Vec::new();
    let mut outcome = crate::app::reactor::events::EventOutcome::default();

    state.windows.purge_expired(APP_RULE_TTL_MS);

    let any_recent = observed.iter().any(|window| {
        let info = &window.info;
        info.sys_id
            .is_some_and(|wsid| state.windows.is_wsid_recent(wsid, APP_RULE_TTL_MS))
    });

    if any_recent && app_info.is_none() && !observed.is_empty() {
        // Update state for any newly reported windows, but do not early-return;
        // proceed to emit WindowsOnScreenUpdated so existing mappings are respected
        // without reapplying app rules.
        for window in &observed {
            if state.windows.contains_window(window.wid) {
                outcome.absorb(sync_known_window(state, window_server, layout, focus, window));
            } else {
                let mut window_state: WindowState = window.info.clone().into();
                let manageable = manageable(server_facts(
                    window_server,
                    Facts::of(&window_state.info),
                    window_state.info.sys_id,
                    |wsid| state.windows.get_window_server_info(wsid),
                ));
                window_state.is_manageable = manageable;
                state.windows.insert_window(window.wid, window_state);
            }
            outcome.absorb(sync_window_server_id_mapping(
                state,
                layout,
                focus,
                window.wid,
                None,
                window.info.sys_id,
                window.current_native_space,
            ));
        }
        // fall through
    }

    // Process all new windows
    for window in observed {
        if state.windows.contains_window(window.wid) {
            outcome.absorb(sync_known_window(state, window_server, layout, focus, &window));
        } else {
            outcome.absorb(sync_window_server_id_mapping(
                state,
                layout,
                focus,
                window.wid,
                None,
                window.info.sys_id,
                window.current_native_space,
            ));
            new_windows.push((window.wid, window.info));
        }
    }

    (new_windows, outcome)
}

/// A window rini already tracks, reported again: its window-server id mapping moved to the one now
/// reported, and its state brought up to date.
fn sync_known_window(
    state: &mut crate::app::reactor::state::RiniState,
    window_server: &dyn WindowServer,
    layout: &mut crate::workspaces::LayoutEngine,
    focus: &mut MainWindowTracker,
    window: &ObservedWindow,
) -> crate::app::reactor::events::EventOutcome {
    let old_sys_id = state.windows.window(window.wid).and_then(|known| known.info.sys_id);
    let mut outcome = sync_window_server_id_mapping(
        state,
        layout,
        focus,
        window.wid,
        old_sys_id,
        window.info.sys_id,
        window.current_native_space,
    );
    if let Ok(existing_outcome) = sync_existing_window_state(
        state,
        window_server,
        window.wid,
        &window.info,
        window.active_space,
    ) {
        outcome.absorb(existing_outcome);
    }
    outcome
}

/// Inserts the newly discovered window snapshots into domain state.
pub(crate) fn update_window_states(
    rini_state: &mut crate::app::reactor::state::RiniState,
    window_server: &dyn WindowServer,
    new_windows: Vec<(WindowId, WindowInfo)>,
) {
    // Update or insert window states
    for (wid, info) in new_windows {
        let mut state: WindowState = info.into();
        let manageable = manageable(server_facts(
            window_server,
            Facts::of(&state.info),
            state.info.sys_id,
            |wsid| rini_state.windows.get_window_server_info(wsid),
        ));
        state.is_manageable = manageable;
        rini_state.windows.insert_window(wid, state);
    }
}

/// Send layout events for discovered windows.
pub(crate) struct EmitLayoutPayload<'a> {
    pub(crate) pid: pid_t,
    pub(crate) known_visible: &'a [WindowId],
    pub(crate) app_info: &'a Option<AppInfo>,
    pub(crate) discovery_spaces: HashMap<WindowId, SpaceId>,
    pub(crate) authoritative_spaces: HashMap<WindowId, SpaceId>,
    pub(crate) active_spaces: Vec<SpaceId>,
    pub(crate) focused_window: Option<(SpaceId, WindowId)>,
}

pub(crate) fn emit_layout_events(
    state: &mut crate::app::reactor::state::RiniState,
    layout: &mut crate::workspaces::LayoutEngine,
    payload: EmitLayoutPayload<'_>,
) -> crate::app::reactor::events::EventOutcome {
    let mut outcome = crate::app::reactor::events::EventOutcome::default();
    if !state.windows.iter_windows().any(|(wid, _)| wid.pid == payload.pid) {
        return outcome;
    }
    let mut app_windows = windows_to_list(state, layout, &payload);
    // Pre-pass: update the VWM for all windows definitively assigned to a space before
    // processing any per-space layout events. Without this, the ordering of space events
    // determines whether a window removed from one space's tree gets re-added by the
    // loop in sync_tiled_windows_for_app (which reads the VWM state at event time).
    // By updating the VWM upfront, the guard logic in sync_tiled_windows_for_app can
    // correctly identify cross-space moves regardless of event ordering.
    let mut assignment_results =
        assign_by_rules(state, layout, &app_windows, payload.app_info.as_ref());

    let EmitLayoutPayload {
        pid,
        app_info,
        active_spaces,
        focused_window,
        ..
    } = payload;
    let discovered_spaces = active_spaces.to_vec();
    for space in active_spaces {
        let windows_for_space = app_windows.remove(&space).unwrap_or_default();

        if !windows_for_space.is_empty() {
            for &wid in &windows_for_space {
                let assign_result = assignment_results.remove(&(space, wid)).unwrap_or_else(|| {
                    layout.assign_window_by_rules(
                        &mut state.windows,
                        &mut state.display_memory,
                        wid,
                        space,
                        app_info.as_ref(),
                    )
                });
                // Discovery re-lists every window below, so only the removal matters here.
                let before = layout.before_rules(&state.windows, space, wid);
                if layout.settle_app_rule(&mut state.windows, wid, space, before, &assign_result)
                    == AfterRules::RemoveFromLayout
                {
                    outcome = outcome.with_layout_event(LayoutEvent::WindowRemoved(wid));
                }
            }
        }

        let windows_with_titles: Vec<OnScreenEntry> = windows_for_space
            .iter()
            .filter_map(|&wid| {
                let window = state.windows.window(wid)?;
                window
                    .matches_filter(WindowFilter::EffectivelyManageable)
                    .then(|| on_screen_entry(wid, Some(window)))
            })
            .collect();

        outcome =
            outcome.with_layout_event(LayoutEvent::WindowsOnScreenUpdated(space, AppWindows {
                pid,
                windows: windows_with_titles,
                app_info: app_info.clone(),
            }));
    }

    // Matching is allowed to observe every native-space slice for this application before stale
    // saved identities are discarded. Cleaning after an individual slice would incorrectly drop
    // windows that are discovered on a later space in the same batch.
    outcome = outcome.with_layout_event(LayoutEvent::WindowDiscoveryCompleted(
        pid,
        app_info.as_ref().and_then(|info| info.bundle_id.clone()),
        discovered_spaces,
    ));

    if let Some((space, main_window)) = focused_window {
        outcome = outcome.with_layout_event(LayoutEvent::WindowFocused(space, main_window));
    }
    outcome
}

/// The application's manageable windows to list, by the space they were discovered on: those the
/// window server shows, then those the app reports visible when the window server shows none on an
/// active space.
fn windows_to_list(
    state: &crate::app::reactor::state::RiniState,
    layout: &crate::workspaces::LayoutEngine,
    payload: &EmitLayoutPayload<'_>,
) -> BTreeMap<SpaceId, Vec<WindowId>> {
    let EmitLayoutPayload {
        pid,
        known_visible,
        discovery_spaces,
        authoritative_spaces,
        active_spaces,
        ..
    } = payload;
    let pid = *pid;
    let mut app_windows: BTreeMap<SpaceId, Vec<WindowId>> = BTreeMap::new();
    let mut included: HashSet<WindowId> = HashSet::default();
    let has_visible_window_server_windows = state
        .windows
        .iter_visible_window_server_ids()
        .filter_map(|wsid| state.windows.tracked_window_id(wsid))
        .any(|wid| {
            wid.pid == pid
                && state.windows.window(wid).is_some_and(|window| {
                    window.matches_filter(WindowFilter::EffectivelyManageable)
                })
        });

    // Collect windows from visible window server IDs
    for wid in state
        .windows
        .iter_visible_window_server_ids()
        .filter_map(|wsid| state.windows.tracked_window_id(wsid))
        .filter(|wid| wid.pid == pid)
        .filter(|wid| {
            state
                .windows
                .window(*wid)
                .is_some_and(|window| window.matches_filter(WindowFilter::EffectivelyManageable))
        })
    {
        let Some(space) = discovery_spaces.get(&wid).copied() else {
            continue;
        };
        if !should_emit_window_for_space(state, layout, space, wid) {
            continue;
        }
        included.insert(wid);
        app_windows.entry(space).or_default().push(wid);
    }

    // If we have no visible WSIDs (e.g., SpaceChanged provided empty ws_info),
    // fall back to the app-reported known_visible list for this pid.
    for wid in known_visible.iter().copied().filter(|wid| wid.pid == pid) {
        if included.contains(&wid)
            || !state
                .windows
                .window(wid)
                .is_some_and(|window| window.matches_filter(WindowFilter::EffectivelyManageable))
        {
            continue;
        }
        if has_visible_window_server_windows
            && authoritative_spaces
                .get(&wid)
                .is_none_or(|space| !active_spaces.contains(space))
        {
            // Once the active-space snapshot already contains some windows for
            // this app, do not let AX-only fallback resurrect other windows on
            // the current desktop via geometry inference alone.
            continue;
        }
        let Some(_state) = state.windows.window(wid) else {
            continue;
        };
        let Some(space) = discovery_spaces.get(&wid).copied() else {
            continue;
        };
        if !should_emit_window_for_space(state, layout, space, wid) {
            continue;
        }
        included.insert(wid);
        app_windows.entry(space).or_default().push(wid);
    }

    app_windows
}

/// Each window to list assigned by the app rules on the space it was discovered on.
fn assign_by_rules(
    state: &mut crate::app::reactor::state::RiniState,
    layout: &mut crate::workspaces::LayoutEngine,
    app_windows: &BTreeMap<SpaceId, Vec<WindowId>>,
    app_info: Option<&AppInfo>,
) -> BTreeMap<(SpaceId, WindowId), Result<AppRuleResult, WorkspaceError>> {
    let mut assignment_results = BTreeMap::new();
    for (&space, windows_for_space) in app_windows {
        for &wid in windows_for_space {
            assignment_results.insert(
                (space, wid),
                layout.assign_window_by_rules(
                    &mut state.windows,
                    &mut state.display_memory,
                    wid,
                    space,
                    app_info,
                ),
            );
        }
    }
    assignment_results
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};

    use super::*;
    use crate::app::reactor::state::RiniState;
    use crate::app::reactor::testing::make_window_info;
    use crate::windows::domain::info::WindowServerInfo;
    use crate::windows::domain::state::WindowState;

    fn wid(pid: pid_t, idx: u32) -> WindowId { WindowId::new(pid, idx) }

    fn frame() -> CGRect { CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(100.0, 100.0)) }

    /// One app with `count` windows, each tracked under a window-server id equal to its index.
    fn state_with(pid: pid_t, count: u32) -> RiniState {
        let mut state = RiniState::default();
        for idx in 1..=count {
            let wsid = WindowServerId::new(idx);
            let info = make_window_info(frame(), Some(wsid), "w", Some("com.test"));
            state.windows.insert_window(wid(pid, idx), WindowState::from(info));
            state.windows.track_window_server_id(wsid, wid(pid, idx));
            // Without a visible window the app looks absent, and cleanup skips entirely.
            state.windows.mark_window_visible(wsid);
        }
        state
    }

    fn manageable_state_with(pid: pid_t, visible: u32, reported_only: u32) -> RiniState {
        let mut state = state_with(pid, visible);
        for idx in visible + 1..=visible + reported_only {
            let wsid = WindowServerId::new(idx);
            let info = make_window_info(frame(), Some(wsid), "w", Some("com.test"));
            state.windows.insert_window(wid(pid, idx), WindowState::from(info));
            state.windows.track_window_server_id(wsid, wid(pid, idx));
        }
        for idx in 1..=visible + reported_only {
            state.windows.window_mut(wid(pid, idx)).unwrap().is_manageable = true;
        }
        state
    }

    fn listed(
        state: &RiniState,
        known_visible: &[WindowId],
        authoritative: &[(WindowId, SpaceId)],
    ) -> BTreeMap<SpaceId, Vec<WindowId>> {
        let layout = crate::workspaces::LayoutEngine::new(
            &crate::app::config::VirtualWorkspaceSettings::default(),
            &crate::app::config::LayoutSettings::default(),
        );
        let space = SpaceId::new(1);
        let discovery_spaces = (1..=4).map(|idx| (wid(1, idx), space)).collect::<HashMap<_, _>>();
        windows_to_list(state, &layout, &EmitLayoutPayload {
            pid: 1,
            known_visible,
            app_info: &None,
            discovery_spaces,
            authoritative_spaces: authoritative.iter().copied().collect(),
            active_spaces: vec![space],
            focused_window: None,
        })
    }

    /// The windows the server shows are listed first; a window only the app reports visible joins
    /// them only when the server's active-space snapshot puts it on an active space.
    #[test]
    fn a_window_only_the_app_reports_is_listed_only_on_an_active_space() {
        let state = manageable_state_with(1, 2, 1);
        let reported = [wid(1, 3)];
        let space = SpaceId::new(1);

        let sorted = |listed: BTreeMap<SpaceId, Vec<WindowId>>| {
            let mut windows = listed[&space].clone();
            windows.sort();
            (listed.len(), windows)
        };
        assert_eq!(
            sorted(listed(&state, &reported, &[])),
            (1, vec![wid(1, 1), wid(1, 2)])
        );
        assert_eq!(
            sorted(listed(&state, &reported, &[(wid(1, 3), space)])),
            (1, vec![wid(1, 1), wid(1, 2), wid(1, 3)])
        );
    }

    /// With nothing on screen for the app, what the app reports visible is listed as it is.
    #[test]
    fn with_no_window_on_screen_the_apps_report_is_listed() {
        let state = manageable_state_with(1, 0, 2);
        let reported = [wid(1, 1), wid(1, 2), wid(2, 1)];
        assert_eq!(
            listed(&state, &reported, &[]),
            BTreeMap::from([(SpaceId::new(1), vec![wid(1, 1), wid(1, 2)])]),
            "another app's window is never listed"
        );
    }

    /// A window the server still knows about, at a sane frame: not gone.
    fn present(wsid: u32) -> StaleWindowObservation {
        StaleWindowObservation {
            info: Some(WindowServerInfo {
                id: WindowServerId::new(wsid),
                pid: 1,
                layer: 0,
                frame: frame(),
                min_frame: CGSize::new(0.0, 0.0),
                max_frame: CGSize::new(0.0, 0.0),
            }),
            liveness: Liveness {
                suitable: Some(true),
                ordered_in: Some(true),
            },
        }
    }

    /// The server answers, and its answer is that the window is neither suitable nor on screen.
    fn gone(wsid: u32) -> StaleWindowObservation {
        StaleWindowObservation {
            liveness: Liveness {
                suitable: Some(false),
                ordered_in: Some(false),
            },
            ..present(wsid)
        }
    }

    /// The server did not answer. Not the same as answering "gone".
    fn unanswered() -> StaleWindowObservation {
        StaleWindowObservation {
            info: None,
            liveness: Liveness {
                suitable: None,
                ordered_in: None,
            },
        }
    }

    fn snapshot(observations: Vec<(u32, StaleWindowObservation)>) -> StaleCleanupSnapshot {
        StaleCleanupSnapshot {
            pending_refresh: false,
            mid_change: None,
            inactive_windows: HashSet::default(),
            server_observations: observations
                .into_iter()
                .map(|(wsid, o)| (WindowServerId::new(wsid), o))
                .collect(),
        }
    }

    #[test]
    fn a_window_the_app_no_longer_lists_and_the_server_calls_gone_is_stale() {
        let state = state_with(1, 2);
        let snap = snapshot(vec![(1, present(1)), (2, gone(2))]);
        let (stale, _) = identify_stale_windows(&state, 1, &[wid(1, 1)], &snap);
        assert_eq!(stale, vec![wid(1, 2)]);
    }

    #[test]
    fn a_window_the_app_still_lists_is_never_stale() {
        let state = state_with(1, 2);
        // Even with the server calling it gone: the app is authoritative about its own windows.
        let snap = snapshot(vec![(1, gone(1)), (2, gone(2))]);
        let (stale, _) = identify_stale_windows(&state, 1, &[wid(1, 1), wid(1, 2)], &snap);
        assert!(stale.is_empty());
    }

    // The distinction the whole path turns on. See `docs/testing.md`: an unanswerable query and a
    // negative one mean different things, and treating "no answer" as "gone" retires live windows.
    #[test]
    fn an_unanswered_query_is_not_a_negative_one() {
        let state = state_with(1, 1);
        let snap = snapshot(vec![(1, unanswered())]);
        let (stale, _) = identify_stale_windows(&state, 1, &[], &snap);
        assert!(stale.is_empty(), "no server answer must not retire a window");
    }

    #[test]
    fn a_window_with_no_observation_at_all_is_left_alone() {
        let state = state_with(1, 1);
        let (stale, _) = identify_stale_windows(&state, 1, &[], &snapshot(vec![]));
        assert!(stale.is_empty());
    }

    #[test]
    fn another_apps_windows_are_never_touched() {
        let mut state = state_with(1, 1);
        let wsid = WindowServerId::new(9);
        let info = make_window_info(frame(), Some(wsid), "other", Some("com.other"));
        state.windows.insert_window(wid(2, 9), WindowState::from(info));
        state.windows.track_window_server_id(wsid, wid(2, 9));
        let snap = snapshot(vec![(1, gone(1)), (9, gone(9))]);
        let (stale, _) = identify_stale_windows(&state, 1, &[], &snap);
        assert_eq!(
            stale,
            vec![wid(1, 1)],
            "pid 2's window is not this app's business"
        );
    }

    #[test]
    fn a_minimized_window_is_not_stale_for_being_absent() {
        let mut state = state_with(1, 1);
        let mut info = make_window_info(frame(), Some(WindowServerId::new(1)), "w", None);
        info.is_minimized = true;
        state.windows.insert_window(wid(1, 1), WindowState::from(info));
        let snap = snapshot(vec![(1, gone(1))]);
        let (stale, _) = identify_stale_windows(&state, 1, &[], &snap);
        assert!(stale.is_empty(), "minimized windows are absent on purpose");
    }

    #[test]
    fn a_window_on_a_known_inactive_space_is_not_stale_for_being_absent() {
        let state = state_with(1, 1);
        let mut snap = snapshot(vec![(1, gone(1))]);
        snap.inactive_windows.insert(wid(1, 1));
        let (stale, _) = identify_stale_windows(&state, 1, &[], &snap);
        assert!(stale.is_empty(), "a parked window is absent on purpose");
    }

    #[test]
    fn cleanup_is_skipped_whenever_the_world_is_mid_change() {
        let state = state_with(1, 1);
        for change in [
            MidChange::Suppressed,
            MidChange::MissionControl,
            MidChange::Drag,
        ] {
            let mut snap = snapshot(vec![(1, gone(1))]);
            snap.mid_change = Some(change);
            let (stale, _) = identify_stale_windows(&state, 1, &[], &snap);
            assert!(stale.is_empty(), "{snap:?} should skip cleanup entirely");
        }
    }

    #[test]
    fn pending_refresh_is_reported_back_when_cleanup_runs() {
        let state = state_with(1, 1);
        let mut snap = snapshot(vec![(1, gone(1))]);
        snap.pending_refresh = true;
        let (_, pending) = identify_stale_windows(&state, 1, &[], &snap);
        assert!(pending, "the caller clears the pid from the pending-refresh set");
    }

    // Skipping cleanup also withholds pending_refresh, so the pid stays queued and the refresh is
    // retried once the world settles. Clearing it here would drop the retry.
    #[test]
    fn a_skipped_cleanup_leaves_the_pending_refresh_queued() {
        let state = state_with(1, 1);
        let mut snap = snapshot(vec![(1, gone(1))]);
        snap.pending_refresh = true;
        snap.mid_change = Some(MidChange::MissionControl);
        let (stale, pending) = identify_stale_windows(&state, 1, &[], &snap);
        assert!(stale.is_empty());
        assert!(!pending, "the pid must stay queued for a retry");
    }

    #[test]
    fn an_app_with_nothing_visible_is_not_judged_at_all() {
        let mut state = RiniState::default();
        let info = make_window_info(frame(), Some(WindowServerId::new(1)), "w", None);
        state.windows.insert_window(wid(1, 1), WindowState::from(info));
        state.windows.track_window_server_id(WindowServerId::new(1), wid(1, 1));
        let (stale, _) = identify_stale_windows(&state, 1, &[], &snapshot(vec![(1, gone(1))]));
        assert!(
            stale.is_empty(),
            "nothing visible and nothing listed teaches nothing"
        );
    }
}
