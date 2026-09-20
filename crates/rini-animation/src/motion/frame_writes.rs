//! Which final-frame writes go out to the apps, and in what order. See "Real windows land before
//! lift" in `docs/animation-smoothness.md`.
use objc2_core_foundation::CGRect;
use rini_geometry::{CGRectExt, is_off_screen};
use rini_windows::ids::WindowId;

/// The order the overlay's final frames go out to the apps: on-screen destinations first, parks
/// last, each class in the order given. See "Real windows land before lift" in
/// `crates/rini-animation/docs/animation-smoothness.md`.
pub fn frame_send_order(
    frames: Vec<(WindowId, CGRect)>,
    display: CGRect,
) -> Vec<(WindowId, CGRect)> {
    let (mut on_screen, parked): (Vec<_>, Vec<_>) =
        frames.into_iter().partition(|(_, frame)| !is_off_screen(display, *frame));
    on_screen.extend(parked);
    on_screen
}

/// A write that moves a window from one park to another: nothing anyone can see changes, and
/// every such write is an Accessibility round trip that makes the app repaint while the overlay is
/// flying. A pan sent 20 frames of which 13 were park-to-park; the app repaints stalled the
/// compositor for 50-130ms at the start of the flight. See "Real windows land before lift" in
/// `crates/rini-animation/docs/animation-smoothness.md`.
///
/// A park is a sliver still touching the display, never a frame wholly off it: a workspace switch
/// leaves its departing row a full display height below, which macOS clamps to a 41pt band along
/// the bottom edge, and the pass that follows is what moves that band into the corner. Skipping
/// that write left the band on screen and let the windows drift into the active workspace.
pub fn is_park_to_park(current: CGRect, target: CGRect, display: CGRect) -> bool {
    let is_park = |frame: CGRect| {
        is_off_screen(display, frame) && frame.intersection(&display).area() > 0.0
    };
    is_park(current) && is_park(target)
}

/// Whether a final-frame write must go out, judged from where the window server says the window IS.
///
/// The model's frame is deliberately not an input: a write the app dropped leaves the model saying
/// "parked" while the window still sits on screen, and skipping on the model kept a full Ghostty
/// window on the right half of the display under the strip. No real frame means we cannot rule the
/// write out, so it goes.
pub fn frame_write_needed(real: Option<CGRect>, target: CGRect, display: CGRect) -> bool {
    !real.is_some_and(|real| is_park_to_park(real, target, display))
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


    fn ids(order: &[(WindowId, CGRect)]) -> Vec<WindowId> {
        order.iter().map(|(w, _)| *w).collect()
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
            |frame: &CGRect| is_off_screen(display, *frame);
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
}
