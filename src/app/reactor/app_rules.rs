//! The app rules carried out: windows assigned by their rules when they appear, when their space
//! activates and when a title changes, and the placements and resizes a rule asks for.
use std::collections::BTreeMap;

use objc2_core_foundation::CGRect;
use rini_core::ids::{SpaceId, WindowId, pid_t};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use tracing::warn;

use super::{Reactor, WindowFilter};
use crate::windows::domain::info::AppInfo;
use crate::windows::domain::request::Request;
use crate::windows::domain::transaction::TransactionId;
use crate::workspaces::LayoutEvent;
use crate::workspaces::domain::app_rules::AfterRules;
use crate::workspaces::engine::{AppWindows, OnScreenEntry, on_screen_entry};

impl Reactor {
    pub(super) fn apply_app_rules_for_activated_spaces(&mut self, activated: &[SpaceId]) {
        let activated_set: HashSet<SpaceId> = activated.iter().copied().collect();
        let mut windows_by_pid: HashMap<pid_t, Vec<WindowId>> = HashMap::default();

        for (wid, state) in self.state.windows.iter_windows() {
            if !state.matches_filter(WindowFilter::Manageable) {
                continue;
            }
            let Some(space) = self.affinity().best_space_for_window_id(wid) else {
                continue;
            };

            if !activated_set.contains(&space) {
                continue;
            }

            windows_by_pid.entry(wid.pid).or_default().push(wid);
        }

        for (pid, window_ids) in windows_by_pid {
            let Some(app_state) = self.apps.get(&pid) else {
                continue;
            };

            self.process_windows_for_app_rules(pid, window_ids, app_state.info.clone());
        }
    }

    pub(super) fn maybe_reapply_app_rules_for_window(&mut self, window_id: WindowId) {
        if !self.config.virtual_workspaces.reapply_app_rules_on_title_change {
            return;
        }

        let Some(space) = self.affinity().best_space_for_window_id(window_id) else {
            return;
        };
        if !self.is_space_active(space) {
            return;
        }

        let (is_manageable, wsid) = match self.state.windows.window(window_id) {
            Some(window_state) => (
                window_state.matches_filter(WindowFilter::Manageable),
                window_state.info.sys_id,
            ),
            None => return,
        };

        if !is_manageable {
            return;
        }

        let app_info = match self.apps.get(&window_id.pid) {
            Some(app_state) => app_state.info.clone(),
            None => return,
        };

        if let Some(window_server_id) = wsid {
            self.state.windows.mark_wsids_recent(std::iter::once(window_server_id));
        }

        self.process_windows_for_app_rules(window_id.pid, vec![window_id], app_info);
    }

    pub(super) fn apply_app_rule_placements(
        &mut self,
        placements: Vec<crate::workspaces::domain::app_rules::AppRulePlacement>,
    ) {
        for placement in placements {
            let Some(window) = self.state.windows.window(placement.window) else {
                continue;
            };
            let frame = if placement.position.is_some() {
                let Some(screen) = self.space_state.screen_by_space(placement.space) else {
                    warn!(
                        window = ?placement.window,
                        space = ?placement.space,
                        "could not apply app-rule position without screen geometry"
                    );
                    continue;
                };
                placement.resolve_frame(window.frame_monotonic, screen.frame)
            } else {
                placement.resolve_frame(window.frame_monotonic, CGRect::default())
            };

            let window_server_id = window.info.sys_id;
            let transaction = if let Some(window_server_id) = window_server_id {
                let transaction = self.transactions.next_txid(window_server_id);
                self.transactions.insert(window_server_id, transaction, frame);
                transaction
            } else {
                TransactionId::default()
            };
            if let Some(app) = self.apps.get(&placement.window.pid)
                && let Err(error) = app.handle.send(Request::SetWindowFrame {
                    window: placement.window,
                    frame,
                    txid: transaction,
                })
            {
                warn!(window = ?placement.window, %error, "failed to apply app-rule placement");
            }
        }
    }

    pub(super) fn apply_app_rule_resizes(
        &mut self,
        resizes: Vec<crate::workspaces::domain::app_rules::AppRuleResize>,
    ) {
        for resize in resizes {
            let Some(window) = self.state.windows.window(resize.window) else {
                continue;
            };
            let Some(screen) = self.space_state.screen_by_space(resize.space) else {
                warn!(
                    window = ?resize.window,
                    space = ?resize.space,
                    "could not apply app-rule resize without screen geometry"
                );
                continue;
            };
            let new_frame = resize.resized_frame(window.frame_monotonic);
            self.layout_engine.apply_app_rule_resize(
                resize,
                new_frame,
                screen.frame,
                Some(&screen.display_uuid),
            );
        }
    }

    pub(super) fn process_windows_for_app_rules(
        &mut self,
        pid: pid_t,
        window_ids: Vec<WindowId>,
        app_info: AppInfo,
    ) {
        if window_ids.is_empty() {
            return;
        }

        let mut windows_by_space: BTreeMap<SpaceId, Vec<WindowId>> = BTreeMap::new();
        for &wid in &window_ids {
            let Some(state) = self.state.windows.window(wid) else {
                continue;
            };
            if !state.matches_filter(WindowFilter::Manageable) {
                continue;
            }
            let Some(space) = self.affinity().best_space_for_window_id(wid) else {
                continue;
            };
            windows_by_space.entry(space).or_default().push(wid);
        }

        for (space, wids) in windows_by_space {
            if !self.is_space_active(space) {
                continue;
            }
            let mut windows_needing_layout_refresh: Vec<WindowId> = Vec::new();

            for &wid in &wids {
                let engine = &mut self.layout_engine;
                let before = engine.before_rules(&self.state.windows, space, wid);
                let result = engine.assign_window_by_rules(
                    &mut self.state.windows,
                    &mut self.state.display_memory,
                    wid,
                    space,
                    Some(&app_info),
                );
                match engine.settle_app_rule(&mut self.state.windows, wid, space, before, &result) {
                    AfterRules::RefreshLayout => windows_needing_layout_refresh.push(wid),
                    AfterRules::RemoveFromLayout => {
                        self.send_layout_event(LayoutEvent::WindowRemoved(wid));
                    }
                    AfterRules::Settled => {}
                }
            }

            if windows_needing_layout_refresh.is_empty() {
                continue;
            }

            let windows_with_titles: Vec<OnScreenEntry> = windows_needing_layout_refresh
                .iter()
                .map(|&wid| on_screen_entry(wid, self.state.windows.window(wid)))
                .collect();

            self.send_layout_event(LayoutEvent::WindowsOnScreenUpdated(space, AppWindows {
                pid,
                windows: windows_with_titles,
                app_info: Some(app_info.clone()),
            }));
        }
    }
}
