//! One layout pass sorted into what moves, what stays, and what the overlay has to know. The
//! application gathers a [`PassWindow`] per window from its stores and applies the plan; nothing
//! here reads a store or sends a request.
use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::ScreenRect;
use rini_geometry::{Round, SameAs};

use crate::decorations::domain::style::Worn;
use crate::pictures::domain::target::PictureTarget;
use crate::transitions::domain::request::AnimationRequest;

/// A window as the layout pass sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct PassWindow {
    pub window: WindowId,
    pub server_id: Option<WindowServerId>,
    /// Where the window is, as last confirmed.
    pub current: ScreenRect,
    /// Where the layout puts it. Rounded by the plan.
    pub target: ScreenRect,
    /// A frame already on its way from an earlier pass.
    pub pending: Option<ScreenRect>,
    pub in_active_workspace: bool,
    pub floating: bool,
    /// Whether the owning app can still be told. A dead app's window still counts as changed.
    pub app_alive: bool,
    pub worn: Worn,
}

/// One window this pass moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Move {
    pub window: WindowId,
    pub server_id: Option<WindowServerId>,
    pub from: ScreenRect,
    pub to: ScreenRect,
    pub in_active_workspace: bool,
    pub floating: bool,
    pub worn: Worn,
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
    /// See "The overlay has to draw everything it covers" in `src/animation/docs/capture-overlay-research.md`.
    pub unmoved: Vec<AnimationRequest>,
    /// Every moving window with a server id, visible or not: the ones that slide in on a later
    /// switch are exactly the ones sitting off-strip now, and only capturable ahead of time.
    pub warm: Vec<PictureTarget>,
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
                    worn: m.worn,
                    strip_before: None,
                    strip_after: None,
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
        let to = w.target.rounded();
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
                    worn: w.worn,
                    strip_before: None,
                    strip_after: None,
                });
            }
            continue;
        }
        out.any_frame_changed = true;
        if !w.app_alive {
            continue;
        }
        if let Some(server_id) = w.server_id {
            out.warm.push(PictureTarget {
                window: w.window,
                server_id,
                size: to.size,
            });
        }
        out.moves.push(Move {
            window: w.window,
            server_id: w.server_id,
            from: w.current,
            to,
            in_active_workspace: w.in_active_workspace,
            floating: w.floating,
            worn: w.worn,
        });
    }
    out
}

/// Gives each tiled request its place on the strip before and after the pass, from the strip's
/// layouts, rounded as `plan` rounds a `to` so a parked column moves with its on-screen neighbours
/// to the point. A floating window gets none: the strip layout holds only where it was stored.
pub fn place_on_strip(
    requests: &mut [AnimationRequest],
    before: &[(WindowId, ScreenRect)],
    after: &[(WindowId, ScreenRect)],
) {
    let place = |layout: &[(WindowId, ScreenRect)], window: WindowId| {
        layout.iter().find(|(w, _)| *w == window).map(|(_, frame)| frame.rounded())
    };
    for request in requests.iter_mut().filter(|request| !request.floating) {
        request.strip_before = place(before, request.window);
        request.strip_after = place(after, request.window);
    }
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Point, Size};

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> ScreenRect {
        ScreenRect::new(Point::new(x, y), Size::new(w, h))
    }

    fn window(idx: u32, current: ScreenRect, target: ScreenRect) -> PassWindow {
        PassWindow {
            worn: Default::default(),
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

    /// Each tiled request is given where the strip had its window before the pass and has it after,
    /// rounded as the pass rounds a `to`, so a parked column moves with its on-screen neighbours to
    /// the point; a floating one, or one a layout does not hold, gets none.
    #[test]
    fn a_tiled_request_is_given_its_place_on_the_strip_and_a_floating_one_none() {
        let at = rect(4.0, 32.0, 859.0, 1081.0);
        let tiled = window(1, at, at);
        let mut floating = window(2, at, at);
        floating.floating = true;
        let new = window(3, at, at);
        let mut requests = plan([tiled, floating, new]).unmoved;
        let before = [
            (WindowId::new(1, 1), rect(-859.4, 32.0, 859.0, 1081.0)),
            (WindowId::new(1, 2), rect(100.0, 100.0, 400.0, 300.0)),
        ];
        let after = [
            (WindowId::new(1, 1), rect(4.0, 32.0, 859.0, 1081.0)),
            (WindowId::new(1, 2), rect(100.0, 100.0, 400.0, 300.0)),
            (WindowId::new(1, 3), rect(1729.5, 32.0, 859.0, 1081.0)),
        ];
        place_on_strip(&mut requests, &before, &after);
        let places: Vec<_> = requests.iter().map(|r| (r.strip_before, r.strip_after)).collect();
        assert_eq!(
            places,
            vec![
                (Some(rect(-859.0, 32.0, 859.0, 1081.0)), Some(rect(4.0, 32.0, 859.0, 1081.0))),
                (None, None),
                (None, Some(rect(1730.0, 32.0, 859.0, 1081.0))),
            ]
        );
    }
}
