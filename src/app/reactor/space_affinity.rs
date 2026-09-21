//! Which native space a window belongs to, resolved from the strongest source available: a live
//! window-server read, a frame rini is in the middle of sending, the workspace it is assigned
//! to, or its geometry. Read-only over the reactor's stores; the reactor hands out a view.
use objc2_core_foundation::{CGPoint, CGRect};
use rustc_hash::FxHashSet as HashSet;

use rini_core::ids::SpaceId;
use crate::displays::domain::topology::ForwardedSpaceState;
use rini_geometry::CGRectExt;
use rini_core::ids::{WindowId, WindowServerId};
use crate::windows::domain::state::WindowState;
use crate::windows::domain::transaction::TransactionManager;
use crate::windows::platform::window_server;
use crate::workspaces::{LayoutEngine, WindowStore};

pub(crate) struct SpaceAffinity<'a> {
    pub(crate) windows: &'a WindowStore,
    pub(crate) spaces: &'a ForwardedSpaceState,
    pub(crate) engine: &'a LayoutEngine,
    pub(crate) transactions: &'a TransactionManager,
    pub(crate) active_spaces: &'a HashSet<SpaceId>,
}

impl SpaceAffinity<'_> {
    pub(crate) fn is_space_active(&self, space: SpaceId) -> bool {
        self.active_spaces.contains(&space)
    }

    pub(crate) fn best_space_for_window(
        &self,
        frame: &CGRect,
        window_server_id: Option<WindowServerId>,
    ) -> Option<SpaceId> {
        if let Some(wsid) = window_server_id
            && self.is_known_fullscreen_window(wsid)
        {
            return None;
        }

        if let Some(wsid) = window_server_id {
            if let Some(space) = self.resolve_native_space(wsid, None) {
                return Some(space);
            }
        }

        if let Some(space) = self.hidden_assigned_space_for_frame(window_server_id, frame) {
            return Some(space);
        }

        self.best_space_for_frame(frame)
    }

    pub(crate) fn best_space_for_frame(&self, frame: &CGRect) -> Option<SpaceId> {
        let center = frame.mid();
        self.spaces.screen_for_point(center).and_then(|screen| screen.space).or_else(|| {
            self.spaces
                .screens
                .iter()
                .filter_map(|screen| {
                    let space = screen.space?;
                    let area = screen.frame.intersection(frame).area() as i64;
                    if area > 0 { Some((area, space)) } else { None }
                })
                .max_by_key(|(area, _)| *area)
                .map(|(_, space)| space)
        })
    }

    pub(crate) fn best_space_for_window_state(&self, window: &WindowState) -> Option<SpaceId> {
        self.best_space_for_window(&window.frame_monotonic, window.info.sys_id)
    }

    pub(crate) fn hidden_assigned_space_for_frame(
        &self,
        window_server_id: Option<WindowServerId>,
        _frame: &CGRect,
    ) -> Option<SpaceId> {
        let wsid = window_server_id?;
        let wid = self.windows.tracked_window_id(wsid)?;
        let assigned_space = self.assigned_space_for_window_id(wid)?;
        if !self.is_space_active(assigned_space)
            || !self.window_in_non_active_workspace(assigned_space, wid)
        {
            return None;
        }

        Some(assigned_space)
    }

    pub(crate) fn hidden_assigned_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        let window = self.windows.window(wid)?;
        self.hidden_assigned_space_for_frame(window.info.sys_id, &window.frame_monotonic)
    }

    pub(crate) fn assigned_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        self.engine
            .virtual_workspace_manager()
            .workspace_info_for_window_any(&self.windows, wid)
            .map(|info| info.space)
    }

    pub(crate) fn pending_target_space_for_window_server_id(&self, wsid: WindowServerId) -> Option<SpaceId> {
        let wid = self.windows.tracked_window_id(wsid)?;
        let target_frame = self.transactions.get_target_frame(wsid)?;
        let assigned_space = self.assigned_space_for_window_id(wid)?;
        let target_space = self
            .hidden_assigned_space_for_frame(Some(wsid), &target_frame)
            .or_else(|| self.best_space_for_frame(&target_frame))?;
        (target_space == assigned_space).then_some(target_space)
    }

    pub(crate) fn current_reported_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        self.windows
            .window(wid)
            .and_then(|window| window.info.sys_id)
            .and_then(|wsid| self.resolve_native_space(wsid, None))
    }

    pub(crate) fn authoritative_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        let reported_space = self.current_reported_space_for_window_id(wid);
        if let Some(hidden_assigned_space) = self.hidden_assigned_space_for_window_id(wid) {
            return match reported_space {
                Some(space) if space != hidden_assigned_space => Some(space),
                _ => Some(hidden_assigned_space),
            };
        }

        reported_space.or_else(|| self.assigned_space_for_window_id(wid))
    }

    /// Resolve native space ownership from the strongest available source.
    ///
    /// `observation` is a direct per-space membership observation. A pending
    /// Rini move wins over an observation that is not backed by the live
    /// WindowServer state, while a live conflict is treated as a newer external
    /// move. With no direct observation, the live WindowServer query wins over
    /// the accepted prior observation and the pending target wins over stale
    /// cached state.
    pub(crate) fn resolve_native_space(
        &self,
        wsid: WindowServerId,
        observation: Option<SpaceId>,
    ) -> Option<SpaceId> {
        let pending = self.pending_target_space_for_window_server_id(wsid);
        let live = window_server::window_space(wsid);
        let prior = self.windows.window_server_space(wsid);

        match (observation, pending) {
            (Some(observed), Some(target)) if observed != target => {
                if live == Some(observed) {
                    Some(observed)
                } else {
                    Some(target)
                }
            }
            (Some(observed), _) => Some(observed),
            (None, _) => live.or(pending).or(prior),
        }
    }

    pub(crate) fn best_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        self.authoritative_space_for_window_id(wid).or_else(|| {
            self.windows
                .window(wid)
                .and_then(|window| self.best_space_for_window_state(window))
        })
    }

    pub(crate) fn is_window_on_known_inactive_space(&self, wid: WindowId) -> bool {
        self.authoritative_space_for_window_id(wid)
            .is_some_and(|space| !self.is_space_active(space))
    }

    pub(crate) fn discovery_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        let window = self.windows.window(wid)?;
        let authoritative = self.authoritative_space_for_window_id(wid);
        if let Some(space) = authoritative {
            return Some(space);
        }

        if let Some(space) = self.best_space_for_frame(&window.frame_monotonic)
            && self.is_space_active(space)
        {
            return Some(space);
        }

        self.best_space_for_window_id(wid)
    }

    pub(crate) fn geometry_space_for_window(
        &self,
        frame: &CGRect,
        window_server_id: Option<WindowServerId>,
    ) -> Option<SpaceId> {
        if let Some(wsid) = window_server_id
            && self.is_known_fullscreen_window(wsid)
        {
            return None;
        }

        if let Some(space) = self.hidden_assigned_space_for_frame(window_server_id, frame) {
            return Some(space);
        }

        self.best_space_for_frame(frame)
    }

    pub(crate) fn is_known_fullscreen_window(&self, wsid: WindowServerId) -> bool {
        self.windows.is_window_server_id_native_fullscreen_suspended(wsid)
    }

    /// True when a window has been parked off-strip: it overlaps its screen by no
    /// more than the hidden sliver, so nothing meaningful of it is on display.
    ///
    /// Used to sort parked columns to the back of the raise order. Floating windows
    /// are never considered parked — they are positioned by the user, and a small
    /// window near a screen edge is not the same thing as a scrolled-away column.
    pub(crate) fn is_window_parked_offscreen(&self, wid: WindowId) -> bool {
        // Generous relative to the 1pt parking sliver: a parked window can sit a
        // fraction of a point inside after rounding, and a genuinely useful window
        // is never this close to invisible.
        const VISIBLE_SLACK: f64 = 4.0;

        if self.engine.is_window_floating(wid) {
            return false;
        }
        let Some(window) = self.windows.window(wid) else {
            return false;
        };
        let frame = window.frame_monotonic;
        let Some(screen) =
            self.spaces.screen_for_point(frame.mid()).map(|screen| screen.frame).or_else(|| {
                // A fully parked window's midpoint is outside every display, so fall
                // back to whichever screen its own space belongs to.
                self.best_space_for_window_id(wid).and_then(|space| {
                    self.spaces.screen_by_space(space).map(|screen| screen.frame)
                })
            })
        else {
            return false;
        };

        let visible_width =
            (frame.max().x.min(screen.max().x) - frame.origin.x.max(screen.origin.x)).max(0.0);
        visible_width <= VISIBLE_SLACK
    }

    pub(crate) fn window_center_on_known_screen(&self, wid: WindowId) -> Option<CGPoint> {
        let window_center = self.windows.window(wid)?.frame_monotonic.mid();
        self.spaces.screen_for_point(window_center).map(|_| window_center)
    }

    pub(crate) fn window_in_non_active_workspace(&self, space: SpaceId, window_id: WindowId) -> bool {
        let Some(active_workspace) = self.engine.active_workspace(space)
        else {
            return false;
        };
        self.engine
            .virtual_workspace_manager()
            .workspace_for_window(&self.windows, space, window_id)
            .is_some_and(|window_workspace| window_workspace != active_workspace)
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;
    use rini_core::ids::ScreenId;
    use crate::displays::screen::ScreenInfo;
    use crate::windows::domain::transaction::WindowTxStore;

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn spaces(frames: &[CGRect]) -> ForwardedSpaceState {
        ForwardedSpaceState {
            screens: frames
                .iter()
                .enumerate()
                .map(|(i, frame)| ScreenInfo {
                    id: ScreenId::new(i as u32 + 1),
                    frame: *frame,
                    display_uuid: format!("uuid-{i}"),
                    name: None,
                    space: Some(SpaceId::new(i as u64 + 1)),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn with_view<R>(spaces: &ForwardedSpaceState, f: impl FnOnce(SpaceAffinity<'_>) -> R) -> R {
        let windows = WindowStore::default();
        let engine = LayoutEngine::new(
            &crate::app::config::VirtualWorkspaceSettings::default(),
            &crate::app::config::LayoutSettings::default(),
            None,
        );
        let transactions = TransactionManager::new(WindowTxStore::new());
        let active_spaces: HashSet<SpaceId> = spaces.iter_known_spaces().collect();
        f(SpaceAffinity {
            windows: &windows,
            spaces,
            engine: &engine,
            transactions: &transactions,
            active_spaces: &active_spaces,
        })
    }

    #[test]
    fn a_frame_belongs_to_the_screen_under_its_centre() {
        let s = spaces(&[rect(0., 0., 1000., 1000.), rect(1000., 0., 1000., 1000.)]);
        with_view(&s, |view| {
            assert_eq!(view.best_space_for_frame(&rect(900., 0., 400., 400.)), Some(SpaceId::new(2)));
            assert_eq!(view.best_space_for_frame(&rect(100., 0., 400., 400.)), Some(SpaceId::new(1)));
        });
    }

    #[test]
    fn a_frame_whose_centre_is_off_every_screen_goes_to_the_largest_overlap() {
        let s = spaces(&[rect(0., 0., 1000., 1000.), rect(1000., 0., 1000., 1000.)]);
        with_view(&s, |view| {
            // Centre at y = 1200, below both screens; 300pt of it hangs into screen 2.
            assert_eq!(view.best_space_for_frame(&rect(1100., 700., 400., 1000.)), Some(SpaceId::new(2)));
            assert_eq!(view.best_space_for_frame(&rect(5000., 5000., 10., 10.)), None);
        });
    }

    #[test]
    fn an_untracked_window_resolves_by_geometry_only() {
        let s = spaces(&[rect(0., 0., 1000., 1000.)]);
        with_view(&s, |view| {
            let frame = rect(100., 100., 200., 200.);
            assert_eq!(view.best_space_for_window(&frame, None), Some(SpaceId::new(1)));
            assert_eq!(view.geometry_space_for_window(&frame, None), Some(SpaceId::new(1)));
            assert_eq!(view.best_space_for_window_id(WindowId::new(1, 1)), None);
        });
    }
}
