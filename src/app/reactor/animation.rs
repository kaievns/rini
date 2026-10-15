use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{SpaceId, WindowId, pid_t};
use rini_geometry::space::{FromCg, ScreenRect, ScreenSize, ToCg};
use rini_geometry::stretch::is_a_resize;
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, trace};

use crate::app::reactor::Reactor;
use crate::app::reactor::present::{Present, instant_requests};
use crate::transitions::domain::pass::{self, Move, PassPlan, PassWindow};
use crate::transitions::domain::travel::changes_visibly;
use crate::windows::domain::request::Request;

/// The layout side of animation: decides per pass whether the overlay flies it, and places
/// the real windows when it does not. A namespace; it holds no state. The sorting of a pass is
/// `crate::transitions::domain::pass`; this applies the plan to the stores and the apps.
pub struct AnimationManager;

impl AnimationManager {
    /// One `PassWindow` per window the pass touches, read from the reactor's stores.
    fn gather(
        reactor: &Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
        app_liveness_matters: bool,
    ) -> Vec<PassWindow> {
        let engine = &reactor.layout_manager.layout_engine;
        let windows = &reactor.state.windows;
        let rings = reactor.layout_rings();
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
                let app_alive =
                    !app_liveness_matters || reactor.app_manager.apps.contains_key(&wid.pid);
                if app_liveness_matters && !app_alive {
                    debug!(?wid, "Skipping for window - app no longer exists");
                }
                Some(PassWindow {
                    window: wid,
                    server_id,
                    current: ScreenRect::from_cg(window.frame_monotonic),
                    target: ScreenRect::from_cg(target),
                    pending: server_id
                        .and_then(|wsid| reactor.transaction_manager.get_target_frame(wsid))
                        .map(ScreenRect::from_cg),
                    in_active_workspace: in_active(wid),
                    floating: engine.is_window_floating(wid),
                    app_alive,
                    worn: reactor.worn(wid, &rings),
                })
            })
            .collect()
    }

    /// Records a move in the window store and the transaction table; returns its transaction id.
    ///
    /// The narrow borrows are `present::Present`, which is also where the batching rule and its
    /// tests live.
    fn commit(
        reactor: &mut Reactor,
        m: &Move,
    ) -> crate::windows::domain::transaction::TransactionId {
        Present::new(&mut reactor.state.windows, &reactor.transaction_manager)
            .commit(m.window, m.to.to_cg())
    }

    pub fn animate_layout(
        reactor: &mut Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        is_resize: bool,
        skip_wid: Option<WindowId>,
    ) -> bool {
        let Some(active_ws) = reactor.layout_manager.layout_engine.active_workspace(space) else {
            return false;
        };
        // A pass only asks for pictures of the windows it moves, and after a restart nothing moves:
        // every window is where the saved layout left it. So each window is asked for once, the first
        // time a pass lays it out, whichever workspace it is on. Per window rather than per space:
        // at startup apps are found one by one, and the first pass knew one window of 21.
        reactor
            .pictured_windows
            .retain(|window| reactor.state.windows.window(*window).is_some());
        let first_laid_out: Vec<crate::pictures::domain::target::PictureTarget> = layout
            .iter()
            .filter(|(window, _)| !reactor.pictured_windows.contains(window))
            .filter_map(|&(window, frame)| {
                Some(crate::pictures::domain::target::PictureTarget {
                    window,
                    server_id: reactor.state.windows.window(window)?.info.sys_id?,
                    size: ScreenSize::from_cg(frame.size),
                })
            })
            .collect();
        if !first_laid_out.is_empty()
            && let Some(pictures) = &reactor.communication_manager.pictures
        {
            reactor
                .pictured_windows
                .extend(first_laid_out.iter().map(|target| target.window));
            pictures.ask(crate::pictures::domain::schedule::Ask {
                priority: crate::pictures::domain::schedule::Priority::Warm,
                targets: first_laid_out,
            });
        }
        let plan = pass::plan(Self::gather(reactor, space, layout, skip_wid, true));
        // A strip scroll moves every window by the SAME vector: a viewport pan over the one
        // workspace, the horizontal twin of the vertical workspace switch. The strip's own scroll
        // offset says how far, once per press; reading it off the windows answered differently on
        // each of the several passes one keystroke produces. Taken on every pass, placing or not, so
        // the offset and the strip's layout stay current; the move with it, so it reaches one pass
        // only.
        let strip_movement = reactor.take_strip_movement(space);
        let (strip_before, strip_now) = reactor.take_strip_layout(space).unwrap_or_default();
        let strip_before = strip_before.unwrap_or_default();
        let moved = reactor.take_move_in(layout);

        // Visible moves wait for the flight decision; hidden ones go straight to the app.
        let mut placements = Vec::new();
        for m in &plan.moves {
            let txid = Self::commit(reactor, m);
            let Some(handle) =
                reactor.app_manager.apps.get(&m.window.pid).map(|a| a.handle.clone())
            else {
                continue;
            };
            if m.in_active_workspace {
                trace!(window = ?m.window, from = ?m.from, to = ?m.to, "Animating visible window");
                placements.push((handle, m.window, m.to.to_cg(), txid));
            } else {
                trace!(window = ?m.window, from = ?m.from, to = ?m.to, "Direct positioning hidden window");
                let request = Request::SetWindowFrame(m.window, m.to.to_cg(), txid, true);
                if let Err(e) = handle.send(request) {
                    debug!(window = ?m.window, ?e, "Failed to send frame request for hidden window");
                }
            }
        }

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
        let screen_space = |layout: &[(WindowId, CGRect)]| -> Vec<(WindowId, ScreenRect)> {
            layout
                .iter()
                .map(|&(window, frame)| (window, ScreenRect::from_cg(frame)))
                .collect()
        };
        let (was, is) = (screen_space(&strip_before), screen_space(&strip_now));
        pass::place_on_strip(&mut overlay_requests, &was, &is);
        pass::place_on_strip(&mut unmoved, &was, &is);
        let low_power = (reactor.reads.low_power)();
        let layout_animate = reactor.config.settings.animate;
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
            (is_resize && reactor.is_in_drag()) || !layout_animate || low_power || unchanged;
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
            && reactor.communication_manager.transitions_tx.is_some();

        // A pass that resizes a window is neither a pan nor a move: the strip surface draws final
        // sizes, which would snap the resize. A move along the strip is a strip movement whether or
        // not the strip scrolled, with the two columns that changed places crossing it.
        let moved = moved.filter(|_| !any_resize);
        let pan_delta = if any_resize {
            None
        } else if moved.is_some() {
            Some(strip_movement.unwrap_or(CGPoint::new(0.0, 0.0)))
        } else {
            strip_movement.filter(|movement| movement.x.abs() >= 1.0)
        };
        let moves: Vec<(WindowId, CGRect)> =
            placements.iter().map(|&(_, window, frame, _)| (window, frame)).collect();
        if use_overlay
            && let Some(delta) = pan_delta
            && reactor.start_strip_pan(
                space,
                active_ws,
                moves,
                delta,
                moved,
                &overlay_requests,
                &strip_before,
            )
        {
            // The strip movement owns it, including placing the real windows once it covers them.
        } else if use_overlay {
            let duration = std::time::Duration::from_secs_f64(
                reactor.config.settings.animation_duration.max(0.0),
            );
            let count = overlay_requests.len();
            let flight = crate::transitions::platform::engine::Event::Animate {
                windows: overlay_requests,
                focus: reactor.layout_manager.layout_engine.focused_window(),
                focused_before: reactor.focused_as_event_began,
                key: reactor.main_window_tracker.key_window(),
                duration,
                look: crate::animation::domain::choreography::Look::default(),
                cause: reactor.cause,
            };
            if reactor.send_flight(space, flight) {
                trace!(count, "handed the layout to the overlay animation engine");
            }
        }

        // Warm whether or not this pass animated.
        if !warm.is_empty()
            && let Some(pictures) = &reactor.communication_manager.pictures
        {
            pictures.ask(crate::pictures::domain::schedule::Ask {
                priority: crate::pictures::domain::schedule::Priority::Warm,
                targets: warm,
            });
        }

        if !use_overlay {
            // Nothing flies: the windows go straight to their frames. When the overlay flies, the
            // real windows are placed by Event::ApplyOverlayFrames once it covers them; placing them
            // now let them visibly jump into position before the overlay appeared.
            for (handle, wid, frame, txid) in placements {
                _ = handle.send(Request::SetWindowFrame(wid, frame, txid, true));
            }
        }

        any_frame_changed
    }

    pub fn instant_layout(
        reactor: &mut Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
    ) -> bool {
        Self::instant_layout_inner(reactor, space, layout, skip_wid, false)
    }

    /// Slide a workspace switch vertically instead of cutting to it.
    ///
    /// Workspaces are stacked vertically, so a switch reads as travelling up or down the stack,
    /// as ONE strip movement across every workspace between the two: a jump from 1 to 4 scrolls
    /// past 2 and 3. `movement` is the pass's, from `Reactor::take_workspace_movement`. Falls back
    /// to the position-only instant path when there is none or animation is off. Kept apart from
    /// `instant_layout`: layouts merely suppressed during a switch may still change sizes and must
    /// use the full-frame request.
    pub fn workspace_switch_layout(
        reactor: &mut Reactor,
        space: SpaceId,
        movement: Option<(usize, usize)>,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
    ) -> bool {
        let animate = reactor.config.settings.animate && !(reactor.reads.low_power)();
        if animate
            && let Some((from_index, to_index)) = movement
            && reactor.start_strip_switch(space, from_index, to_index, layout, skip_wid)
        {
            // The strip movement owns this movement, including placing the real windows once it covers them.
            return true;
        }
        Self::instant_layout_inner(reactor, space, layout, skip_wid, true)
    }

    fn instant_layout_inner(
        reactor: &mut Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
        position_only: bool,
    ) -> bool {
        let PassPlan { moves, any_frame_changed, .. } =
            pass::plan(Self::gather(reactor, space, layout, skip_wid, false));

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
            let Some(handle) = reactor.app_manager.apps.get(&pid).map(|a| a.handle.clone()) else {
                debug!(?pid, "Skipping layout update for app - app no longer exists");
                continue;
            };
            let destinations: Vec<(WindowId, CGRect)> =
                frames.iter().map(|&(wid, to, _)| (wid, to)).collect();
            let txid = Present::new(&mut reactor.state.windows, &reactor.transaction_manager)
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
