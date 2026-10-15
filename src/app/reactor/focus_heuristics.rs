//! Where focus goes when nothing names it: the window under the cursor, the desktop of an empty
//! workspace, the window a workspace switch focuses, raising on mouse over, and focus follows mouse
//! held off while a menu or Mission Control is open.

use objc2_core_foundation::CGPoint;
use rini_core::ids::{SpaceId, WindowId, WindowServerId, pid_t};
use tracing::{debug, instrument, trace, warn};

use super::{MenuState, Reactor, RefocusState, WindowFilter};
use crate::input::platform::input_tap;
use crate::windows::domain::stack::covered_by_peer_above;
use crate::workspaces::LayoutEvent;

impl Reactor {
    pub(crate) fn window_id_under_cursor(&self) -> Option<WindowId> {
        self.tracked_window_under_cursor().map(|(_, wid)| wid)
    }

    fn window_server_id_under_cursor(&self) -> Option<WindowServerId> {
        self.reads.window_server.window_under_cursor()
    }

    fn tracked_window_under_cursor(&self) -> Option<(WindowServerId, WindowId)> {
        let wsid = self.window_server_id_under_cursor()?;
        let wid = self.state.windows.tracked_window_id(wsid)?;
        Some((wsid, wid))
    }

    pub(super) fn activation_from_unmanageable_window(&self, pid: pid_t) -> Option<WindowServerId> {
        let (wsid, wid) = self.tracked_window_under_cursor()?;
        let window = self.state.windows.window(wid)?;
        (wid.pid == pid && !window.matches_filter(WindowFilter::EffectivelyManageable))
            .then_some(wsid)
    }

    fn focus_untracked_window_under_cursor(&mut self) -> bool {
        let Some(wsid) = self.window_server_id_under_cursor() else {
            return false;
        };
        if self.state.windows.tracked_window_id(wsid).is_some() {
            return false;
        }

        let window_info = self
            .state
            .windows
            .get_window_server_info(wsid)
            .or_else(|| self.reads.window_server.get_window(wsid));

        let Some(info) = window_info else { return false };
        self.reads.window_server.make_key_window(info.pid, wsid).is_ok()
    }

    pub(super) fn focus_desktop_if_active_workspace_empty(&mut self, space: SpaceId) -> bool {
        if !self.is_space_active(space)
            || !self
                .layout_engine
                .windows_in_active_workspace(&self.state.windows, space)
                .is_empty()
        {
            return false;
        }
        let Some(screen) = self.space_state.screen_by_space(space) else {
            return false;
        };
        if !self.reads.window_server.focus_desktop(&screen.display_uuid, screen.frame) {
            return false;
        }

        self.layout_engine.commit_workspace_focus(&mut self.state.windows, space, None);
        true
    }

    pub(super) fn last_focused_window_in_space(&self, space: SpaceId) -> Option<WindowId> {
        let active_workspace = self.layout_engine.active_workspace(space)?;
        let wid = self
            .layout_engine
            .virtual_workspace_manager()
            .last_focused_window(space, active_workspace)?;
        let window = self.state.windows.window(wid)?;

        if self.affinity().best_space_for_window_id(wid)? != space {
            return None;
        }
        if window
            .info
            .sys_id
            .is_some_and(|wsid| !self.state.windows.is_window_visible(wsid))
        {
            return None;
        }
        Some(wid)
    }

    /// The window a workspace switch focuses. The preferred window (the one the switch is for)
    /// only has to belong to the workspace: it is parked until the layout lands, so the window
    /// server may report it off screen. The fallbacks must be visible. See
    /// `choose_switch_focus`.
    pub(super) fn visible_focus_candidate_in_active_workspace(
        &self,
        space: SpaceId,
        preferred: Option<WindowId>,
    ) -> Option<WindowId> {
        let in_space = |wid: WindowId| {
            self.state.windows.window(wid).is_some_and(|w| w.info.sys_id.is_some())
                && self.affinity().best_space_for_window_id(wid) == Some(space)
                && self
                    .layout_engine
                    .is_window_in_active_workspace(&self.state.windows, space, wid)
        };
        let is_visible_in_space = |wid: WindowId| {
            in_space(wid)
                && self
                    .state
                    .windows
                    .window(wid)
                    .and_then(|w| w.info.sys_id)
                    .is_some_and(|wsid| self.state.windows.is_window_visible(wsid))
        };
        let remembered =
            self.last_focused_window_in_space(space).filter(|wid| is_visible_in_space(*wid));
        let first_visible = self
            .layout_engine
            .windows_in_active_workspace(&self.state.windows, space)
            .into_iter()
            .find(|wid| is_visible_in_space(*wid));
        choose_switch_focus(
            preferred,
            preferred.is_some_and(in_space),
            remembered,
            first_visible,
        )
    }

    fn request_refocus_if_hidden(&mut self, space: SpaceId, window_id: WindowId) {
        if self.affinity().window_in_non_active_workspace(space, window_id) {
            self.refocus_manager.refocus_state = RefocusState::Pending(space);
        }
    }

    pub(super) fn prepare_refocus_after_layout_event(&mut self, event: &LayoutEvent) {
        match event {
            LayoutEvent::WindowAdded(space, wid) => {
                self.request_refocus_if_hidden(*space, *wid);
            }
            LayoutEvent::WindowsOnScreenUpdated(space, _, windows, _) => {
                let hidden_exists = windows
                    .iter()
                    .any(|entry| self.affinity().window_in_non_active_workspace(*space, entry.wid));
                if hidden_exists {
                    self.refocus_manager.refocus_state = RefocusState::Pending(*space);
                }
            }
            _ => {}
        }
    }

    // Returns true if the window should be raised on mouse over considering
    // active workspace membership and potential occlusion of floating windows above it.
    pub(crate) fn should_raise_on_mouse_over(&self, wid: WindowId) -> bool {
        let Some(window) = self.state.windows.window(wid) else {
            return false;
        };

        if !window.matches_filter(WindowFilter::EffectivelyManageable)
            && !self.layout_engine.is_window_floating(wid)
        {
            return false;
        }

        let candidate_frame = window.frame_monotonic;

        if matches!(self.menu_state, MenuState::Open(_)) {
            trace!(?wid, "Skipping autoraise while menu open");
            return false;
        }

        let Some(space) =
            self.affinity().best_space_for_window(&candidate_frame, window.info.sys_id)
        else {
            return false;
        };
        if !self.is_space_active(space) {
            return false;
        }

        if !self
            .layout_engine
            .is_window_in_active_workspace(&self.state.windows, space, wid)
        {
            trace!("Ignoring mouse over window {:?} - not in active workspace", wid);
            return false;
        }

        let Some(candidate_wsid) = window.info.sys_id else {
            return true;
        };

        // The stack from the top down to the candidate; only floating windows rini tracks count.
        let ws = &*self.reads.window_server;
        let order = ws.space_window_list_for_connection(&[space.get()]);
        let candidate_u32 = candidate_wsid.as_u32();
        let above = order.into_iter().take_while(|&id| id != candidate_u32).filter_map(|id| {
            let wsid = WindowServerId::new(id);
            let wid = self.state.windows.tracked_window_id(wsid)?;
            if !self.layout_engine.is_window_floating(wid) {
                return None;
            }
            let frame = self.state.windows.window(wid)?.frame_monotonic;
            Some(ws.stack_place(wsid, frame))
        });
        !covered_by_peer_above(ws.stack_place(candidate_wsid, candidate_frame), above)
    }

    pub(super) fn try_focus_or_warp_without_raise(
        &mut self,
        warp_space: Option<SpaceId>,
        focus_window: &mut Option<WindowId>,
    ) -> bool {
        if let Some(wid) = self.window_id_under_cursor() {
            *focus_window = Some(wid);
            return false;
        }
        if self.focus_untracked_window_under_cursor() {
            return true;
        }
        self.config.settings.mouse_follows_focus
            && warp_space.is_some_and(|space| self.warp_mouse_to_space_center(space))
    }

    pub fn warp_mouse(&mut self, point: CGPoint) {
        let Some(event_tap_tx) = self.communication_manager.event_tap_tx.clone() else {
            return;
        };
        event_tap_tx.send(crate::input::platform::input_tap::Request::Warp(point));
    }

    fn warp_mouse_to_space_center(&mut self, space: SpaceId) -> bool {
        let Some(screen) = self.space_state.screen_by_space(space) else {
            return false;
        };
        self.warp_mouse(screen.frame.mid());
        true
    }

    #[instrument(skip(self))]
    pub(super) fn clear_menu_state_for_pid(&mut self, pid: pid_t) {
        if matches!(self.menu_state, MenuState::Open(owner) if owner == pid) {
            debug!(pid, "Clearing menu-open state for deactivated app");
            self.menu_state = MenuState::Closed;
            self.update_focus_follows_mouse_state();
        }
    }

    pub(super) fn clear_menu_state_for_non_owner(&mut self, pid: pid_t) {
        if matches!(self.menu_state, MenuState::Open(owner) if owner != pid) {
            debug!(pid, "Clearing stale menu-open state after app focus changed");
            self.menu_state = MenuState::Closed;
            self.update_focus_follows_mouse_state();
        }
    }

    fn set_focus_follows_mouse_enabled(&self, enabled: bool) {
        if let Some(event_tap_tx) = self.communication_manager.event_tap_tx.as_ref() {
            event_tap_tx.send(input_tap::Request::SetFocusFollowsMouseEnabled(enabled));
        }
    }

    pub(super) fn update_focus_follows_mouse_state(&mut self) {
        let should_enable = self.config.settings.focus_follows_mouse
            && matches!(self.menu_state, MenuState::Closed)
            && !self.is_mission_control_active();
        self.set_focus_follows_mouse_enabled(should_enable);
    }
}

/// Which window a workspace switch focuses: the window the switch is for whenever it belongs to the
/// workspace (visible or not: it is parked until the layout lands), else the workspace's remembered
/// window if visible, else the first visible one.
pub(crate) fn choose_switch_focus<W: Copy>(
    preferred: Option<W>,
    preferred_in_workspace: bool,
    remembered_visible: Option<W>,
    first_visible: Option<W>,
) -> Option<W> {
    preferred
        .filter(|_| preferred_in_workspace)
        .or(remembered_visible)
        .or(first_visible)
}
