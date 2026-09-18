use objc2_core_foundation::CGRect;
use tracing::{debug, trace};

use super::TransactionId;
use rini_overlay::window_snapshot::is_a_resize;
use crate::actor::app::{AppThreadHandle, Request, WindowId, pid_t};
use crate::actor::reactor::Reactor;
use rini_shared::collections::HashMap;
use rini_shared::geometry::{Round, SameAs};
use rini_macos::power;
use rini_shared::ids::SpaceId;
use rini_shared::ids::WindowServerId;

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
        let mut overlay_requests: Vec<rini_overlay::engine::AnimationRequest> =
            Vec::new();
        // Every window in this layout, visible or not, so the cache can be warmed for the ones that
        // will slide in on a later switch. Real WindowIds, which is the whole point: ids derived from
        // the window server never match what an animation looks up.
        let mut warm_targets: Vec<rini_overlay::snapshot_service::SnapshotTarget> = Vec::new();
        // The windows this pass leaves where they are. They still have to be DRAWN: the overlay is opaque
        // and covers the display, so anything it omits vanishes for the length of the animation. See "The
        // overlay has to draw everything it covers" in `crates/rini-overlay/docs/capture-overlay-research.md`.
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
                warm_targets.push(rini_overlay::snapshot_service::SnapshotTarget {
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
                    overlay_requests.push(rini_overlay::engine::AnimationRequest {
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
                overlay_requests.push(rini_overlay::engine::AnimationRequest {
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
            // See "Resizes through the overlay" in `crates/rini-overlay/docs/animation-smoothness.md`.
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
                    _ = tx.send(rini_overlay::engine::Event::Animate {
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
                    _ = tx.send(rini_overlay::engine::Event::WarmWindows(
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

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    fn rect(origin_x: f64, origin_y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(origin_x, origin_y), CGSize::new(width, height))
    }


    fn display() -> CGRect {
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1728.0, 1117.0))
    }

    /// A strip column, as the animation path describes one: where it is now, where the layout wants it.
    fn moving(from_x: f64, to_x: f64) -> rini_overlay::engine::AnimationRequest {
        rini_overlay::engine::AnimationRequest {
            window: WindowId::new(1, (from_x.abs() as u32).max(1)),
            server_id: rini_macos::window_server::WindowServerId::new(1),
            from: CGRect::new(CGPoint::new(from_x, 32.0), CGSize::new(859.0, 1081.0)),
            to: CGRect::new(CGPoint::new(to_x, 32.0), CGSize::new(859.0, 1081.0)),
            floating: false,
        }
    }

    /// T5 (1.3) of `.kiro/specs/flight-render-stability/bugfix.md`. A strip switch interleaves
    /// the leaving window's park with the arriving windows' slots; each write is four serialized
    /// AX round trips, so a park nobody will see must not delay a slot everybody will. Asserts the
    /// fixed order; unfixed sends frames as given.
    #[test]
    fn on_screen_destinations_are_sent_before_parks() {
        let leaving = WindowId::new(1, 1);
        let arriving = WindowId::new(1, 2);
        let park = rect(1728.0, 32.0, 1720.0, 1081.0);
        let slot = rect(4.0, 32.0, 1720.0, 1081.0);
        let order = frame_send_order(vec![(leaving, park), (arriving, slot)], display());
        let ids: Vec<WindowId> = order.iter().map(|(w, _)| *w).collect();
        assert_eq!(ids, vec![arriving, leaving], "park sent before the on-screen slot: {ids:?}");
    }

    fn ids(order: &[(WindowId, CGRect)]) -> Vec<WindowId> {
        order.iter().map(|(w, _)| *w).collect()
    }

    /// 2.3 of `flight-render-stability`. Mixed frames: every on-screen destination first, then
    /// every park, each class in the order given. All parked, all on screen, and empty inputs
    /// come back as given.
    /// A window parked on one side re-parked on the other is not written; a window leaving or
    /// arriving is.
    #[test]
    fn only_a_park_to_park_move_is_skipped() {
        let d = display();
        let left_park = CGRect::new(CGPoint::new(d.origin.x - 859.0 + 1.0, d.origin.y + 1116.0), CGSize::new(859.0, 1081.0));
        let right_park = CGRect::new(CGPoint::new(d.origin.x + d.size.width - 1.0, d.origin.y + 1116.0), CGSize::new(859.0, 1081.0));
        let slot = CGRect::new(CGPoint::new(d.origin.x + 4.0, d.origin.y + 32.0), CGSize::new(859.0, 1081.0));
        assert!(is_park_to_park(left_park, right_park, d));
        assert!(is_park_to_park(right_park, right_park, d));
        assert!(!is_park_to_park(right_park, slot, d), "arriving");
        assert!(!is_park_to_park(slot, right_park, d), "leaving");
        assert!(!is_park_to_park(slot, slot, d));
        // A switch's departing row sits a display height below, wholly off the display; macOS
        // clamps it to a band along the bottom edge, so the write that parks it must go out.
        let row_below = CGRect::new(CGPoint::new(slot.origin.x, d.origin.y + d.size.height + 32.0), slot.size);
        assert!(!is_park_to_park(row_below, right_park, d), "a stacked row is not a park");
        assert!(!is_park_to_park(right_park, row_below, d));
    }

    /// Regression: a full Ghostty window sat on the right half of the display under the strip
    /// because the model said "parked" (the app had dropped an earlier write) and the skip trusted
    /// the model. The write is judged from the window server's frame; a window that is really on
    /// screen is always sent to its park, and an unknown real frame never suppresses a write.
    #[test]
    fn park_write_is_judged_from_the_real_frame_not_the_model() {
        let d = display();
        let right_park = CGRect::new(CGPoint::new(d.origin.x + d.size.width - 1.0, d.origin.y + 1116.0), CGSize::new(859.0, 1081.0));
        let left_park = CGRect::new(CGPoint::new(d.origin.x - 859.0 + 1.0, d.origin.y + 1116.0), CGSize::new(859.0, 1081.0));
        let slot = CGRect::new(CGPoint::new(d.origin.x + 4.0, d.origin.y + 32.0), CGSize::new(859.0, 1081.0));
        // Model would say left_park -> right_park (skip); the server says the window is in a slot.
        assert!(frame_write_needed(Some(slot), right_park, d), "on-screen window must be parked");
        assert!(!frame_write_needed(Some(left_park), right_park, d), "true park-to-park is skipped");
        assert!(frame_write_needed(None, right_park, d), "unknown real frame never suppresses");
        assert!(frame_write_needed(Some(right_park), slot, d), "arrivals always go out");
    }

    #[test]
    fn frame_send_order_partitions_stably() {
        let w = |i| WindowId::new(1, i);
        let slot_a = rect(4.0, 32.0, 859.0, 1081.0);
        let slot_b = rect(867.0, 32.0, 859.0, 1081.0);
        let park_right = rect(1727.0, 1116.0, 859.0, 1081.0);
        let park_left = rect(-858.0, 1116.0, 859.0, 1081.0);

        let mixed = vec![(w(1), park_right), (w(2), slot_a), (w(3), park_left), (w(4), slot_b)];
        let order = frame_send_order(mixed, display());
        assert_eq!(ids(&order), vec![w(2), w(4), w(1), w(3)]);

        let parked = vec![(w(1), park_right), (w(2), park_left)];
        assert_eq!(ids(&frame_send_order(parked, display())), vec![w(1), w(2)]);

        let visible = vec![(w(1), slot_b), (w(2), slot_a)];
        assert_eq!(ids(&frame_send_order(visible, display())), vec![w(1), w(2)]);

        assert!(frame_send_order(Vec::new(), display()).is_empty());
    }

    /// 2.3. For random mixes of slots and parks, the order is a permutation with every on-screen
    /// frame before every park and each class in its given order. Seed 98, 200 runs.
    #[test]
    fn frame_send_order_is_a_stable_partition() {
        let mut seed: u64 = 98;
        let mut next = move |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let display = display();
        let is_park =
            |frame: &CGRect| crate::model::HiddenWindowPlacement::is_off_screen(display, *frame);
        let mut mixed_runs = 0usize;
        for _ in 0..200 {
            let count = next(8) as usize;
            let frames: Vec<(WindowId, CGRect)> = (1..=count)
                .map(|i| {
                    let frame = match next(3) {
                        0 => rect(1727.0, 1116.0, 859.0, 1081.0),
                        1 => rect(-1719.0 + next(100) as f64, 1116.0, 1720.0, 1081.0),
                        _ => rect(next(1400) as f64 - 200.0, 32.0, 859.0, 1081.0),
                    };
                    (WindowId::new(1, i as u32), frame)
                })
                .collect();
            let order = frame_send_order(frames.clone(), display);

            let mut sorted_in = ids(&frames);
            let mut sorted_out = ids(&order);
            sorted_in.sort();
            sorted_out.sort();
            assert_eq!(sorted_in, sorted_out, "seed 98: not a permutation");

            let first_park = order.iter().position(|(_, f)| is_park(f));
            let last_visible = order.iter().rposition(|(_, f)| !is_park(f));
            if let (Some(park), Some(visible)) = (first_park, last_visible) {
                assert!(visible < park, "seed 98: a park before an on-screen frame: {order:?}");
                mixed_runs += 1;
            }
            let given_visible: Vec<_> = frames.iter().filter(|(_, f)| !is_park(f)).collect();
            let sent_visible: Vec<_> = order.iter().filter(|(_, f)| !is_park(f)).collect();
            assert_eq!(given_visible, sent_visible, "seed 98: on-screen order changed");
            let given_parks: Vec<_> = frames.iter().filter(|(_, f)| is_park(f)).collect();
            let sent_parks: Vec<_> = order.iter().filter(|(_, f)| is_park(f)).collect();
            assert_eq!(given_parks, sent_parks, "seed 98: park order changed");
        }
        assert!(mixed_runs > 0, "generator sanity: no mixed run");
    }



    /// A one-point move is the measured case, not a hypothetical: a floating window oscillated between
    /// x = 502 and x = 503 on every space-state refresh, and each of those ran a full-screen animation that
    /// blanked every other window for 350ms.
    #[test]
    fn a_move_of_a_single_point_is_not_worth_animating() {
        assert!(!travels_visibly(&[moving(502.0, 503.0)]));
        assert!(!travels_visibly(&[moving(503.0, 502.0)]));
        assert!(!travels_visibly(&[]));
    }

    #[test]
    fn a_real_move_is_worth_animating() {
        // A column step on this display, and the smallest move that counts.
        assert!(travels_visibly(&[moving(4.0, 865.0)]));
        assert!(travels_visibly(&[moving(4.0, 6.0)]));
        // One window going somewhere is enough, however many are standing still around it.
        assert!(travels_visibly(&[moving(502.0, 502.0), moving(4.0, 865.0)]));
    }

    /// Vertical movement counts too: a workspace switch travels in y, and reading only x would treat one as
    /// motionless and place every window instantly.
    #[test]
    fn travel_is_measured_on_both_axes() {
        let mut vertical = moving(4.0, 4.0);
        vertical.to.origin.y = 32.0 + 1117.0;
        assert!(travels_visibly(&[vertical]));
    }



    /// The surface gives the way the view was pushed: focus right at the last column pulls the
    /// strip left, the next workspace at the bottom pulls the row up.
    #[test]
    fn an_edge_bounce_moves_the_content_the_way_it_would_have_gone() {
        use rini_layout::Direction;
        let o = EDGE_BOUNCE_OVERSHOOT;
        assert_eq!(edge_bounce_overshoot(Direction::Right), CGPoint::new(-o, 0.0));
        assert_eq!(edge_bounce_overshoot(Direction::Left), CGPoint::new(o, 0.0));
        assert_eq!(edge_bounce_overshoot(Direction::Down), CGPoint::new(0.0, -o));
        assert_eq!(edge_bounce_overshoot(Direction::Up), CGPoint::new(0.0, o));
        assert!(o < 100.0, "a nudge, not a scroll");
    }

}


/// The order the overlay's final frames go out to the apps: on-screen destinations first, parks
/// last, each class in the order given. See "Real windows land before lift" in
/// `crates/rini-overlay/docs/animation-smoothness.md`.
pub(super) fn frame_send_order(
    frames: Vec<(WindowId, CGRect)>,
    display: CGRect,
) -> Vec<(WindowId, CGRect)> {
    let (mut on_screen, parked): (Vec<_>, Vec<_>) = frames.into_iter().partition(|(_, frame)| {
        !crate::model::HiddenWindowPlacement::is_off_screen(display, *frame)
    });
    on_screen.extend(parked);
    on_screen
}

/// A write that moves a window from one park to another: nothing anyone can see changes, and
/// every such write is an Accessibility round trip that makes the app repaint while the overlay is
/// flying. A pan sent 20 frames of which 13 were park-to-park; the app repaints stalled the
/// compositor for 50-130ms at the start of the flight. See "Real windows land before lift" in
/// `crates/rini-overlay/docs/animation-smoothness.md`.
///
/// A park is a sliver still touching the display, never a frame wholly off it: a workspace switch
/// leaves its departing row a full display height below, which macOS clamps to a 41pt band along
/// the bottom edge, and the pass that follows is what moves that band into the corner. Skipping
/// that write left the band on screen and let the windows drift into the active workspace.
pub(super) fn is_park_to_park(current: CGRect, target: CGRect, display: CGRect) -> bool {
    use crate::model::HiddenWindowPlacement;
    let is_park = |frame: CGRect| {
        HiddenWindowPlacement::is_off_screen(display, frame)
            && HiddenWindowPlacement::intersection_area(frame, display) > 0.0
    };
    is_park(current) && is_park(target)
}

/// Whether a final-frame write must go out, judged from where the window server says the window IS.
///
/// The model's frame is deliberately not an input: a write the app dropped leaves the model saying
/// "parked" while the window still sits on screen, and skipping on the model kept a full Ghostty
/// window on the right half of the display under the strip. No real frame means we cannot rule the
/// write out, so it goes.
pub(super) fn frame_write_needed(real: Option<CGRect>, target: CGRect, display: CGRect) -> bool {
    !real.is_some_and(|real| is_park_to_park(real, target, display))
}

/// How far one window is travelling, as a single distance.
///
/// Diagonal moves are rare in a tiling layout, so the larger of the two axes is the honest measure and
/// avoids paying for a square root on every window of every layout pass.
fn travel(request: &rini_overlay::engine::AnimationRequest) -> f64 {
    let dx = request.to.origin.x - request.from.origin.x;
    let dy = request.to.origin.y - request.from.origin.y;
    dx.abs().max(dy.abs())
}

/// Whether any window in this pass moves far enough for the movement to be worth showing.
///
/// The threshold is about cost, not precision: an animation covers the display for its duration, so a
/// one-point move buys nothing and freezes everything. Two points, because the layout rounds to whole
/// points and a column boundary can land either side of where it was without anything having moved.
fn travels_visibly(requests: &[rini_overlay::engine::AnimationRequest]) -> bool {
    const MIN_VISIBLE_TRAVEL: f64 = 2.0;
    requests.iter().any(|request| travel(request) >= MIN_VISIBLE_TRAVEL)
}

/// How far the surface gives when a command pushes past an end, in points. Enough to read as
/// the view straining against a stop, small enough that no column leaves its place.
pub const EDGE_BOUNCE_OVERSHOOT: f64 = 36.0;

/// The surface's nudge for a push in `direction`: the way the view was pushed, so the content
/// moves the opposite way, as it would have had there been anything further. Focus right at the
/// last column pulls the strip left; the next workspace at the bottom of the stack pulls the
/// row up.
pub fn edge_bounce_overshoot(direction: rini_layout::Direction) -> objc2_core_foundation::CGPoint {
    use rini_layout::Direction;
    match direction {
        Direction::Left => objc2_core_foundation::CGPoint::new(EDGE_BOUNCE_OVERSHOOT, 0.0),
        Direction::Right => objc2_core_foundation::CGPoint::new(-EDGE_BOUNCE_OVERSHOOT, 0.0),
        Direction::Up => objc2_core_foundation::CGPoint::new(0.0, EDGE_BOUNCE_OVERSHOOT),
        Direction::Down => objc2_core_foundation::CGPoint::new(0.0, -EDGE_BOUNCE_OVERSHOOT),
    }
}
