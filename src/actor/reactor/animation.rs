use objc2_core_foundation::CGRect;
use tracing::{debug, trace};

use super::TransactionId;
use rini_animation::motion::travel::travels_visibly;
use rini_animation::window_snapshot::is_a_resize;
use rini_windows::app_actor::{AppThreadHandle, Request};
use rini_windows::ids::{WindowId, pid_t};
use crate::actor::reactor::Reactor;
use rustc_hash::FxHashMap as HashMap;
use rini_geometry::{Round, SameAs};
use crate::platform::power;
use rini_displays::ids::SpaceId;
use rini_windows::ids::WindowServerId;

/// The layout side of animation: decides per pass whether the overlay flies it, and places
/// the real windows when it does not. A namespace; it holds no state.
pub struct AnimationManager;

/// A window the pass moves: who to tell, where it goes, under which transaction.
type Placement = (AppThreadHandle, WindowId, CGRect, TransactionId);

impl AnimationManager {
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
        let mut placements: Vec<Placement> = Vec::new();
        let mut animated_count = 0;
        let mut any_frame_changed = false;
        // Collected alongside the Accessibility animation so the two engines can be compared without
        // duplicating the eligibility rules, which decide what counts as a visible window.
        let mut overlay_requests: Vec<rini_animation::engine::AnimationRequest> =
            Vec::new();
        // Every window in this layout, visible or not, so the cache can be warmed for the ones that
        // will slide in on a later switch. Real WindowIds, which is the whole point: ids derived from
        // the window server never match what an animation looks up.
        let mut warm_targets: Vec<rini_animation::snapshot_service::SnapshotTarget> = Vec::new();
        // The windows this pass leaves where they are. They still have to be DRAWN: the overlay is opaque
        // and covers the display, so anything it omits vanishes for the length of the animation. See "The
        // overlay has to draw everything it covers" in `crates/rini-animation/docs/capture-overlay-research.md`.
        let mut unmoved: Vec<(WindowId, CGRect, WindowServerId)> = Vec::new();

        for &(wid, target_frame) in layout {
            if skip_wid == Some(wid) {
                trace!(
                    ?wid,
                    "Skipping animated layout update for window currently being dragged"
                );
                continue;
            }

            let target_frame = target_frame.round();
            let (current_frame, window_server_id, txid) = {
                let window_store = &mut reactor.state.windows;
                match window_store.window_mut(wid) {
                    Some(window) => {
                        let current_frame = window.frame_monotonic;
                        if target_frame.same_as(current_frame) {
                            if let Some(wsid) = window.info.sys_id {
                                unmoved.push((wid, current_frame, wsid));
                            }
                            continue;
                        }
                        let wsid = window.info.sys_id;
                        if let Some(wsid) = wsid {
                            if reactor
                                .transaction_manager
                                .get_target_frame(wsid)
                                .is_some_and(|pending| pending.same_as(target_frame))
                            {
                                trace!(?wid, ?target_frame, "Skipping redundant layout request");
                                // Already on its way from an earlier pass, so this pass is not moving it
                                // either, and the overlay still has to draw it.
                                unmoved.push((wid, current_frame, wsid));
                                continue;
                            }
                        }
                        any_frame_changed = true;
                        let txid = wsid
                            .map(|wsid| reactor.transaction_manager.generate_next_txid(wsid))
                            .unwrap_or_default();
                        (current_frame, wsid, txid)
                    }
                    None => {
                        debug!(?wid, "Skipping - window no longer exists");
                        continue;
                    }
                }
            };

            let Some(app_state) = &reactor.app_manager.apps.get(&wid.pid) else {
                debug!(?wid, "Skipping for window - app no longer exists");
                continue;
            };

            if let Some(wsid) = window_server_id {
                warm_targets.push(rini_animation::snapshot_service::SnapshotTarget {
                    window: wid,
                    server_id: wsid,
                    size: target_frame.size,
                });
            }

            let is_active = reactor
                .layout_manager
                .layout_engine
                .virtual_workspace_manager()
                .workspace_for_window(&reactor.state.windows, space, wid)
                .is_some_and(|ws| ws == active_ws);

            if is_active {
                trace!(?wid, ?current_frame, ?target_frame, "Animating visible window");
                placements.push((app_state.handle.clone(), wid, target_frame, txid));
                animated_count += 1;
                if let Some(wsid) = window_server_id {
                    overlay_requests.push(rini_animation::engine::AnimationRequest {
                        window: wid,
                        server_id: wsid,
                        from: current_frame,
                        to: target_frame,
                        floating: reactor.layout_manager.layout_engine.is_window_floating(wid),
                    });
                }
                if let Some(wsid) = window_server_id {
                    reactor.transaction_manager.update_txid_entries([(wsid, txid, target_frame)]);
                }
            } else {
                trace!(
                    ?wid,
                    ?current_frame,
                    ?target_frame,
                    "Direct positioning hidden window"
                );
                if let Some(wsid) = window_server_id {
                    reactor.transaction_manager.update_txid_entries([(wsid, txid, target_frame)]);
                }
                if let Err(e) =
                    app_state.handle.send(Request::SetWindowFrame(wid, target_frame, txid, true))
                {
                    debug!(?wid, ?e, "Failed to send frame request for hidden window");
                    continue;
                }
            }

            if let Some(window) = reactor.state.windows.window_mut(wid) {
                window.frame_monotonic = target_frame;
            }
        }

        if animated_count > 0 {
            let low_power = power::is_low_power_mode_enabled();
            let layout_animate = reactor.config.settings.animate;
            // `is_resize` means a window REPORTED a size change, which happens both
            // when the user is dragging an edge and when we ourselves resized it via
            // a command (ctrl-R preset cycling, ctrl-F full width). Skipping the
            // animation is right for a drag — animation would lag the cursor — but
            // wrong for a command, and it produced a specific visible artefact:
            //
            //   the NEIGHBOUR column animated into its new position (that pass had
            //   is_resize = false), then the resized window SNAPPED to its new size
            //   when the app's own resize notification arrived and forced a second,
            //   unanimated pass.
            //
            // Only a real interactive drag should skip. is_in_drag() is the existing
            // signal for that, already used to suppress arrange passes mid-drag in
            // reactor.rs.
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

            // Added after the movement tests above, which must judge only the windows going somewhere.
            for (wid, frame, wsid) in unmoved {
                let in_active_workspace = reactor
                    .layout_manager
                    .layout_engine
                    .virtual_workspace_manager()
                    .workspace_for_window(&reactor.state.windows, space, wid)
                    .is_some_and(|ws| ws == active_ws);
                if !in_active_workspace {
                    continue;
                }
                overlay_requests.push(rini_animation::engine::AnimationRequest {
                    window: wid,
                    server_id: wsid,
                    from: frame,
                    to: frame,
                    floating: reactor.layout_manager.layout_engine.is_window_floating(wid),
                });
            }

            // The overlay engine replaces the per-frame Accessibility writes entirely: the real
            // windows are placed once, and the motion the eye follows is drawn in the overlay.
            //
            // Ordering matters. The overlay is asked to animate FIRST, and its first frame draws the
            // windows at the positions they are leaving, so even if the real windows land before the
            // overlay is visible the picture stays continuous.
            //
            // Resizes ride the overlay too: the tile is anchored and cropped rather than stretched.
            // See "Resizes through the overlay" in `crates/rini-animation/docs/animation-smoothness.md`.
            let any_resize = overlay_requests
                .iter()
                .any(|request| is_a_resize(request.from.size, request.to.size));
            let use_overlay = !skip_anim
                && !overlay_requests.is_empty()
                && reactor.communication_manager.workspace_animation_tx.is_some();

            // A strip scroll moves every window by the SAME vector, which is a viewport pan over the
            // one workspace, the horizontal twin of the vertical workspace switch. Treating it as one
            // strip pan gives the same sense of distance and the same freedom from per-window drift.
            // The strip's own scroll offset says exactly how far it is travelling, once per press. Reading
            // it off the windows instead answered differently on each of the several layout passes a single
            // keystroke produces: one press retargeted the strip surface five times, with the distance jumping
            // between 6315pt, 9471pt and -7749pt, which is visible as the strip jerking.
            // Always consumed, so the offset bookkeeping stays current, but a pass that resizes a
            // window is not a pan — the strip surface draws final sizes, which would snap the resize.
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

            // Warm whether or not this pass animated: the windows that will slide in next are exactly
            // the ones sitting off-strip now, and they are only capturable ahead of time.
            if !warm_targets.is_empty() {
                if let Some(tx) = &reactor.communication_manager.workspace_animation_tx {
                    _ = tx.send(rini_animation::engine::Event::WarmWindows(
                        std::mem::take(&mut warm_targets),
                    ));
                }
            }

            if use_overlay {
                // Deliberately no Accessibility work here. The real windows are placed by
                // Event::ApplyOverlayFrames once the overlay is covering them, because placing them
                // now let them visibly jump into position before the overlay appeared.
            } else {
                // Nothing flies: the windows go straight to their frames.
                for (handle, wid, frame, txid) in placements {
                    _ = handle.send(Request::SetWindowFrame(wid, frame, txid, true));
                }
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

    /// Apply the position-only layout used while switching virtual workspaces.
    ///
    /// Keep this entry point separate from `instant_layout`: layouts merely suppressed
    /// while a switch is in progress may still change window sizes and must use the
    /// full-frame request.
    /// Slide a workspace switch vertically instead of cutting to it.
    ///
    /// Workspaces are stacked vertically, so a switch reads as travelling up or down the
    /// stack: the arriving strip enters from the opposite edge to the one the departing strip
    /// leaves by. Combined with each column's horizontal position that gives the diagonal
    /// motion the stack implies.
    ///
    /// This used to be an instant reposition. With no animation and no other indicator, two
    /// workspaces on one display were indistinguishable from two overlapping strips — the
    /// switch simply happened, which read as a glitch rather than as movement.
    ///
    /// Falls back to the instant path when there is no recorded direction (the first layout
    /// for a display, or a switch that did not change workspace) or when animation is off.
    pub fn workspace_switch_layout(
        reactor: &mut Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
    ) -> bool {
        let animate = reactor.config.settings.animate && !power::is_low_power_mode_enabled();

        // Animate the switch as ONE strip movement across every workspace between the two, so a jump
        // from 1 to 4 scrolls past 2 and 3. Moving each window separately could not do this: the
        // intermediate workspaces are off screen at both ends of every window's path, so none of them
        // was ever drawn and a four-workspace jump looked exactly like a one-workspace step.
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
        let mut per_app: HashMap<pid_t, Vec<(WindowId, CGRect, bool)>> = HashMap::default();
        let mut any_frame_changed = false;

        for &(wid, target_frame) in layout {
            if skip_wid == Some(wid) {
                trace!(?wid, "Skipping layout update for window currently being dragged");
                continue;
            }

            let is_hidden = !reactor.layout_manager.layout_engine.is_window_in_active_workspace(
                &reactor.state.windows,
                space,
                wid,
            );
            let window_store = &mut reactor.state.windows;
            let Some(window) = window_store.window_mut(wid) else {
                debug!(?wid, "Skipping layout - window no longer exists");
                continue;
            };
            let target_frame = target_frame.round();
            let current_frame = window.frame_monotonic;
            if target_frame.same_as(current_frame) {
                continue;
            }
            if let Some(wsid) = window.info.sys_id {
                if reactor
                    .transaction_manager
                    .get_target_frame(wsid)
                    .is_some_and(|pending| pending.same_as(target_frame))
                {
                    trace!(?wid, ?target_frame, "Skipping redundant instant layout request");
                    continue;
                }
            }
            any_frame_changed = true;
            trace!(
                ?wid,
                ?current_frame,
                ?target_frame,
                hidden = is_hidden,
                "Instant workspace positioning"
            );

            let size_unchanged = current_frame.size.same_as(target_frame.size);
            per_app.entry(wid.pid).or_default().push((wid, target_frame, size_unchanged));
            window.frame_monotonic = target_frame;
        }

        for (pid, frames) in per_app {
            if frames.is_empty() {
                continue;
            }

            let Some(app_state) = reactor.app_manager.apps.get(&pid) else {
                debug!(?pid, "Skipping layout update for app - app no longer exists");
                continue;
            };

            let handle = app_state.handle.clone();

            let (first_wid, first_target, _) = frames[0];
            let mut txid = TransactionId::default();
            let mut has_txid = false;
            let mut txid_entries: Vec<(WindowServerId, TransactionId, CGRect)> = Vec::new();
            if let Some(window) = reactor.state.windows.window_mut(first_wid) {
                if let Some(wsid) = window.info.sys_id {
                    txid = reactor.transaction_manager.generate_next_txid(wsid);
                    has_txid = true;
                    txid_entries.push((wsid, txid, first_target));
                }
            }

            if has_txid {
                for (wid, frame, _) in frames.iter().skip(1) {
                    if let Some(w) = reactor.state.windows.window_mut(*wid)
                        && let Some(wsid) = w.info.sys_id
                    {
                        reactor.transaction_manager.set_last_sent_txid(wsid, txid);
                        txid_entries.push((wsid, txid, *frame));
                    }
                }
                reactor.transaction_manager.update_txid_entries(txid_entries);
            }

            let requests = if position_only {
                let mut positions = Vec::new();
                let mut full_frames = Vec::new();
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
            } else {
                vec![Request::SetBatchWindowFrame(
                    frames.into_iter().map(|(wid, frame, _)| (wid, frame)).collect(),
                    txid,
                    true,
                )]
            };
            for request in requests {
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
