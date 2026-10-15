//! The strip kept one group in front of the floating windows after focus lands on it, and every
//! raise sent with the focus reports it will provoke remembered.

use rini_core::ids::{SpaceId, WindowId};
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, warn};

use super::Reactor;
use crate::windows::domain::raise::{self as raise_manager, RaiseRequest};
use crate::windows::domain::raise_order::{self, StackGroup, regroup_tiled};
use crate::windows::domain::request::Quiet;

impl Reactor {
    /// Which z-order group a window belongs to.
    fn stack_group_of(&self, window: WindowId) -> StackGroup {
        StackGroup::of(self.layout_manager.layout_engine.is_window_floating(window))
    }

    /// Puts the strip back in front of the windows that are not on it, after focus lands on the strip.
    ///
    /// macOS raises the one window that was clicked, so clicking one half of a 50/50 pair lifts it over a
    /// floating window and leaves the other half behind it. The strip is one surface and has to be one
    /// group. Empty when the order already obeys the rule, which is the common case and costs nothing:
    /// putting it back costs an Accessibility raise per strip window in the workspace.
    ///
    /// Only the on-screen windows are judged and raised. Raising the whole strip was tried and measured:
    /// 19 windows across 10 apps take seconds (one activation wait per app, serialized), every raise
    /// echoes as a focus report, and echoes past the 400ms `RaiseEcho` window read as the user moving,
    /// so the strip scrolled to random parked windows and regrouped again, 13 rounds per press. A parked
    /// column's place cannot be seen; it is judged again by `regroup_after_layout` once a layout pass
    /// brings it on screen, since nothing else raises a column that scrolls back into view.
    pub(super) fn strip_group_to_lift(
        &mut self,
        space: SpaceId,
        focused: WindowId,
    ) -> Vec<WindowId> {
        let depths = self.reads.window_server.front_to_back_depths();
        let mut order: Vec<StackedWindow> = self
            .layout_manager
            .layout_engine
            .windows_in_active_workspace(&self.state.windows, space)
            .into_iter()
            .filter(|wid| !self.affinity().is_window_parked_offscreen(*wid))
            .filter_map(|wid| {
                let server_id = self.state.windows.window(wid)?.info.sys_id?;
                let depth = depths.get(&server_id.as_u32()).copied()?;
                Some(StackedWindow {
                    window: wid,
                    depth,
                    group: self.stack_group_of(wid),
                })
            })
            .collect();
        order.sort_by_key(|stacked| stacked.depth);

        let to_raise = strip_group_to_lift_for(&order, focused, self.stack_group_of(focused));
        debug!(
            focused = focused.idx.get(),
            order = ?order.iter().map(|s| (s.window.idx.get(), s.group)).collect::<Vec<_>>(),
            lifting = to_raise.len(),
            "strip regroup judged"
        );
        to_raise
    }

    /// The regroup for a focus that has landed and a layout that has been applied: the strip windows now
    /// on screen are lifted over any floating window in front of one of them, the focused window last.
    ///
    /// Two cases end here. A click is raised by macOS, not by rini, and produces only a focus report,
    /// so nothing else in that path fixes the order. A focus move raises before the strip scrolls
    /// (`handle_layout_response`), so a column that was parked behind a floating window comes into view
    /// behind it and the floating window sits between it and the focused column; `animate_layout` has
    /// set every moved window's `frame_monotonic` by now, so the on-screen set is the post-layout one
    /// while the window server's order is still the old one. Skipped when every window it would raise
    /// was already raised by this event's earlier regroup: that raise is still in flight and the order
    /// it will produce is the one wanted.
    pub(super) fn regroup_after_layout(&mut self) {
        let already = std::mem::take(&mut self.regroup_raised);
        let Some(focused) = self.layout_manager.layout_engine.focused_window() else {
            return;
        };
        // The layout's focus is stale when the window that really has focus is one the layout
        // does not hold: Zoom's meeting toolbar (not manageable) took focus when the user
        // switched to the call, the layout kept Kiro as focused, and the regroup lifted the strip
        // over the call and re-focused Kiro. Nothing to put right in that case: macOS ordered
        // the windows for the app the user chose.
        if self.main_window().is_some_and(|front| front != focused) {
            debug!(
                focused = focused.idx.get(),
                front = ?self.main_window().map(|w| w.idx.get()),
                "regroup skipped: focus is on a window outside the layout"
            );
            return;
        }
        let Some(space) = self.affinity().best_space_for_window_id(focused) else {
            return;
        };
        let to_raise = self.strip_group_to_lift(space, focused);
        if to_raise.is_empty() || to_raise.iter().all(|wid| already.contains(wid)) {
            return;
        }
        if let Some(request) = self.regroup_raise(&to_raise, focused) {
            debug!(
                focused = focused.idx.get(),
                windows = to_raise.len(),
                "a floating window was in front of the strip; lifting the whole strip over it"
            );
            self.dispatch_raise(request);
        }
    }

    /// One quiet raise of `to_raise` in order, focusing `focused` (which is expected to be last).
    fn regroup_raise(
        &mut self,
        to_raise: &[WindowId],
        focused: WindowId,
    ) -> Option<raise_manager::Event> {
        let mut app_handles = HashMap::default();
        for wid in to_raise {
            self.insert_app_handle_for_window(&mut app_handles, *wid);
        }
        let batches = raise_order::group_by_app_and_space(to_raise, |wid| {
            self.affinity().best_space_for_window_id(wid)
        });
        if batches.is_empty() {
            return None;
        }
        self.insert_app_handle_for_window(&mut app_handles, focused);
        self.regroup_raised = to_raise.to_vec();
        Some(raise_manager::Event::RaiseRequest(RaiseRequest {
            raise_windows: batches,
            focus_window: Some((focused, None)),
            app_handles,
            // rini's own doing, so the activation it provokes must not be read as the user moving.
            focus_quiet: Quiet::Yes,
        }))
    }

    /// Sends a raise request, remembering the focus reports it is about to provoke.
    ///
    /// The single place raises leave the reactor, so nothing can issue one without its echo being recorded.
    /// See [`crate::windows::domain::focus::RaiseEcho`].
    pub(super) fn dispatch_raise(&mut self, request: raise_manager::Event) {
        if let raise_manager::Event::RaiseRequest(details) = &request {
            let touched: Vec<WindowId> = details
                .raise_windows
                .iter()
                .flat_map(|batch| batch.windows().iter().copied())
                .collect();
            self.raise_echo.expect(
                touched.into_iter(),
                details.focus_window.map(|(wid, _)| wid),
                std::time::Instant::now(),
            );
        }
        if let Err(error) = self.communication_manager.raise_manager_tx.try_send(request) {
            warn!(%error, "failed to send raise request");
        }
    }
}

/// One on-screen window of the active workspace as the window server stacks it, for the strip regroup.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct StackedWindow {
    pub(crate) window: WindowId,
    /// Front-to-back position, 0 being frontmost.
    pub(crate) depth: usize,
    pub(crate) group: StackGroup,
}

/// The windows to raise so the strip is one group in front of the floating windows, in raise order:
/// back to front, the focused window last. Empty when a floating window takes focus (macOS already
/// put it in front, which is the rule) or when the order already obeys the rule.
///
/// `order` is front to back, as `front_to_back_depths` reports it. The focused window goes last
/// whatever its depth: a raise sequence ends with the window that has to be frontmost, and after a
/// keyboard move the target has not been raised yet.
pub(crate) fn strip_group_to_lift_for(
    order: &[StackedWindow],
    focused: WindowId,
    focused_group: StackGroup,
) -> Vec<WindowId> {
    if focused_group == StackGroup::Floating {
        return Vec::new();
    }
    let groups: Vec<(WindowId, StackGroup)> = order.iter().map(|s| (s.window, s.group)).collect();
    let back_to_front = regroup_tiled(&groups);
    let mut raise: Vec<WindowId> =
        back_to_front.iter().copied().filter(|wid| *wid != focused).collect();
    if back_to_front.contains(&focused) {
        raise.push(focused);
    }
    raise
}
