//! A window being dragged: the drag session, and the swap with the window it is dragged over.

use objc2_core_foundation::CGRect;
use rini_core::ids::{SpaceId, WindowId};
use tracing::trace;

use super::{DragSession, DragState, Reactor};

impl Reactor {
    pub(super) fn is_in_drag(&self) -> bool {
        matches!(
            self.drag_manager.drag_state,
            DragState::Active { .. } | DragState::PendingSwap { .. }
        )
    }

    pub(super) fn get_pending_drag_swap(&self) -> Option<(WindowId, WindowId)> {
        if let DragState::PendingSwap { session, target } = &self.drag_manager.drag_state {
            Some((session.window, *target))
        } else {
            None
        }
    }

    fn get_active_drag_session(&self) -> Option<&DragSession> {
        if let DragState::Active { session } = &self.drag_manager.drag_state {
            Some(session)
        } else {
            None
        }
    }

    fn take_active_drag_session(&mut self) -> Option<DragSession> {
        match std::mem::replace(&mut self.drag_manager.drag_state, DragState::Inactive) {
            DragState::Active { session } => Some(session),
            DragState::PendingSwap { session, .. } => Some(session),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn ensure_active_drag(&mut self, wid: WindowId, frame: &CGRect) {
        let needs_new_session =
            self.get_active_drag_session().is_none_or(|session| session.window != wid);
        if needs_new_session {
            let server_id = self.state.windows.window(wid).and_then(|window| window.info.sys_id);
            let origin_space = self.affinity().best_space_for_window(frame, server_id);
            self.drag_manager.drag_state = DragState::Active {
                session: DragSession {
                    window: wid,
                    start_frame: *frame,
                    dragged_end: None,
                    last_frame: *frame,
                    origin_space,
                    settled_space: origin_space,
                    layout_dirty: false,
                },
            };
        }
        self.drag_manager.skip_layout_for_window = Some(wid);
    }

    fn collect_drag_swap_candidates(
        &self,
        wid: WindowId,
        space: SpaceId,
    ) -> Vec<(WindowId, CGRect)> {
        self.state
            .windows
            .iter_windows()
            .filter_map(|(other_wid, other_state)| {
                if other_wid == wid {
                    return None;
                }
                let other_space = self.affinity().best_space_for_window_state(other_state)?;
                if other_space != space
                    || !self.layout_engine.is_window_in_active_workspace(
                        &self.state.windows,
                        space,
                        other_wid,
                    )
                    || self.layout_engine.is_window_floating(other_wid)
                {
                    return None;
                }
                Some((other_wid, other_state.frame_monotonic))
            })
            .collect()
    }

    pub(super) fn maybe_swap_on_drag(&mut self, wid: WindowId, new_frame: CGRect) {
        if !self.is_in_drag() {
            trace!(?wid, "Skipping swap: not in drag (mouse up received)");
            return;
        }

        // A floating window has no swap semantics: it is not in the tiling strip, so there
        // is nothing for it to trade places with. Returning before `track_swap_target` is the
        // point — its fall-through clears
        // `skip_layout_for_window`, and clearing it MID-GESTURE lets the next layout pass
        // reassert the window's stored frame while the user is still holding the mouse.
        //
        // Measured on System Settings: the reported `old_frame` rewound repeatedly during a
        // single drag (695,188 -> 832,167 -> 927,146, then back to 350,212), because rini
        // kept writing the stale stored position underneath the drag. The window ended up
        // wherever the last tug-of-war left it, which reads as snapping back part of the
        // way — roughly a third of the distance, in the reported case.
        if self.layout_engine.is_window_floating(wid) {
            trace!(
                ?wid,
                "Skipping swap: floating windows do not participate in strip swaps"
            );
            return;
        }

        let Some((space, origin_space_hint)) = self.drag_spaces(wid, new_frame) else {
            return;
        };
        if let Some(origin_space) = origin_space_hint
            && origin_space != space
        {
            self.reset_swap_after_space_change(wid, origin_space, space);
            return;
        }
        if !self
            .layout_engine
            .is_window_in_active_workspace(&self.state.windows, space, wid)
        {
            return;
        }

        self.track_swap_target(wid, new_frame, space);
    }

    /// The space a dragged window is over, the drag session's settled one first, and the space it
    /// started from.
    fn drag_spaces(&self, wid: WindowId, new_frame: CGRect) -> Option<(SpaceId, Option<SpaceId>)> {
        let server_id = self.state.windows.window(wid)?.info.sys_id;
        let space = self
            .get_active_drag_session()
            .and_then(|session| session.settled_space)
            .or_else(|| self.affinity().best_space_for_window(&new_frame, server_id))?;

        let origin_space_hint = self
            .get_active_drag_session()
            .and_then(|session| session.origin_space)
            .or_else(|| {
                self.drag_manager
                    .origin_frame()
                    .and_then(|frame| self.affinity().best_space_for_window(&frame, server_id))
            });

        Some((space, origin_space_hint))
    }

    /// A dragged window entering another space than the one it started from: its pending swap
    /// dropped and the swap tracking reset.
    fn reset_swap_after_space_change(
        &mut self,
        wid: WindowId,
        origin_space: SpaceId,
        space: SpaceId,
    ) {
        if let Some((pending_wid, pending_target)) = self.get_pending_drag_swap()
            && pending_wid == wid
        {
            trace!(
                ?wid,
                ?pending_target,
                ?origin_space,
                ?space,
                "Clearing pending drag swap; dragged window entered new space"
            );
            self.drag_manager.drag_state = DragState::Inactive;
        }
        trace!(
            ?wid,
            ?origin_space,
            ?space,
            "Resetting drag swap tracking after space change"
        );
        self.drag_manager.drag_swap.reset();
    }

    /// The swap target the dragged window is over, deferred to the mouse-up as a pending swap, or
    /// a pending swap dropped once the overlap ends.
    fn track_swap_target(&mut self, wid: WindowId, new_frame: CGRect, space: SpaceId) {
        let candidates = self.collect_drag_swap_candidates(wid, space);

        let previous_pending = self.get_pending_drag_swap();
        let new_candidate =
            self.drag_manager.drag_swap.on_frame_change(wid, new_frame, &candidates);
        let active_target = self.drag_manager.drag_swap.last_target();
        if let Some(target_wid) = active_target {
            if new_candidate.is_some() || previous_pending != Some((wid, target_wid)) {
                trace!(
                    ?wid,
                    ?target_wid,
                    "Detected swap candidate; deferring until MouseUp"
                );
            }

            if let Some(session) = self.take_active_drag_session() {
                self.drag_manager.drag_state =
                    DragState::PendingSwap { session, target: target_wid };
            } else {
                trace!(
                    ?wid,
                    ?target_wid,
                    "Skipping pending swap; no active drag session"
                );
                self.drag_manager.drag_state = DragState::Inactive;
                self.drag_manager.skip_layout_for_window = None;
                return;
            }

            self.drag_manager.skip_layout_for_window = Some(wid);
            return;
        }

        if let Some((pending_wid, pending_target)) = previous_pending
            && pending_wid == wid
        {
            trace!(
                ?wid,
                ?pending_target,
                "Clearing pending drag swap; overlap ended before MouseUp"
            );
            if let Some(session) = self.take_active_drag_session() {
                self.drag_manager.drag_state = DragState::Active { session };
            } else {
                self.drag_manager.drag_state = DragState::Inactive;
            }
        }

        if self.drag_manager.skip_layout_for_window == Some(wid) {
            self.drag_manager.skip_layout_for_window = None;
        }
        // wait for mouse::up before doing *anything*
    }
}
