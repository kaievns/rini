//! One layout pass sorted into what moves, what stays, and what the overlay has to know. The
//! application gathers a [`PassWindow`] per window from its stores and applies the plan; nothing
//! here reads a store or sends a request.
use objc2_core_foundation::{CGPoint, CGRect};
use rini_geometry::{Round, SameAs};
use rini_windows::app_actor::Request;
use rini_core::ids::{WindowId, WindowServerId};
use rini_windows::transaction::TransactionId;

use crate::engine::AnimationRequest;
use crate::snapshot_service::SnapshotTarget;

/// A window as the layout pass sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct PassWindow {
    pub window: WindowId,
    pub server_id: Option<WindowServerId>,
    /// Where the window is, as last confirmed.
    pub current: CGRect,
    /// Where the layout puts it. Rounded by the plan.
    pub target: CGRect,
    /// A frame already on its way from an earlier pass.
    pub pending: Option<CGRect>,
    pub in_active_workspace: bool,
    pub floating: bool,
    /// Whether the owning app can still be told. A dead app's window still counts as changed.
    pub app_alive: bool,
}

/// One window this pass moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Move {
    pub window: WindowId,
    pub server_id: Option<WindowServerId>,
    pub from: CGRect,
    pub to: CGRect,
    pub in_active_workspace: bool,
    pub floating: bool,
}

impl Move {
    pub fn size_unchanged(&self) -> bool {
        self.from.size.same_as(self.to.size)
    }
}

#[derive(Debug, Default, Clone)]
pub struct PassPlan {
    /// In layout order. Visible moves fly; hidden ones are placed directly.
    pub moves: Vec<Move>,
    /// Windows on the active workspace the pass leaves where they are, `from == to`. The overlay is
    /// opaque and covers the display, so anything it omits vanishes for the length of the flight.
    /// See "The overlay has to draw everything it covers" in `docs/capture-overlay-research.md`.
    pub unmoved: Vec<AnimationRequest>,
    /// Every moving window with a server id, visible or not: the ones that slide in on a later
    /// switch are exactly the ones sitting off-strip now, and only capturable ahead of time.
    pub warm: Vec<SnapshotTarget>,
    /// Whether any window's frame changes, counting windows whose app is gone.
    pub any_frame_changed: bool,
}

impl PassPlan {
    /// What the overlay flies: the visible moves that have a server id to capture.
    pub fn overlay_requests(&self) -> Vec<AnimationRequest> {
        self.moves
            .iter()
            .filter(|m| m.in_active_workspace)
            .filter_map(|m| {
                Some(AnimationRequest {
                    window: m.window,
                    server_id: m.server_id?,
                    from: m.from,
                    to: m.to,
                    floating: m.floating,
                })
            })
            .collect()
    }
}

/// Sort a pass. A window whose rounded target equals its current frame, or equals a frame already
/// in flight, is unmoved; the rest move.
pub fn plan(windows: impl IntoIterator<Item = PassWindow>) -> PassPlan {
    let mut out = PassPlan::default();
    for w in windows {
        let to = w.target.round();
        let stays = to.same_as(w.current) || w.pending.is_some_and(|p| p.same_as(to));
        if stays {
            if w.in_active_workspace
                && let Some(server_id) = w.server_id
            {
                out.unmoved.push(AnimationRequest {
                    window: w.window,
                    server_id,
                    from: w.current,
                    to: w.current,
                    floating: w.floating,
                });
            }
            continue;
        }
        out.any_frame_changed = true;
        if !w.app_alive {
            continue;
        }
        if let Some(server_id) = w.server_id {
            out.warm.push(SnapshotTarget { window: w.window, server_id, size: to.size });
        }
        out.moves.push(Move {
            window: w.window,
            server_id: w.server_id,
            from: w.current,
            to,
            in_active_workspace: w.in_active_workspace,
            floating: w.floating,
        });
    }
    out
}

/// The requests that place one app's share of an instant pass. Position-only passes (a workspace
/// switch) send origins for the windows whose size is unchanged, so the app skips the resize.
pub fn instant_requests(
    frames: Vec<(WindowId, CGRect, bool)>,
    txid: TransactionId,
    position_only: bool,
) -> Vec<Request> {
    if !position_only {
        return vec![Request::SetBatchWindowFrame(
            frames.into_iter().map(|(wid, frame, _)| (wid, frame)).collect(),
            txid,
            true,
        )];
    }
    let mut positions: Vec<(WindowId, CGPoint)> = Vec::new();
    let mut full_frames: Vec<(WindowId, CGRect)> = Vec::new();
    for (wid, frame, size_unchanged) in frames {
        if size_unchanged {
            positions.push((wid, frame.origin));
        } else {
            full_frames.push((wid, frame));
        }
    }
    let mut requests = Vec::with_capacity(2);
    if !positions.is_empty() {
        requests.push(Request::SetWorkspaceSwitchPositions(positions, txid, true));
    }
    if !full_frames.is_empty() {
        requests.push(Request::SetBatchWindowFrame(full_frames, txid, true));
    }
    requests
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn window(idx: u32, current: CGRect, target: CGRect) -> PassWindow {
        PassWindow {
            window: WindowId::new(1, idx),
            server_id: Some(WindowServerId::new(idx)),
            current,
            target,
            pending: None,
            in_active_workspace: true,
            floating: false,
            app_alive: true,
        }
    }

    #[test]
    fn a_window_already_at_its_rounded_target_is_unmoved_and_still_drawn() {
        let at = rect(4.0, 32.0, 859.0, 1081.0);
        let plan = plan([window(1, at, rect(4.04, 31.96, 859.0, 1081.0))]);
        assert!(plan.moves.is_empty());
        assert!(!plan.any_frame_changed);
        assert_eq!(plan.unmoved.len(), 1);
        assert_eq!((plan.unmoved[0].from, plan.unmoved[0].to), (at, at));
        assert!(plan.overlay_requests().is_empty());
    }

    #[test]
    fn a_frame_already_in_flight_is_not_sent_again() {
        let from = rect(4.0, 32.0, 859.0, 1081.0);
        let to = rect(867.0, 32.0, 859.0, 1081.0);
        let mut w = window(1, from, to);
        w.pending = Some(to);
        let plan = plan([w]);
        assert!(plan.moves.is_empty());
        assert!(!plan.any_frame_changed);
        assert_eq!(plan.unmoved.len(), 1, "still covered by the overlay");
    }

    #[test]
    fn hidden_moves_warm_the_cache_but_do_not_fly() {
        let from = rect(4.0, 32.0, 859.0, 1081.0);
        let to = rect(1727.0, 1116.0, 859.0, 1081.0);
        let mut w = window(1, from, to);
        w.in_active_workspace = false;
        let plan = plan([w]);
        assert_eq!(plan.moves.len(), 1);
        assert!(plan.any_frame_changed);
        assert_eq!(plan.warm.len(), 1);
        assert!(plan.overlay_requests().is_empty());
        assert!(plan.unmoved.is_empty());
    }

    #[test]
    fn a_dead_apps_window_counts_as_changed_but_is_not_moved() {
        let mut w = window(1, rect(0.0, 0.0, 100.0, 100.0), rect(50.0, 0.0, 100.0, 100.0));
        w.app_alive = false;
        let plan = plan([w]);
        assert!(plan.any_frame_changed);
        assert!(plan.moves.is_empty());
        assert!(plan.warm.is_empty());
    }

    #[test]
    fn unmoved_windows_off_the_active_workspace_or_without_a_server_id_are_dropped() {
        let at = rect(0.0, 0.0, 100.0, 100.0);
        let mut off = window(1, at, at);
        off.in_active_workspace = false;
        let mut no_id = window(2, at, at);
        no_id.server_id = None;
        let plan = plan([off, no_id]);
        assert!(plan.unmoved.is_empty());
    }

    #[test]
    fn overlay_requests_keep_layout_order_and_carry_floating() {
        let a = window(1, rect(0.0, 0.0, 100.0, 100.0), rect(10.0, 0.0, 100.0, 100.0));
        let mut b = window(2, rect(0.0, 0.0, 100.0, 100.0), rect(20.0, 0.0, 100.0, 100.0));
        b.floating = true;
        let plan = plan([a, b]);
        let overlay = plan.overlay_requests();
        assert_eq!(overlay.len(), 2);
        assert_eq!(overlay[0].window, WindowId::new(1, 1));
        assert!(overlay[1].floating);
    }

    #[test]
    fn position_only_requests_split_moves_from_resizes() {
        let txid = TransactionId::default();
        let w = |i| WindowId::new(1, i);
        let frames = vec![(w(1), rect(0.0, 0.0, 10.0, 10.0), true), (w(2), rect(5.0, 0.0, 20.0, 10.0), false)];
        let requests = instant_requests(frames.clone(), txid, true);
        assert!(matches!(&requests[0], Request::SetWorkspaceSwitchPositions(p, _, true) if p == &vec![(w(1), CGPoint::new(0.0, 0.0))]));
        assert!(matches!(&requests[1], Request::SetBatchWindowFrame(f, _, true) if f.len() == 1 && f[0].0 == w(2)));
        let full = instant_requests(frames, txid, false);
        assert_eq!(full.len(), 1);
        assert!(matches!(&full[0], Request::SetBatchWindowFrame(f, _, true) if f.len() == 2));
    }
}
