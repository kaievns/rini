use objc2_core_foundation::CGRect;
use tracing::{debug, trace};

use rini_animation::motion::travel::travels_visibly;
use rini_animation::pass::{self, Move, PassPlan, PassWindow};
use rini_animation::window_snapshot::is_a_resize;
use rini_windows::app_actor::Request;
use rini_windows::ids::{WindowId, pid_t};
use crate::actor::reactor::Reactor;
use rustc_hash::FxHashMap as HashMap;
use rini_animation::power;
use rini_displays::ids::SpaceId;

/// The layout side of animation: decides per pass whether the overlay flies it, and places
/// the real windows when it does not. A namespace; it holds no state. The sorting of a pass is
/// `rini_animation::pass`; this applies the plan to the stores and the apps.
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
        // An unassigned window is not on the active workspace: it must not fly.
        let active_ws = engine.active_workspace(space);
        let in_active = |wid| {
            active_ws.is_some()
                && engine.virtual_workspace_manager().workspace_for_window(windows, space, wid) == active_ws
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
                let app_alive = !app_liveness_matters || reactor.app_manager.apps.contains_key(&wid.pid);
                if app_liveness_matters && !app_alive {
                    debug!(?wid, "Skipping for window - app no longer exists");
                }
                Some(PassWindow {
                    window: wid,
                    server_id,
                    current: window.frame_monotonic,
                    target,
                    pending: server_id.and_then(|wsid| reactor.transaction_manager.get_target_frame(wsid)),
                    in_active_workspace: in_active(wid),
                    floating: engine.is_window_floating(wid),
                    app_alive,
                })
            })
            .collect()
    }

    /// Records a move in the window store and the transaction table; returns its transaction id.
    fn commit(reactor: &mut Reactor, m: &Move) -> rini_windows::transaction::TransactionId {
        let txid = m
            .server_id
            .map(|wsid| reactor.transaction_manager.generate_next_txid(wsid))
            .unwrap_or_default();
        if let Some(wsid) = m.server_id {
            reactor.transaction_manager.update_txid_entries([(wsid, txid, m.to)]);
        }
        if let Some(window) = reactor.state.windows.window_mut(m.window) {
            window.frame_monotonic = m.to;
        }
        txid
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
        let plan = pass::plan(Self::gather(reactor, space, layout, skip_wid, true));

        // Visible moves wait for the flight decision; hidden ones go straight to the app.
        let mut placements = Vec::new();
        for m in &plan.moves {
            let txid = Self::commit(reactor, m);
            let Some(handle) = reactor.app_manager.apps.get(&m.window.pid).map(|a| a.handle.clone()) else {
                continue;
            };
            if m.in_active_workspace {
                trace!(window = ?m.window, from = ?m.from, to = ?m.to, "Animating visible window");
                placements.push((handle, m.window, m.to, txid));
            } else {
                trace!(window = ?m.window, from = ?m.from, to = ?m.to, "Direct positioning hidden window");
                if let Err(e) = handle.send(Request::SetWindowFrame(m.window, m.to, txid, true)) {
                    debug!(window = ?m.window, ?e, "Failed to send frame request for hidden window");
                }
            }
        }

        if placements.is_empty() {
            return plan.any_frame_changed;
        }

        let mut overlay_requests = plan.overlay_requests();
        let PassPlan { unmoved, warm, any_frame_changed, .. } = plan;
        let low_power = power::is_low_power_mode_enabled();
        let layout_animate = reactor.config.settings.animate;
        // `is_resize` means a window REPORTED a size change, which happens both when the user is
        // dragging an edge and when rini resized it for a command. Only a real interactive drag
        // skips; animating a command's resize is what keeps the neighbour column and the resized
        // window arriving together.
        //
        // A move nobody can see is not worth covering the display for. Guarded on the list being
        // non-empty, because a window with no window server id never reaches it and an empty list
        // must not read as "nothing is moving".
        let motionless = !overlay_requests.is_empty() && !travels_visibly(&overlay_requests);
        let skip_anim =
            (is_resize && reactor.is_in_drag()) || !layout_animate || low_power || motionless;
        if motionless {
            debug!(
                windows = overlay_requests.len(),
                "no window travels far enough to see; placing rather than animating"
            );
        }
        // Added after the movement test above, which must judge only the windows going somewhere.
        overlay_requests.extend(unmoved);

        // The overlay engine replaces the per-frame Accessibility writes entirely: the real windows
        // are placed once, and the motion the eye follows is drawn in the overlay. It is asked to
        // animate FIRST, and its first frame draws the windows where they are leaving from, so the
        // picture stays continuous even if the real windows land before the overlay is visible.
        // Resizes ride the overlay too, anchored and cropped rather than stretched. See "Resizes
        // through the overlay" in `crates/rini-animation/docs/animation-smoothness.md`.
        let any_resize = overlay_requests
            .iter()
            .any(|request| is_a_resize(request.from.size, request.to.size));
        let use_overlay = !skip_anim
            && !overlay_requests.is_empty()
            && reactor.communication_manager.workspace_animation_tx.is_some();

        // A strip scroll moves every window by the SAME vector: a viewport pan over the one
        // workspace, the horizontal twin of the vertical workspace switch. The strip's own scroll
        // offset says how far, once per press; reading it off the windows answered differently on
        // each of the several passes one keystroke produces. Always consumed, so the offset
        // bookkeeping stays current, but a pass that resizes a window is not a pan: the strip
        // surface draws final sizes, which would snap the resize.
        let strip_movement = reactor.take_strip_movement(space);
        let pan_delta = if any_resize {
            None
        } else {
            strip_movement.filter(|moved| moved.x.abs() >= 1.0)
        };
        if use_overlay
            && let Some(delta) = pan_delta
            && reactor.start_strip_pan(space, active_ws, layout, skip_wid, delta)
        {
            // The strip movement owns it, including placing the real windows once it covers them.
        } else if use_overlay {
            reactor.publish_animation_display_for(Some(space));
            if let Some(tx) = &reactor.communication_manager.workspace_animation_tx {
                let duration = std::time::Duration::from_secs_f64(
                    reactor.config.settings.animation_duration.max(0.0),
                );
                let count = overlay_requests.len();
                _ = tx.send(rini_animation::engine::Event::Animate {
                    windows: overlay_requests,
                    focus: reactor.layout_manager.layout_engine.focused_window(),
                    duration,
                });
                trace!(count, "handed the layout to the overlay animation engine");
            }
        }

        // Warm whether or not this pass animated.
        if !warm.is_empty()
            && let Some(tx) = &reactor.communication_manager.workspace_animation_tx
        {
            _ = tx.send(rini_animation::engine::Event::WarmWindows(warm));
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
    /// past 2 and 3. Falls back to the position-only instant path when there is no recorded
    /// direction or animation is off. Kept apart from `instant_layout`: layouts merely
    /// suppressed during a switch may still change sizes and must use the full-frame request.
    pub fn workspace_switch_layout(
        reactor: &mut Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
    ) -> bool {
        let animate = reactor.config.settings.animate && !power::is_low_power_mode_enabled();
        if animate
            && let Some((from_index, to_index)) = reactor.workspace_switch_indices(space)
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
            per_app.entry(m.window.pid).or_default().push((m.window, m.to, m.size_unchanged()));
            if let Some(window) = reactor.state.windows.window_mut(m.window) {
                window.frame_monotonic = m.to;
            }
        }

        for (pid, frames) in per_app {
            let Some(handle) = reactor.app_manager.apps.get(&pid).map(|a| a.handle.clone()) else {
                debug!(?pid, "Skipping layout update for app - app no longer exists");
                continue;
            };
            let wsid_of = |reactor: &Reactor, wid: WindowId| {
                reactor.state.windows.window(wid).and_then(|w| w.info.sys_id)
            };
            let (first_wid, first_target, _) = frames[0];
            let mut txid = Default::default();
            if let Some(wsid) = wsid_of(reactor, first_wid) {
                txid = reactor.transaction_manager.generate_next_txid(wsid);
                let mut entries = vec![(wsid, txid, first_target)];
                for &(wid, frame, _) in frames.iter().skip(1) {
                    if let Some(wsid) = wsid_of(reactor, wid) {
                        reactor.transaction_manager.set_last_sent_txid(wsid, txid);
                        entries.push((wsid, txid, frame));
                    }
                }
                reactor.transaction_manager.update_txid_entries(entries);
            }
            for request in pass::instant_requests(frames, txid, position_only) {
                if let Err(e) = handle.send(request) {
                    debug!(?pid, ?e, "Failed to send instant layout request - app may have quit");
                    break;
                }
            }
        }

        any_frame_changed
    }
}
