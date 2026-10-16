//! The layout side of animation: whether each pass flies on the overlay, and the real windows
//! placed when it does not. The sorting of a pass is `crate::transitions::domain::pass`; this
//! applies the plan to the stores and the apps.
use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{SpaceId, WindowId, pid_t};
use rini_geometry::space::{FromCg, ScreenRect, ScreenSize, ToCg};
use rini_geometry::stretch::is_a_resize;
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, trace};

use crate::app::reactor::Reactor;
use crate::app::reactor::present::{Present, instant_requests};
use crate::app::reactor::strip_motion::{StripLayout, StripPan};
use crate::pictures::domain::schedule::{Ask, Priority};
use crate::pictures::domain::target::PictureTarget;
use crate::transitions::domain::pass::{self, Move, PassPlan, PassWindow};
use crate::transitions::domain::request::AnimationRequest;
use crate::transitions::domain::travel::changes_visibly;
use crate::windows::domain::request::{AppThreadHandle, Request};
use crate::windows::domain::transaction::TransactionId;

/// A committed move's frame for its window, and the app thread that writes it.
struct Placement {
    handle: AppThreadHandle,
    window: WindowId,
    frame: CGRect,
    txid: TransactionId,
}

impl Placement {
    fn request(&self) -> Request {
        Request::SetWindowFrame {
            window: self.window,
            frame: self.frame,
            txid: self.txid,
        }
    }
}

impl Reactor {
    /// One `PassWindow` per window the pass touches, read from the reactor's stores.
    fn pass_windows(
        &self,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
        app_liveness_matters: bool,
    ) -> Vec<PassWindow> {
        let engine = &self.layout_engine;
        let windows = &self.state.windows;
        let rings = self.layout_rings();
        // An unassigned window is not on the active workspace: it must not fly.
        let active_ws = engine.active_workspace(space);
        let in_active = |wid| {
            active_ws.is_some()
                && engine.virtual_workspace_manager().workspace_for_window(windows, space, wid)
                    == active_ws
        };
        layout
            .iter()
            .filter_map(|&(wid, target)| {
                if skip_wid == Some(wid) {
                    trace!(?wid, "Skipping layout update for window currently being dragged");
                    return None;
                }
                let Some(window) = windows.window(wid) else {
                    debug!(?wid, "Skipping layout - window no longer exists");
                    return None;
                };
                let server_id = window.info.sys_id;
                let app_alive = !app_liveness_matters || self.apps.contains_key(&wid.pid);
                if app_liveness_matters && !app_alive {
                    debug!(?wid, "Skipping for window - app no longer exists");
                }
                Some(PassWindow {
                    window: wid,
                    server_id,
                    current: ScreenRect::from_cg(window.frame_monotonic),
                    target: ScreenRect::from_cg(target),
                    pending: server_id
                        .and_then(|wsid| self.transactions.target(&wsid))
                        .map(ScreenRect::from_cg),
                    in_active_workspace: in_active(wid),
                    floating: engine.is_window_floating(wid),
                    app_alive,
                    worn: self.worn(wid, &rings),
                })
            })
            .collect()
    }

    /// Records a move in the window store and the transaction table; returns its transaction id.
    ///
    /// The narrow borrows are `present::Present`, which is also where the batching rule and its
    /// tests live.
    fn commit_move(&mut self, m: &Move) -> crate::windows::domain::transaction::TransactionId {
        Present::new(&mut self.state.windows, &self.transactions).commit(m.window, m.to.to_cg())
    }

    pub(super) fn animate_layout(
        &mut self,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        is_resize: bool,
        skip_wid: Option<WindowId>,
    ) -> bool {
        let Some(active_ws) = self.layout_engine.active_workspace(space) else {
            return false;
        };
        self.ask_pictures_of_first_laid_out(layout);
        let plan = pass::plan(self.pass_windows(space, layout, skip_wid, true));
        // A strip scroll moves every window by the SAME vector: a viewport pan over the one
        // workspace, the horizontal twin of the vertical workspace switch. The strip's own scroll
        // offset says how far, once per press; reading it off the windows answered differently on
        // each of the several passes one keystroke produces. Taken on every pass, placing or not, so
        // the offset and the strip's layout stay current; the move with it, so it reaches one pass
        // only.
        let strip_movement = self.take_strip_movement(space);
        let StripLayout { before, now: strip_now } =
            self.take_strip_layout(space).unwrap_or_default();
        let strip_before = before.unwrap_or_default();
        let moved = self.take_move_in(layout);

        let placements = self.commit_pass_moves(&plan.moves);
        if placements.is_empty() {
            return plan.any_frame_changed;
        }

        let mut overlay_requests = plan.overlay_requests();
        let PassPlan {
            mut unmoved,
            warm,
            any_frame_changed,
            ..
        } = plan;
        place_on_strip(
            &mut [&mut overlay_requests, &mut unmoved],
            &strip_before,
            &strip_now,
        );
        let low_power = (self.reads.low_power)();
        let layout_animate = self.config.settings.animate;
        // `is_resize` means a window REPORTED a size change, which happens both when the user is
        // dragging an edge and when rini resized it for a command. Only a real interactive drag
        // skips; animating a command's resize is what keeps the neighbour column and the resized
        // window arriving together.
        //
        // A change nobody can see is not worth covering the display for. Guarded on the list being
        // non-empty, because a window with no window server id never reaches it and an empty list
        // must not read as "nothing is changing".
        let unchanged = !overlay_requests.is_empty() && !changes_visibly(&overlay_requests);
        let skip_anim =
            (is_resize && self.is_in_drag()) || !layout_animate || low_power || unchanged;
        if unchanged {
            debug!(
                windows = overlay_requests.len(),
                "no window moves or resizes far enough to see; placing rather than animating"
            );
        }
        // Added after the change test above, which must judge only the windows going somewhere.
        overlay_requests.extend(unmoved);

        // The overlay engine replaces the per-frame Accessibility writes entirely: the real windows
        // are placed once, and the motion the eye follows is drawn in the overlay. It is asked to
        // animate FIRST, and its first frame draws the windows where they are leaving from, so the
        // picture stays continuous even if the real windows land before the overlay is visible.
        // Resizes ride the overlay too, anchored and cropped rather than stretched. See "Resizes
        // through the overlay" in `src/animation/docs/animation-smoothness.md`.
        let any_resize = overlay_requests
            .iter()
            .any(|request| is_a_resize(request.from.size, request.to.size));
        let use_overlay = !skip_anim
            && !overlay_requests.is_empty()
            && self.communication_manager.transitions_tx.is_some();

        let moved = moved.filter(|_| !any_resize);
        let change = if any_resize {
            PassChange::Resize
        } else if moved.is_some() {
            PassChange::Move
        } else {
            PassChange::Scroll
        };
        let pan_delta = pan_delta(change, strip_movement);
        let moves: Vec<(WindowId, CGRect)> =
            placements.iter().map(|placement| (placement.window, placement.frame)).collect();
        if use_overlay
            && let Some(delta) = pan_delta
            && self.start_strip_pan(space, active_ws, StripPan {
                final_frames: moves,
                delta,
                moved,
                standing: &overlay_requests,
                laid_out: &strip_before,
            })
        {
            // The strip movement owns it, including placing the real windows once it covers them.
        } else if use_overlay {
            self.fly_on_overlay(space, overlay_requests);
        }

        // Warm whether or not this pass animated.
        self.ask_warm(warm);

        if !use_overlay {
            // Nothing flies: the windows go straight to their frames. When the overlay flies, the
            // real windows are placed by Event::ApplyOverlayFrames once it covers them; placing them
            // now let them visibly jump into position before the overlay appeared.
            for placement in placements {
                _ = placement.handle.send(placement.request());
            }
        }

        any_frame_changed
    }

    /// Pictures asked for, once, of each window a pass lays out for the first time.
    ///
    /// A pass only asks for pictures of the windows it moves, and after a restart nothing moves:
    /// every window is where the saved layout left it. So each window is asked for once, the first
    /// time a pass lays it out, whichever workspace it is on. Per window rather than per space:
    /// at startup apps are found one by one, and the first pass knew one window of 21.
    fn ask_pictures_of_first_laid_out(&mut self, layout: &[(WindowId, CGRect)]) {
        self.pictured_windows
            .retain(|window| self.state.windows.window(*window).is_some());
        let first_laid_out: Vec<PictureTarget> = layout
            .iter()
            .filter(|(window, _)| !self.pictured_windows.contains(window))
            .filter_map(|&(window, frame)| {
                Some(PictureTarget {
                    window,
                    server_id: self.state.windows.window(window)?.info.sys_id?,
                    size: ScreenSize::from_cg(frame.size),
                })
            })
            .collect();
        if !first_laid_out.is_empty() && self.communication_manager.pictures.is_some() {
            self.pictured_windows.extend(first_laid_out.iter().map(|target| target.window));
            self.ask_warm(first_laid_out);
        }
    }

    fn ask_warm(&self, targets: Vec<PictureTarget>) {
        if !targets.is_empty()
            && let Some(pictures) = &self.communication_manager.pictures
        {
            pictures.ask(Ask {
                priority: Priority::Warm,
                targets,
            });
        }
    }

    /// Each of a pass's moves committed; the visible ones returned to wait for the flight
    /// decision, the hidden ones sent straight to their app.
    fn commit_pass_moves(&mut self, moves: &[Move]) -> Vec<Placement> {
        let mut placements = Vec::new();
        for m in moves {
            let txid = self.commit_move(m);
            let Some(handle) = self.apps.get(&m.window.pid).map(|a| a.handle.clone()) else {
                continue;
            };
            let placement = Placement {
                handle,
                window: m.window,
                frame: m.to.to_cg(),
                txid,
            };
            if m.in_active_workspace {
                trace!(window = ?m.window, from = ?m.from, to = ?m.to, "Animating visible window");
                placements.push(placement);
            } else {
                trace!(window = ?m.window, from = ?m.from, to = ?m.to, "Direct positioning hidden window");
                if let Err(e) = placement.handle.send(placement.request()) {
                    debug!(window = ?m.window, ?e, "Failed to send frame request for hidden window");
                }
            }
        }
        placements
    }

    /// The pass handed to the transitions engine as a flight over the overlay.
    fn fly_on_overlay(&mut self, space: SpaceId, overlay_requests: Vec<AnimationRequest>) {
        let duration =
            std::time::Duration::from_secs_f64(self.config.settings.animation_duration.max(0.0));
        let count = overlay_requests.len();
        let flight = crate::transitions::platform::engine::Event::Animate {
            windows: overlay_requests,
            focus: self.layout_engine.focused_window(),
            focused_before: self.focused_as_event_began,
            key: self.main_window_tracker.key_window(),
            duration,
            look: crate::animation::domain::choreography::Look::default(),
            cause: self.cause,
        };
        if self.send_flight(space, flight) {
            trace!(count, "handed the layout to the overlay animation engine");
        }
    }

    pub(super) fn instant_layout(
        &mut self,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
    ) -> bool {
        self.instant_layout_inner(space, layout, skip_wid, false)
    }

    /// Slide a workspace switch vertically instead of cutting to it.
    ///
    /// Workspaces are stacked vertically, so a switch reads as travelling up or down the stack,
    /// as ONE strip movement across every workspace between the two: a jump from 1 to 4 scrolls
    /// past 2 and 3. `movement` is the pass's, from `Reactor::take_workspace_movement`. Falls back
    /// to the position-only instant path when there is none or animation is off. Kept apart from
    /// `instant_layout`: layouts merely suppressed during a switch may still change sizes and must
    /// use the full-frame request.
    pub(super) fn workspace_switch_layout(
        &mut self,
        space: SpaceId,
        movement: Option<(usize, usize)>,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
    ) -> bool {
        let animate = self.config.settings.animate && !(self.reads.low_power)();
        if animate
            && let Some((from_index, to_index)) = movement
            && self.start_strip_switch(space, from_index, to_index, layout, skip_wid)
        {
            // The strip movement owns this movement, including placing the real windows once it covers them.
            return true;
        }
        self.instant_layout_inner(space, layout, skip_wid, true)
    }

    fn instant_layout_inner(
        &mut self,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
        position_only: bool,
    ) -> bool {
        let PassPlan { moves, any_frame_changed, .. } =
            pass::plan(self.pass_windows(space, layout, skip_wid, false));

        // One transaction per app: the first window's id, shared by the rest of the batch.
        let mut per_app: HashMap<pid_t, Vec<(WindowId, CGRect, bool)>> = HashMap::default();
        for m in &moves {
            trace!(
                window = ?m.window,
                from = ?m.from,
                to = ?m.to,
                hidden = !m.in_active_workspace,
                "Instant workspace positioning"
            );
            per_app.entry(m.window.pid).or_default().push((
                m.window,
                m.to.to_cg(),
                m.size_unchanged(),
            ));
        }

        for (pid, frames) in per_app {
            let Some(handle) = self.apps.get(&pid).map(|a| a.handle.clone()) else {
                debug!(?pid, "Skipping layout update for app - app no longer exists");
                continue;
            };
            let destinations: Vec<(WindowId, CGRect)> =
                frames.iter().map(|&(wid, to, _)| (wid, to)).collect();
            let txid = Present::new(&mut self.state.windows, &self.transactions)
                .commit_app_batch(&destinations);
            for request in instant_requests(frames, txid, position_only) {
                if let Err(e) = handle.send(request) {
                    debug!(
                        ?pid,
                        ?e,
                        "Failed to send instant layout request - app may have quit"
                    );
                    break;
                }
            }
        }

        any_frame_changed
    }
}

/// Each request placed on the strip as it was and as it is, by `pass::place_on_strip`.
fn place_on_strip(
    requests: &mut [&mut Vec<AnimationRequest>],
    before: &[(WindowId, CGRect)],
    now: &[(WindowId, CGRect)],
) {
    let screen_space = |layout: &[(WindowId, CGRect)]| -> Vec<(WindowId, ScreenRect)> {
        layout
            .iter()
            .map(|&(window, frame)| (window, ScreenRect::from_cg(frame)))
            .collect()
    };
    let (was, is) = (screen_space(before), screen_space(now));
    for requests in requests {
        pass::place_on_strip(requests, &was, &is);
    }
}

/// What a pass changes on the strip, as far as panning it goes: a window's size, the order of two
/// columns, or no more than the scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassChange {
    Resize,
    Move,
    Scroll,
}

/// How far a pass pans the strip surface, or `None` when it does not pan. A pass that resizes a
/// window is neither a pan nor a move: the strip surface draws final sizes, which would snap the
/// resize. A move along the strip is a strip movement whether or not the strip scrolled, with the
/// two columns that changed places crossing it; a scroll under a point is no pan.
fn pan_delta(change: PassChange, strip_movement: Option<CGPoint>) -> Option<CGPoint> {
    match change {
        PassChange::Resize => None,
        PassChange::Move => Some(strip_movement.unwrap_or(CGPoint::new(0.0, 0.0))),
        PassChange::Scroll => strip_movement.filter(|movement| movement.x.abs() >= 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_pans_by_the_strip_movement_unless_it_resizes() {
        let scroll = Some(CGPoint::new(-300.0, 0.0));
        let sliver = Some(CGPoint::new(0.5, 0.0));
        for (change, movement, pan) in [
            (PassChange::Scroll, scroll, scroll),
            (PassChange::Scroll, sliver, None),
            (PassChange::Scroll, None, None),
            (PassChange::Resize, scroll, None),
            (PassChange::Resize, None, None),
            (PassChange::Move, scroll, scroll),
            (PassChange::Move, sliver, sliver),
            (PassChange::Move, None, Some(CGPoint::new(0.0, 0.0))),
        ] {
            assert_eq!(pan_delta(change, movement), pan, "{change:?}, {movement:?}");
        }
    }
}
