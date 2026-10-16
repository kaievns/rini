//! Which final-frame writes go out to the apps, and in what order. See "Real windows land before
//! lift" in `src/transitions/docs/coordination.md`.
use rini_core::ids::WindowId;
use rini_geometry::is_off_screen;
use rini_geometry::space::ScreenRect;

/// The order the overlay's final frames go out to the apps: on-screen destinations first, parks
/// last, each class in the order given. See "Real windows land before lift" in
/// `src/transitions/docs/coordination.md`.
pub fn frame_send_order(
    frames: Vec<(WindowId, ScreenRect)>,
    display: ScreenRect,
) -> Vec<(WindowId, ScreenRect)> {
    let (mut on_screen, parked): (Vec<_>, Vec<_>) =
        frames.into_iter().partition(|(_, frame)| !is_off_screen(display, *frame));
    on_screen.extend(parked);
    on_screen
}

/// A write that moves a window from one park to another: nothing anyone can see changes, and
/// every such write is an Accessibility round trip that makes the app repaint while the overlay is
/// flying. A pan sent 20 frames of which 13 were park-to-park; the app repaints stalled the
/// compositor for 50-130ms at the start of the flight. See "Real windows land before lift" in
/// `src/transitions/docs/coordination.md`.
///
/// A park is a sliver still touching the display, never a frame wholly off it: a workspace switch
/// leaves its departing row a full display height below, which macOS clamps to a 41pt band along
/// the bottom edge, and the pass that follows is what moves that band into the corner. Skipping
/// that write left the band on screen and let the windows drift into the active workspace.
pub fn is_park_to_park(current: ScreenRect, target: ScreenRect, display: ScreenRect) -> bool {
    let is_park =
        |frame: ScreenRect| is_off_screen(display, frame) && frame.intersection(&display).is_some();
    is_park(current) && is_park(target)
}

/// Whether a final-frame write must go out, judged from where the window server says the window IS.
///
/// The model's frame is deliberately not an input: a write the app dropped leaves the model saying
/// "parked" while the window still sits on screen, and skipping on the model kept a full Ghostty
/// window on the right half of the display under the strip. No real frame means we cannot rule the
/// write out, so it goes.
pub fn frame_write_needed(
    real: Option<ScreenRect>,
    target: ScreenRect,
    display: ScreenRect,
) -> bool {
    !real.is_some_and(|real| is_park_to_park(real, target, display))
}

/// The writes to send, in send order, and how many park-to-park moves were left out.
///
/// `real` answers where the window server has each window now. It is a lookup rather than a query
/// per window: one round trip per window cost a burst of presses 60ms a press. See "Rapid presses"
/// in `specs/focus.md`.
pub fn writes_to_send(
    frames: Vec<(WindowId, ScreenRect)>,
    display: ScreenRect,
    real: impl Fn(WindowId) -> Option<ScreenRect>,
) -> (Vec<(WindowId, ScreenRect)>, usize) {
    let mut skipped = 0;
    let writes = frame_send_order(frames, display)
        .into_iter()
        .filter(|&(window, target)| {
            let needed = frame_write_needed(real(window), target, display);
            skipped += usize::from(!needed);
            needed
        })
        .collect();
    (writes, skipped)
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{ScreenPoint, ScreenSize};

    use super::*;

    fn rect(origin_x: f64, origin_y: f64, width: f64, height: f64) -> ScreenRect {
        ScreenRect::new(
            ScreenPoint::new(origin_x, origin_y),
            ScreenSize::new(width, height),
        )
    }

    fn display() -> ScreenRect {
        ScreenRect::new(ScreenPoint::new(0.0, 0.0), ScreenSize::new(1728.0, 1117.0))
    }

    fn ids(order: &[(WindowId, ScreenRect)]) -> Vec<WindowId> {
        order.iter().map(|(w, _)| *w).collect()
    }

    /// A strip switch interleaves the leaving window's park with the arriving windows' slots; each
    /// write is four serialized AX round trips, so a park nobody will see must not delay a slot
    /// everybody will.
    #[test]
    fn on_screen_destinations_are_sent_before_parks() {
        let leaving = WindowId::new(1, 1);
        let arriving = WindowId::new(1, 2);
        let park = rect(1728.0, 32.0, 1720.0, 1081.0);
        let slot = rect(4.0, 32.0, 1720.0, 1081.0);
        let order = frame_send_order(vec![(leaving, park), (arriving, slot)], display());
        let ids: Vec<WindowId> = order.iter().map(|(w, _)| *w).collect();
        assert_eq!(
            ids,
            vec![arriving, leaving],
            "park sent before the on-screen slot: {ids:?}"
        );
    }

    /// A window parked on one side re-parked on the other is not written; a window leaving or
    /// arriving is.
    #[test]
    fn only_a_park_to_park_move_is_skipped() {
        let d = display();
        let left_park = ScreenRect::new(
            ScreenPoint::new(d.origin.x - 859.0 + 1.0, d.origin.y + 1116.0),
            ScreenSize::new(859.0, 1081.0),
        );
        let right_park = ScreenRect::new(
            ScreenPoint::new(d.origin.x + d.size.width - 1.0, d.origin.y + 1116.0),
            ScreenSize::new(859.0, 1081.0),
        );
        let slot = ScreenRect::new(
            ScreenPoint::new(d.origin.x + 4.0, d.origin.y + 32.0),
            ScreenSize::new(859.0, 1081.0),
        );
        assert!(is_park_to_park(left_park, right_park, d));
        assert!(is_park_to_park(right_park, right_park, d));
        assert!(!is_park_to_park(right_park, slot, d), "arriving");
        assert!(!is_park_to_park(slot, right_park, d), "leaving");
        assert!(!is_park_to_park(slot, slot, d));
        // A switch's departing row sits a display height below, wholly off the display; macOS
        // clamps it to a band along the bottom edge, so the write that parks it must go out.
        let row_below = ScreenRect::new(
            ScreenPoint::new(slot.origin.x, d.origin.y + d.size.height + 32.0),
            slot.size,
        );
        assert!(
            !is_park_to_park(row_below, right_park, d),
            "a stacked row is not a park"
        );
        assert!(!is_park_to_park(right_park, row_below, d));
    }

    /// A model that says parked is not trusted, since the app may have dropped the write: the write
    /// is judged from the window server's frame, a window that is really on screen is always sent
    /// to its park, and an unknown real frame never suppresses a write.
    #[test]
    fn park_write_is_judged_from_the_real_frame_not_the_model() {
        let d = display();
        let right_park = ScreenRect::new(
            ScreenPoint::new(d.origin.x + d.size.width - 1.0, d.origin.y + 1116.0),
            ScreenSize::new(859.0, 1081.0),
        );
        let left_park = ScreenRect::new(
            ScreenPoint::new(d.origin.x - 859.0 + 1.0, d.origin.y + 1116.0),
            ScreenSize::new(859.0, 1081.0),
        );
        let slot = ScreenRect::new(
            ScreenPoint::new(d.origin.x + 4.0, d.origin.y + 32.0),
            ScreenSize::new(859.0, 1081.0),
        );
        // Model would say left_park -> right_park (skip); the server says the window is in a slot.
        assert!(
            frame_write_needed(Some(slot), right_park, d),
            "on-screen window must be parked"
        );
        assert!(
            !frame_write_needed(Some(left_park), right_park, d),
            "true park-to-park is skipped"
        );
        assert!(
            frame_write_needed(None, right_park, d),
            "unknown real frame never suppresses"
        );
        assert!(
            frame_write_needed(Some(right_park), slot, d),
            "arrivals always go out"
        );
    }

    /// Mixed frames: every on-screen destination first, then every park, each class in the order
    /// given. All parked, all on screen, and empty inputs come back as given.
    #[test]
    fn frame_send_order_partitions_stably() {
        let w = |i| WindowId::new(1, i);
        let slot_a = rect(4.0, 32.0, 859.0, 1081.0);
        let slot_b = rect(867.0, 32.0, 859.0, 1081.0);
        let park_right = rect(1727.0, 1116.0, 859.0, 1081.0);
        let park_left = rect(-858.0, 1116.0, 859.0, 1081.0);

        let mixed = vec![
            (w(1), park_right),
            (w(2), slot_a),
            (w(3), park_left),
            (w(4), slot_b),
        ];
        let order = frame_send_order(mixed, display());
        assert_eq!(ids(&order), vec![w(2), w(4), w(1), w(3)]);

        let parked = vec![(w(1), park_right), (w(2), park_left)];
        assert_eq!(ids(&frame_send_order(parked, display())), vec![w(1), w(2)]);

        let visible = vec![(w(1), slot_b), (w(2), slot_a)];
        assert_eq!(ids(&frame_send_order(visible, display())), vec![w(1), w(2)]);

        assert!(frame_send_order(Vec::new(), display()).is_empty());
    }

    /// For random mixes of slots and parks, the order is a permutation with every on-screen
    /// frame before every park and each class in its given order. Seed 98, 200 runs.
    #[test]
    fn frame_send_order_is_a_stable_partition() {
        let mut seed: u64 = 98;
        let mut next = move |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let display = display();
        let is_park = |frame: &ScreenRect| is_off_screen(display, *frame);
        let mut mixed_runs = 0usize;
        for _ in 0..200 {
            let count = next(8) as usize;
            let frames: Vec<(WindowId, ScreenRect)> = (1..=count)
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
                assert!(
                    visible < park,
                    "seed 98: a park before an on-screen frame: {order:?}"
                );
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

    /// Parks stay parked without a write, on-screen slots are written first, and a window the
    /// window server did not answer for is written anyway.
    #[test]
    fn writes_leave_out_park_to_park_moves_and_keep_the_send_order() {
        let (parked, arriving, unknown) =
            (WindowId::new(1, 1), WindowId::new(1, 2), WindowId::new(1, 3));
        let park = rect(1727.0, 1116.0, 859.0, 1081.0);
        let other_park = rect(-858.0, 1116.0, 859.0, 1081.0);
        let slot = rect(4.0, 32.0, 859.0, 1081.0);
        let real = |window: WindowId| (window == parked).then_some(other_park);

        let (writes, skipped) = writes_to_send(
            vec![(parked, park), (unknown, park), (arriving, slot)],
            display(),
            real,
        );

        assert_eq!(ids(&writes), vec![arriving, unknown]);
        assert_eq!(skipped, 1);
    }
}
