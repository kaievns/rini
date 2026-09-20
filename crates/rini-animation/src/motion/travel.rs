//! Where a tile really starts and ends. The layout hands the engine a `from` and a `to`; a parked
//! window's `from` is a corner it never visibly occupied, and its neighbours' travel says which way it
//! should come in. Everything here is rect arithmetic; the window-server read that feeds `real` is
//! the caller's.

use objc2_core_foundation::{CGPoint, CGRect};
use rini_shared::geometry::{SameAs, is_off_screen, park_entry_frame};

/// What fraction of `frame`'s area lies inside `display`.
pub fn on_screen_fraction(frame: CGRect, display: CGRect) -> f64 {
    let area = frame.size.width * frame.size.height;
    if area <= 0.0 {
        return 0.0;
    }
    let overlap_w = (frame.origin.x + frame.size.width).min(display.origin.x + display.size.width)
        - frame.origin.x.max(display.origin.x);
    let overlap_h = (frame.origin.y + frame.size.height).min(display.origin.y + display.size.height)
        - frame.origin.y.max(display.origin.y);
    if overlap_w <= 0.0 || overlap_h <= 0.0 {
        return 0.0;
    }
    (overlap_w * overlap_h) / area
}

/// The tile's start from the window server's answer (`real`, `None` when it had none) and the
/// request's `from`/`to`. Pure, so the park remap can be tested on plain rects.
///
/// A parked window comes back along the strip's own movement: `to` translated back by
/// `travel`, the vector its neighbours make this pass (`neighbour_travel`). Without a moving
/// neighbour it enters from the display edge on the park's side (`entry_frame`).
pub fn resolve_start(
    real: Option<CGRect>,
    from: CGRect,
    to: CGRect,
    display: CGRect,
    travel: Option<CGPoint>,
) -> CGRect {
        // A park is judged from both frames, before the synthetic test: apps clamp the real frame past
    // the park threshold, and the server may already report the slot. See docs/animation-smoothness.md.
    let parked_real = real.is_some_and(|real| is_off_screen(display, real));
    let parked_from = is_off_screen(display, from);
    if parked_real || parked_from {
        return match travel {
            Some(d) => translated(to, CGPoint::new(-d.x, -d.y)),
            None => {
                let park = if parked_real { real.unwrap_or(from) } else { from };
                park_entry_frame(park, to, display)
            }
        };
    }
    let real = real.unwrap_or(from);
    if start_is_synthetic(real, from, to) {
        return from;
    }
    real
}

/// The tile's visual destination: a window leaving for a corner park travels with the strip,
/// `start` translated by `travel` (`neighbour_travel`), the mirror of `resolve_start`. With no
/// moving neighbour it exits past the display edge on the park's side. See "Layout changes" in
/// `docs/animation-smoothness.md`.
pub fn resolve_end(start: CGRect, to: CGRect, display: CGRect, travel: Option<CGPoint>) -> CGRect {
        if is_off_screen(display, to) && !is_off_screen(display, start) {
        return match travel {
            Some(d) => translated(start, d),
            None => park_entry_frame(to, start, display),
        };
    }
    to
}

/// The vector the rigid strip moves by this pass, as the window at `subject` sees it: the
/// `to - from` of the nearest (by centre x of `from`) strip window that is on screen at both
/// ends and actually moves. `None` when no such neighbour exists, which is the edge fallback's
/// cue. `others` is `(from, to, floating)` for every OTHER request of the pass.
///
/// A displaced window aimed at the display edge covered a different distance from the window
/// beside it under one duration and one curve, so the two ran at different speeds and
/// overlapped (seen 2026-09-15). Sharing the neighbour's vector is what makes them one body.
pub fn neighbour_travel(
    subject: CGRect,
    others: &[(CGRect, CGRect, bool)],
    display: CGRect,
) -> Option<CGPoint> {
        others
        .iter()
        .filter(|(_, _, floating)| !floating)
        .filter(|(from, to, _)| {
            !is_off_screen(display, *from) && !is_off_screen(display, *to)
        })
        // Moving by origin, not `is_moving`: a neighbour that only resizes has no travel to lend.
        .filter(|(from, to, _)| {
            (to.origin.x - from.origin.x).abs() >= 0.5 || (to.origin.y - from.origin.y).abs() >= 0.5
        })
        .min_by(|(a, _, _), (b, _, _)| {
            let da = (a.mid().x - subject.mid().x).abs();
            let db = (b.mid().x - subject.mid().x).abs();
            da.total_cmp(&db)
        })
        .map(|(from, to, _)| CGPoint::new(to.origin.x - from.origin.x, to.origin.y - from.origin.y))
}

/// `frame` moved by `by`, same size.
pub fn translated(frame: CGRect, by: CGPoint) -> CGRect {
    CGRect::new(CGPoint::new(frame.origin.x + by.x, frame.origin.y + by.y), frame.size)
}

/// The subject frame `neighbour_travel` measures from for one request: the slot it leaves when
/// its destination is a park, otherwise the slot it arrives at.
pub fn travel_subject(from: CGRect, to: CGRect, display: CGRect) -> CGRect {
    if is_off_screen(display, to) { from } else { to }
}

/// Is a request's start a deliberate fiction rather than drift to correct?
///
/// A window already sitting at its destination has no drift, so a request that still asks for
/// motion can only be a synthetic start (the debug slide, which invents one). Overriding it with
/// the real frame made `from` equal `to`, which silently killed the whole movement.
pub fn start_is_synthetic(real: CGRect, from: CGRect, to: CGRect) -> bool {
    real.same_as(to) && !from.same_as(to)
}

/// How much of a window has to be on screen for the overlay to bother with it.
///
/// A window being moved needs a real share, or every parked sliver becomes a tile. A window standing still
/// needs only to be visible at all, because whatever shows of it turns into wallpaper otherwise.
pub fn min_on_screen(moving: bool) -> f64 {
    if moving { 0.25 } else { f64::MIN_POSITIVE }
}

/// The smallest visible extent that still reads as a window rather than a sliver.
///
/// The share test alone starved wide windows: a 1720pt window showing 400pt is under a quarter by
/// area yet is exactly the "column peeking in" a scrolling layout is made of. Anything showing at
/// least this much in both axes is drawn.
pub const MIN_VISIBLE_EXTENT: f64 = 80.0;

/// How much of `frame` shows on `display`, as the overlap's width and height.
pub fn on_screen_extent(frame: CGRect, display: CGRect) -> (f64, f64) {
    let w = (frame.origin.x + frame.size.width).min(display.origin.x + display.size.width)
        - frame.origin.x.max(display.origin.x);
    let h = (frame.origin.y + frame.size.height).min(display.origin.y + display.size.height)
        - frame.origin.y.max(display.origin.y);
    (w.max(0.0), h.max(0.0))
}

/// Does a window travelling `from` → `to` appear on `display` at ANY point? The whole path is
/// sampled, not just its ends: a window sweeping across mid-animation is exactly what conveys how
/// far the strip travelled, and testing endpoints alone excluded it.
pub fn worth_animating(from: CGRect, to: CGRect, display: CGRect) -> bool {
    /// Enough that a window cannot cross the display between two samples: the fastest realistic
    /// travel is a few display widths.
    const SAMPLES: usize = 11;

    let area = from.size.width * from.size.height;
    if area <= 0.0 {
        return false;
    }
    let moving = is_moving(from, to);
    (0..SAMPLES).any(|step| {
        let t = step as f64 / (SAMPLES - 1) as f64;
        let at = crate::motion::tile::lerp_rect(from, to, t);
        shows_enough(at, display, moving)
    })
}

/// Whether enough of the window shows at `at` for a tile to be worth drawing there.
pub fn shows_enough(at: CGRect, display: CGRect, moving: bool) -> bool {
    if on_screen_fraction(at, display) >= min_on_screen(moving) {
        return true;
    }
    let (w, h) = on_screen_extent(at, display);
    moving && w.min(h) >= MIN_VISIBLE_EXTENT
}

/// Whether a request actually moves its window.
///
/// Requests with the same start and end are there to be drawn, not moved. Half a point, because the layout
/// rounds to whole points.
pub fn is_moving(from: CGRect, to: CGRect) -> bool {
    (to.origin.x - from.origin.x).abs() >= 0.5
        || (to.origin.y - from.origin.y).abs() >= 0.5
        || (to.size.width - from.size.width).abs() >= 0.5
        || (to.size.height - from.size.height).abs() >= 0.5
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    const DISPLAY: CGRect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize { width: 1728.0, height: 1117.0 },
    };
    const RUNS: usize = 200;


    /// A small deterministic generator, so a failure names its seed and replays.
    struct Gen(u64);

    impl Gen {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 11
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }

        fn pt(&mut self, lo: f64, hi: f64) -> f64 {
            lo + self.below((hi - lo) as u64 + 1) as f64
        }

        fn coin(&mut self) -> bool {
            self.below(2) == 0
        }

        /// A window frame with a real share of the display showing: a column on the strip.
        fn on_screen(&mut self) -> CGRect {
            let w = self.pt(400.0, 1720.0);
            let h = self.pt(600.0, 1081.0);
            let x = self.pt(-w / 4.0, DISPLAY.size.width - w * 0.75);
            rect(x, 32.0, w, h)
        }

        /// The layout's park for a scrolled-off window: 1pt showing in a bottom corner.
        fn park(&mut self, size: CGSize) -> CGRect {
            let x = if self.coin() {
                DISPLAY.size.width - 1.0
            } else {
                DISPLAY.origin.x - size.width + 1.0
            };
            rect(x, DISPLAY.size.height - 1.0, size.width, size.height)
        }
    }

    /// The rigidity property the old canvas layer guaranteed structurally: every unpinned window
    /// of a strip movement translates by exactly the same vector, so tiles animated per-tile from
    /// these rects cannot drift apart. If this ever fails, the strip tears.
    #[test]
    fn a_strip_movement_translates_every_window_by_the_same_vector() {
        let from_offset = CGPoint::new(0.0, 0.0);
        let to_offset = CGPoint::new(-861.0, 1117.0);
        let frames = [
            rect(4.0, 32.0, 859.0, 1081.0),
            rect(867.0, 32.0, 859.0, 1081.0),
            rect(4.0, 1149.0, 1720.0, 1081.0), // the row below, mid-jump
        ];
        for frame in frames {
            let (from, to) = crate::motion::surface::surface_travel(frame, from_offset, to_offset, false);
            assert_eq!(from, frame, "at rest the viewport offset is zero");
            assert_eq!(to.origin.x - from.origin.x, 861.0);
            assert_eq!(to.origin.y - from.origin.y, -1117.0);
            assert_eq!(to.size, frame.size, "a strip movement never resizes");
        }
    }

    /// A floating window does not belong to the strip: a pan leaves it exactly where it stands,
    /// and a standing tile still exists, because the overlay is opaque and omissions vanish.
    #[test]
    fn a_pinned_window_stands_still() {
        let frame = rect(224.0, 95.0, 1280.0, 960.0);
        let (from, to) = crate::motion::surface::surface_travel(frame, CGPoint::new(100.0, 0.0), CGPoint::new(-4000.0, 0.0), true);
        assert_eq!(from, frame);
        assert_eq!(to, frame);
    }

    /// The debug slide invents a start for a window already at rest; correcting that "drift" from
    /// the window server made from equal to and silently killed the whole movement.
    #[test]
    fn an_invented_start_for_a_window_at_rest_is_honoured() {
        let at_rest = rect(4.0, 32.0, 859.0, 1081.0);
        let offset = rect(-396.0, 32.0, 859.0, 1081.0);
        assert!(start_is_synthetic(at_rest, offset, at_rest));
    }

    /// The case `actual_start` exists for: the reactor reports a window at its DESTINATION while
    /// it really sits elsewhere. The real frame differs from the destination, so it is drift, and
    /// the window server's answer must win.
    #[test]
    fn real_drift_is_not_synthetic() {
        let reported = rect(4.0, 32.0, 859.0, 1081.0);
        let destination = rect(867.0, 32.0, 859.0, 1081.0);
        let really_at = rect(400.0, 32.0, 859.0, 1081.0);
        assert!(!start_is_synthetic(really_at, reported, destination));
    }

    /// A request with no motion at all has nothing to honour either way.
    #[test]
    fn a_standing_request_is_not_synthetic() {
        let frame = rect(4.0, 32.0, 859.0, 1081.0);
        assert!(!start_is_synthetic(frame, frame, frame));
    }

    /// The measured miss: a 1720pt Kiro column at x=1439 on a 1728pt display shows 289pt of real
    /// content but only 17% of its area, so the fraction rule read it as a parked sliver and the
    /// overlay painted desktop over it for the length of the animation.
    #[test]
    fn a_wide_window_with_a_real_share_visible_is_drawn() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let kiro = rect(1439.0, 32.0, 1720.0, 1081.0);
        assert!(shows_enough(kiro, display, true));
    }

    /// A park as the layout asks for it shows 40pt at most, and must stay skipped or every park
    /// becomes a tile. Apps clamp the real frame past that; `resolve_start` handles those.
    #[test]
    fn a_parked_sliver_is_still_skipped() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let parked = rect(1688.0, 32.0, 859.0, 1081.0);
        assert!(!shows_enough(parked, display, true));
        // Standing still, even a sliver is drawn: whatever shows of it turns into wallpaper
        // otherwise.
        assert!(shows_enough(parked, display, false));
    }

    /// A request whose start and end match is in the list to be drawn, not to be moved. Placing it
    /// would be an Accessibility round trip that asks an application to put a window where it
    /// already is, and every such write invites another layout pass.
    #[test]
    fn a_window_that_is_not_going_anywhere_is_not_moving() {
        let frame = CGRect::new(CGPoint::new(502.0, 135.0), CGSize::new(723.0, 879.0));
        assert!(!is_moving(frame, frame));
    }

    /// A window standing still with a sliver on screen still has to be drawn: whatever shows of it
    /// would otherwise be replaced by wallpaper for the length of the animation. A window being moved
    /// needs a real share of the display, or every parked sliver ends up as a tile.
    #[test]
    fn a_still_window_earns_its_tile_with_any_part_on_screen() {
        assert!(min_on_screen(false) < min_on_screen(true));
        assert!(min_on_screen(false) > 0.0, "entirely off screen is still not worth drawing");
        assert_eq!(min_on_screen(true), 0.25);
    }

    #[test]
    fn a_window_that_changes_position_or_size_is_moving() {
        let frame = CGRect::new(CGPoint::new(4.0, 32.0), CGSize::new(859.0, 1081.0));
        let moved = CGRect::new(CGPoint::new(865.0, 32.0), CGSize::new(859.0, 1081.0));
        let lowered = CGRect::new(CGPoint::new(4.0, 1149.0), CGSize::new(859.0, 1081.0));
        let widened = CGRect::new(CGPoint::new(4.0, 32.0), CGSize::new(1720.0, 1081.0));
        assert!(is_moving(frame, moved));
        assert!(is_moving(frame, lowered));
        assert!(is_moving(frame, widened));
    }

    /// Change 5 of `.kiro/specs/exit-entrance-animation-regressions`: the park remap is decided
    /// from both the server's frame and the request's, before the synthetic-start test.
    mod park_remap {
        use super::*;

        const SLOT: CGRect = CGRect {
            origin: CGPoint { x: 4.0, y: 32.0 },
            size: CGSize { width: 1720.0, height: 1081.0 },
        };
        const PARK: CGRect = CGRect {
            origin: CGPoint { x: 1727.0, y: 1116.0 },
            size: CGSize { width: 1720.0, height: 1081.0 },
        };
        const RIGHT_EDGE: CGRect = CGRect {
            origin: CGPoint { x: 1728.0, y: 32.0 },
            size: CGSize { width: 1720.0, height: 1081.0 },
        };

        /// Kiro's park shows 41pt: `is_off_screen` says visible, the requested park says parked.
        #[test]
        fn a_41pt_park_enters_from_the_right_edge() {
            let real = rect(1727.0, 1076.0, 1720.0, 1081.0);
            assert!(!is_off_screen(DISPLAY, real));
            assert_eq!(resolve_start(Some(real), PARK, SLOT, DISPLAY, None), RIGHT_EDGE);
        }

        /// Finder's park shows 52pt.
        #[test]
        fn a_52pt_park_enters_from_the_right_edge() {
            let real = rect(1727.0, 1065.0, 859.0, 1081.0);
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let park = rect(1727.0, 1116.0, 859.0, 1081.0);
            assert!(!is_off_screen(DISPLAY, real));
            assert_eq!(
                resolve_start(Some(real), park, slot, DISPLAY, None),
                rect(1728.0, 32.0, 859.0, 1081.0)
            );
        }

        /// The server already reports the slot while the reactor still holds the park: the park
        /// wins over the synthetic-start test, so the tile enters from the edge, not the corner.
        #[test]
        fn a_park_the_server_reports_at_its_slot_enters_from_the_edge() {
            assert_eq!(resolve_start(Some(SLOT), PARK, SLOT, DISPLAY, None), RIGHT_EDGE);
        }

        /// A genuine park (1pt showing) with no server answer still enters from the edge.
        #[test]
        fn a_park_with_no_server_answer_enters_from_the_edge() {
            assert_eq!(resolve_start(None, PARK, SLOT, DISPLAY, None), RIGHT_EDGE);
        }

        /// Both frames on screen and the server already at the destination: the request's start is
        /// a deliberate fiction and is honoured.
        #[test]
        fn a_synthetic_start_on_screen_is_still_honoured() {
            let from = rect(4.0, 32.0, 859.0, 1081.0);
            let to = rect(867.0, 32.0, 859.0, 1081.0);
            assert_eq!(resolve_start(Some(to), from, to, DISPLAY, None), from);
        }

        /// Drift with both frames on screen: the server's frame wins.
        #[test]
        fn drift_on_screen_starts_from_the_servers_frame() {
            let from = rect(4.0, 32.0, 859.0, 1081.0);
            let real = rect(120.0, 32.0, 859.0, 1081.0);
            let to = rect(867.0, 32.0, 859.0, 1081.0);
            assert_eq!(resolve_start(Some(real), from, to, DISPLAY, None), real);
        }

        /// For any corner park as the request's start, whatever the server reports (the park, a
        /// clamped park, or the slot), the tile starts in `to`'s row with `to`'s size, just past
        /// the display edge on the park's side.
        #[test]
        fn any_corner_park_enters_from_its_own_edge() {
            let mut rng = Gen(52);
            for _ in 0..RUNS {
                let to = rng.on_screen();
                let from = rng.park(to.size);
                let clamp = rng.pt(0.0, 60.0);
                let real = match rng.below(3) {
                    0 => Some(from),
                    1 => Some(rect(from.origin.x, from.origin.y - clamp, to.size.width, to.size.height)),
                    _ => Some(to),
                };
                let got = resolve_start(real, from, to, DISPLAY, None);
                let parked_left = from.mid().x < DISPLAY.mid().x;
                let expected_x = if parked_left {
                    DISPLAY.origin.x - to.size.width
                } else {
                    DISPLAY.max().x
                };
                assert_eq!(got.origin.y, to.origin.y, "seed 52: row of {to:?}, got {got:?}");
                assert_eq!(got.size, to.size, "seed 52: size of {to:?}, got {got:?}");
                assert_eq!(got.origin.x, expected_x, "seed 52: park {from:?} real {real:?}, got {got:?}");
            }
        }
    }

    /// The mirror of `park_remap`: a window leaving an on-screen slot for a corner park exits in
    /// its own row past the display edge on the park's side (`resolve_end`), while `final_frames`
    /// keeps the real park. Before, the tile slid diagonally into the corner.
    mod park_exit {
        use super::*;

        const SLOT: CGRect = CGRect {
            origin: CGPoint { x: 867.0, y: 32.0 },
            size: CGSize { width: 859.0, height: 1081.0 },
        };
        const RIGHT_PARK: CGRect = CGRect {
            origin: CGPoint { x: 1727.0, y: 1116.0 },
            size: CGSize { width: 859.0, height: 1081.0 },
        };
        const LEFT_PARK: CGRect = CGRect {
            origin: CGPoint { x: -858.0, y: 1116.0 },
            size: CGSize { width: 859.0, height: 1081.0 },
        };

        #[test]
        fn a_window_leaving_for_the_right_park_exits_past_the_right_edge() {
            assert_eq!(
                resolve_end(SLOT, RIGHT_PARK, DISPLAY, None),
                rect(DISPLAY.max().x, SLOT.origin.y, SLOT.size.width, SLOT.size.height)
            );
        }

        #[test]
        fn a_window_leaving_for_the_left_park_exits_past_the_left_edge() {
            assert_eq!(
                resolve_end(SLOT, LEFT_PARK, DISPLAY, None),
                rect(
                    DISPLAY.origin.x - SLOT.size.width,
                    SLOT.origin.y,
                    SLOT.size.width,
                    SLOT.size.height
                )
            );
        }

        #[test]
        fn a_destination_on_screen_is_unchanged() {
            let to = rect(4.0, 32.0, 859.0, 1081.0);
            assert_eq!(resolve_end(SLOT, to, DISPLAY, None), to);
        }

        /// Park to park: nothing shows either way, so the real frame stands.
        #[test]
        fn a_start_already_parked_is_unchanged() {
            assert_eq!(resolve_end(LEFT_PARK, RIGHT_PARK, DISPLAY, None), RIGHT_PARK);
        }

        /// The edge is the display's own, not the global zero.
        #[test]
        fn exit_is_relative_to_the_display_it_happens_on() {
            let display = rect(-670.0, -1692.0, 3008.0, 1692.0);
            let start = rect(-666.0, -1660.0, 859.0, 1081.0);
            let park = rect(2337.0, -1.0, 859.0, 1081.0);
            assert_eq!(
                resolve_end(start, park, display, None),
                rect(2338.0, -1660.0, 859.0, 1081.0)
            );
        }

        /// For any on-screen start and any corner park, the exit is strictly horizontal: the
        /// start's row and size, at the display edge on the park's side.
        #[test]
        fn any_exit_to_a_corner_park_is_horizontal() {
            let mut rng = Gen(53);
            for _ in 0..RUNS {
                let start = rng.on_screen();
                let park = rng.park(start.size);
                let got = resolve_end(start, park, DISPLAY, None);
                let parked_left = park.mid().x < DISPLAY.mid().x;
                let expected_x = if parked_left {
                    DISPLAY.origin.x - start.size.width
                } else {
                    DISPLAY.max().x
                };
                assert_eq!(got.origin.y, start.origin.y, "seed 53: row of {start:?}, got {got:?}");
                assert_eq!(got.size, start.size, "seed 53: size of {start:?}, got {got:?}");
                assert_eq!(got.origin.x, expected_x, "seed 53: park {park:?}, got {got:?}");
            }
        }

        /// Leaving then entering: a window that exited to a park comes back into the same row it
        /// left from, so the round trip is horizontal both ways.
        #[test]
        fn leaving_then_entering_stays_in_the_row() {
            let mut rng = Gen(54);
            for _ in 0..RUNS {
                let slot = rng.on_screen();
                let park = rng.park(slot.size);
                let exit = resolve_end(slot, park, DISPLAY, None);
                let entry = resolve_start(Some(exit), park, slot, DISPLAY, None);
                assert_eq!(entry.origin.y, slot.origin.y, "seed 54: slot {slot:?}, got {entry:?}");
                assert_eq!(entry.size, slot.size, "seed 54: slot {slot:?}, got {entry:?}");
                assert_eq!(entry.origin.x, exit.origin.x, "seed 54: exit {exit:?}, entry {entry:?}");
            }
        }
    }

    /// The strip is one rigid body: a window displaced to a park, or coming back from one, moves
    /// by the same vector as the strip window nearest it (`neighbour_travel`). Aimed at the edge
    /// instead, it covered a different distance under the same duration and curve, so it ran at
    /// its own speed and overlapped its neighbour (seen 2026-09-15). The edge is only the fallback
    /// when nothing beside it moves.
    mod rigid_park {
        use super::*;

        const W: f64 = 859.0;
        const SLOT_A: CGRect = CGRect {
            origin: CGPoint { x: 4.0, y: 32.0 },
            size: CGSize { width: W, height: 1081.0 },
        };
        const SLOT_B: CGRect = CGRect {
            origin: CGPoint { x: 867.0, y: 32.0 },
            size: CGSize { width: W, height: 1081.0 },
        };
        const RIGHT_PARK: CGRect = CGRect {
            origin: CGPoint { x: 1727.0, y: 1116.0 },
            size: CGSize { width: W, height: 1081.0 },
        };


        fn moved(frame: CGRect, dx: f64) -> CGRect {
            translated(frame, CGPoint::new(dx, 0.0))
        }

        /// `start()`'s per-request rule on plain rects: the tile's `(from, to)` for request
        /// `index` of `requests` (`(from, to, floating)`), with no server answer.
        fn tile_for(index: usize, requests: &[(CGRect, CGRect, bool)]) -> (CGRect, CGRect) {
            let (from, to, floating) = requests[index];
            let others: Vec<_> = requests
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != index)
                .map(|(_, r)| *r)
                .collect();
            let travel = (!floating)
                .then(|| neighbour_travel(travel_subject(from, to, DISPLAY), &others, DISPLAY))
                .flatten();
            let start = resolve_start(None, from, to, DISPLAY, travel);
            (start, resolve_end(start, to, DISPLAY, travel))
        }

        #[test]
        fn leaving_travels_by_the_neighbours_vector() {
            // A opens a column: A is pushed right by W, B is pushed off to the park.
            let requests = [(SLOT_A, moved(SLOT_A, W), false), (SLOT_B, RIGHT_PARK, false)];
            let (start, to) = tile_for(1, &requests);
            assert_eq!(start, SLOT_B);
            assert_eq!(to, moved(SLOT_B, W), "not the corner, not the edge");
        }

        #[test]
        fn returning_travels_by_the_neighbours_vector() {
            // A column closes: A comes back left by W, B returns from the park to its slot.
            let requests = [(moved(SLOT_A, W), SLOT_A, false), (RIGHT_PARK, SLOT_B, false)];
            let (from, to) = tile_for(1, &requests);
            assert_eq!(to, SLOT_B);
            assert_eq!(from, moved(SLOT_B, W), "enters from where the strip was");
        }

        #[test]
        fn no_moving_neighbour_falls_back_to_the_edge() {
            let requests = [(SLOT_A, SLOT_A, false), (SLOT_B, RIGHT_PARK, false)];
            let (_, to) = tile_for(1, &requests);
            assert_eq!(to, park_entry_frame(RIGHT_PARK, SLOT_B, DISPLAY));
            let requests = [(SLOT_A, SLOT_A, false), (RIGHT_PARK, SLOT_B, false)];
            let (from, _) = tile_for(1, &requests);
            assert_eq!(from, park_entry_frame(RIGHT_PARK, SLOT_B, DISPLAY));
        }

        #[test]
        fn a_floating_neighbour_lends_no_travel() {
            let requests = [(SLOT_A, moved(SLOT_A, 300.0), true), (SLOT_B, RIGHT_PARK, false)];
            let (_, to) = tile_for(1, &requests);
            assert_eq!(to, park_entry_frame(RIGHT_PARK, SLOT_B, DISPLAY));
        }

        #[test]
        fn a_neighbour_that_only_resizes_lends_no_travel() {
            let grown = rect(4.0, 32.0, W + 200.0, 1081.0);
            let requests = [(SLOT_A, grown, false), (SLOT_B, RIGHT_PARK, false)];
            assert_eq!(neighbour_travel(SLOT_B, &requests[..1], DISPLAY), None);
        }

        #[test]
        fn a_parked_neighbour_lends_no_travel() {
            let left_park = rect(-W + 1.0, 1116.0, W, 1081.0);
            let others = [(left_park, SLOT_A, false), (RIGHT_PARK, moved(RIGHT_PARK, -5.0), false)];
            assert_eq!(neighbour_travel(SLOT_B, &others, DISPLAY), None);
        }

        #[test]
        fn the_nearest_neighbour_by_centre_x_wins() {
            let far = rect(4.0, 32.0, 400.0, 1081.0);
            let near = rect(1200.0, 32.0, 400.0, 1081.0);
            let subject = rect(1500.0, 32.0, 200.0, 1081.0);
            let others = [(far, moved(far, 100.0), false), (near, moved(near, -250.0), false)];
            assert_eq!(neighbour_travel(subject, &others, DISPLAY), Some(CGPoint::new(-250.0, 0.0)));
            let others = [(near, moved(near, -250.0), false), (far, moved(far, 100.0), false)];
            assert_eq!(neighbour_travel(subject, &others, DISPLAY), Some(CGPoint::new(-250.0, 0.0)));
        }

        /// N on-screen columns; an open at index k pushes every column from k on by +W and the
        /// last off to a park. Every tile from k on moves by exactly (W, 0); the rest stand still.
        #[test]
        fn an_open_moves_the_displaced_columns_as_one_body() {
            let mut rng = Gen(61);
            for _ in 0..RUNS {
                let n = rng.below(4) as usize + 2;
                let w = rng.pt(300.0, 1680.0 / n as f64 - 4.0);
                let k = rng.below(n as u64 - 1) as usize;
                let columns: Vec<CGRect> =
                    (0..n).map(|i| rect(4.0 + i as f64 * (w + 4.0), 32.0, w, 1081.0)).collect();
                let park = rect(DISPLAY.max().x - 1.0, DISPLAY.max().y - 1.0, w, 1081.0);
                let mut requests: Vec<(CGRect, CGRect, bool)> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let to = if i < k {
                            *c
                        } else if i == n - 1 {
                            park
                        } else {
                            moved(*c, w)
                        };
                        (*c, to, false)
                    })
                    .collect();
                // The newcomer, already placed at slot k.
                requests.push((columns[k], columns[k], false));
                for i in 0..n {
                    let (from, to) = tile_for(i, &requests);
                    let dx = to.origin.x - from.origin.x;
                    let dy = to.origin.y - from.origin.y;
                    let want = if i >= k { w } else { 0.0 };
                    assert_eq!((dx, dy), (want, 0.0), "seed 61: n={n} w={w} k={k} i={i}");
                    assert_eq!(to.size, from.size, "seed 61: n={n} w={w} k={k} i={i}");
                }
            }
        }

        /// The mirror: a close at index k pulls every column from k on back by -W and the parked
        /// one back onto the strip. Every tile from k on moves by exactly (-W, 0), and the
        /// returning tile enters from `to + (W, 0)`.
        #[test]
        fn a_close_pulls_the_displaced_columns_back_as_one_body() {
            let mut rng = Gen(62);
            for _ in 0..RUNS {
                let n = rng.below(4) as usize + 2;
                let w = rng.pt(300.0, 1680.0 / n as f64 - 4.0);
                let k = rng.below(n as u64 - 1) as usize;
                let columns: Vec<CGRect> =
                    (0..n).map(|i| rect(4.0 + i as f64 * (w + 4.0), 32.0, w, 1081.0)).collect();
                let park = rect(DISPLAY.max().x - 1.0, DISPLAY.max().y - 1.0, w, 1081.0);
                let requests: Vec<(CGRect, CGRect, bool)> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let from = if i < k {
                            *c
                        } else if i == n - 1 {
                            park
                        } else {
                            moved(*c, w)
                        };
                        (from, *c, false)
                    })
                    .collect();
                for i in 0..n {
                    let (from, to) = tile_for(i, &requests);
                    let dx = to.origin.x - from.origin.x;
                    let dy = to.origin.y - from.origin.y;
                    let want = if i >= k { -w } else { 0.0 };
                    assert_eq!((dx, dy), (want, 0.0), "seed 62: n={n} w={w} k={k} i={i}");
                    assert_eq!(to.size, from.size, "seed 62: n={n} w={w} k={k} i={i}");
                    if i == n - 1 {
                        assert_eq!(from, moved(columns[i], w), "seed 62: n={n} w={w} k={k}");
                    }
                }
            }
        }
    }
}
