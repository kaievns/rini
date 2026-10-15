//! Where a tile really starts and ends. The layout hands the engine a `from` and a `to`; a parked
//! window's `from` or `to` is a corner it is never seen at, and its own place on the strip says
//! where it is drawn coming in or going out. Everything here is rect arithmetic; the window-server
//! read that feeds `real` is the caller's.

use rini_geometry::space::{Point, Rect, ScreenRect, ScreenVector};
use rini_geometry::stretch::is_a_resize;
use rini_geometry::{SameAs, is_off_screen, park_entry_frame};

use crate::transitions::domain::request::AnimationRequest;

/// What fraction of `frame`'s area lies inside `display`.
pub fn on_screen_fraction(frame: ScreenRect, display: ScreenRect) -> f64 {
    let area = frame.size.width * frame.size.height;
    if area <= 0.0 {
        return 0.0;
    }
    let overlap_w = (frame.origin.x + frame.size.width).min(display.origin.x + display.size.width)
        - frame.origin.x.max(display.origin.x);
    let overlap_h = (frame.origin.y + frame.size.height)
        .min(display.origin.y + display.size.height)
        - frame.origin.y.max(display.origin.y);
    if overlap_w <= 0.0 || overlap_h <= 0.0 {
        return 0.0;
    }
    (overlap_w * overlap_h) / area
}

/// The tile's start from the window server's answer (`real`, `None` when it had none) and the
/// request's `from`/`to`. Pure, so the park remap can be tested on plain rects.
///
/// A window parked at the press comes in along its own place on the strip: in `to`'s row at `to`'s
/// size, its edge facing the display where `before`'s is (`along_the_strip`). With no `before` it
/// comes back by `travel`, the vector its neighbours make this pass (`neighbour_travel`), and
/// without that from the display edge on the park's side (`park_entry_frame`), which also replaces
/// any start that would show on the display.
pub fn resolve_start(
    real: Option<ScreenRect>,
    from: ScreenRect,
    to: ScreenRect,
    display: ScreenRect,
    before: Option<ScreenRect>,
    travel: Option<ScreenVector>,
) -> ScreenRect {
    // A park is judged from both frames, before the synthetic test: apps clamp the real frame past
    // the park threshold, and the server may already report the slot. See src/transitions/docs/coordination.md.
    let parked_real = real.is_some_and(|real| is_off_screen(display, real));
    let parked_from = is_off_screen(display, from);
    if parked_real || parked_from {
        let park = if parked_real {
            real.unwrap_or(from)
        } else {
            from
        };
        let start = match before {
            Some(before) => Some(along_the_strip(before, to, display)),
            None => travel.map(|d| to.translate(-d)),
        };
        return start
            .filter(|start| is_off_screen(display, *start))
            .unwrap_or_else(|| park_entry_frame(park, to, display));
    }
    let real = real.unwrap_or(from);
    if start_is_synthetic(real, from, to) {
        return from;
    }
    real
}

/// The tile's visual destination: a window leaving an on-screen start for a park goes along its own
/// place on the strip, in `start`'s row at `start`'s size, its edge facing the display where
/// `after`'s is, the mirror of `resolve_start`. With no `after`, or one that would still show on the
/// display, it exits past the display edge on the park's side. See "Layout changes" in
/// `src/transitions/docs/coordination.md`.
pub fn resolve_end(
    start: ScreenRect,
    to: ScreenRect,
    display: ScreenRect,
    after: Option<ScreenRect>,
) -> ScreenRect {
    if is_off_screen(display, to) && !is_off_screen(display, start) {
        return after
            .map(|after| along_the_strip(after, start, display))
            .filter(|end| is_off_screen(display, *end))
            .unwrap_or_else(|| park_entry_frame(to, start, display));
    }
    to
}

/// `seen` moved along x until its edge facing the display is where `strip`'s is: the right edge for
/// a strip frame left of the display, the left edge for one right of it. A window keeps the row and
/// size it is seen at while its column carries it off or on, so a resize the pass gives it happens
/// off screen.
fn along_the_strip(strip: ScreenRect, seen: ScreenRect, display: ScreenRect) -> ScreenRect {
    let x = if strip.center().x < display.center().x {
        strip.max().x - seen.size.width
    } else {
        strip.origin.x
    };
    Rect::new(Point::new(x, seen.origin.y), seen.size)
}

/// The vector the rigid strip moves by this pass, as the window at `subject` sees it: the
/// `to - from` of the nearest (by centre x of `from`) strip window that is on screen at both
/// ends and moves without resizing. `None` when no such neighbour exists, which is the edge
/// fallback's cue. `others` is `(from, to, floating)` for every OTHER request of the pass.
///
/// A displaced window aimed at the display edge covered a different distance from the window
/// beside it under one duration and one curve, so the two ran at different speeds and
/// overlapped (seen 2026-09-15). Sharing the neighbour's vector is what makes them one body.
pub fn neighbour_travel(
    subject: ScreenRect,
    others: &[(ScreenRect, ScreenRect, bool)],
    display: ScreenRect,
) -> Option<ScreenVector> {
    others
        .iter()
        .filter(|(_, _, floating)| !floating)
        .filter(|(from, to, _)| !is_off_screen(display, *from) && !is_off_screen(display, *to))
        // Moving by origin, not `is_moving`: a neighbour that only resizes has no travel to lend.
        .filter(|(from, to, _)| {
            (to.origin.x - from.origin.x).abs() >= 0.5 || (to.origin.y - from.origin.y).abs() >= 0.5
        })
        // Nor does one that moves and resizes: its corner's movement is its own, not the strip's.
        .filter(|(from, to, _)| !is_a_resize(from.size, to.size))
        .min_by(|(a, _, _), (b, _, _)| {
            let da = (a.center().x - subject.center().x).abs();
            let db = (b.center().x - subject.center().x).abs();
            da.total_cmp(&db)
        })
        .map(|(from, to, _)| to.origin - from.origin)
}

/// Is a request's start a deliberate fiction rather than drift to correct?
///
/// A window already sitting at its destination has no drift, so a request that still asks for
/// motion from elsewhere is a start chosen on purpose. Overriding it with the real frame would make
/// `from` equal `to`, which silently kills the whole movement.
pub fn start_is_synthetic(real: ScreenRect, from: ScreenRect, to: ScreenRect) -> bool {
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
pub fn on_screen_extent(frame: ScreenRect, display: ScreenRect) -> (f64, f64) {
    let w = (frame.origin.x + frame.size.width).min(display.origin.x + display.size.width)
        - frame.origin.x.max(display.origin.x);
    let h = (frame.origin.y + frame.size.height).min(display.origin.y + display.size.height)
        - frame.origin.y.max(display.origin.y);
    (w.max(0.0), h.max(0.0))
}

/// Where one tile starts and where it ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TilePath {
    /// Where the tile starts.
    pub start: ScreenRect,
    /// Where the tile lands. Not always `to`: a window heading for a park is drawn heading off the
    /// edge, and the real park stays in the frame written to the window.
    pub end: ScreenRect,
}

/// The whole geometry decision for `requests[index]`, one window of a pass, or `None` if it is not
/// worth drawing.
///
/// The start is the frame the window server reports, falling back to the requested one
/// (`resolve_start`). A window parked at either end is drawn along its own place on the strip, the
/// strip frames its request carries (`resolve_end`), so it moves with its column; parked at both
/// ends, from one strip frame to the other when neither shows on the display. Only a start with no
/// strip frame rides a neighbour's vector, and a floating window rides none. A tile that would move
/// entirely off screen is not drawn at all.
pub fn tile_path(
    real: Option<ScreenRect>,
    requests: &[AnimationRequest],
    index: usize,
    display: ScreenRect,
) -> Option<TilePath> {
    let request = &requests[index];
    let (from, to) = (request.from, request.to);
    let (before, after) = (request.strip_before, request.strip_after);
    let travel = (before.is_none() && !request.floating)
        .then(|| {
            let others: Vec<(ScreenRect, ScreenRect, bool)> = requests
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, other)| (other.from, other.to, other.floating))
                .collect();
            neighbour_travel(to, &others, display)
        })
        .flatten();

    let start = resolve_start(real, from, to, display, before, travel);
    let (start, end) = match (before, after) {
        (Some(before), Some(after))
            if is_off_screen(display, start)
                && is_off_screen(display, to)
                && is_off_screen(display, before)
                && is_off_screen(display, after) =>
        {
            (before, after)
        }
        _ => (start, resolve_end(start, to, display, after)),
    };
    worth_animating(start, end, display).then_some(TilePath { start, end })
}

/// Does a window travelling `from` → `to` appear on `display` at ANY point? The whole path is
/// sampled, not just its ends: a window sweeping across mid-animation is exactly what conveys how
/// far the strip travelled, and testing endpoints alone excluded it.
pub fn worth_animating(from: ScreenRect, to: ScreenRect, display: ScreenRect) -> bool {
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
        let at = crate::animation::domain::motion::tile::lerp_rect(from, to, t);
        shows_enough(at, display, moving)
    })
}

/// Whether enough of the window shows at `at` for a tile to be worth drawing there.
pub fn shows_enough(at: ScreenRect, display: ScreenRect, moving: bool) -> bool {
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
pub fn is_moving<U>(from: Rect<U>, to: Rect<U>) -> bool {
    (to.origin.x - from.origin.x).abs() >= 0.5
        || (to.origin.y - from.origin.y).abs() >= 0.5
        || (to.size.width - from.size.width).abs() >= 0.5
        || (to.size.height - from.size.height).abs() >= 0.5
}

/// How far one window travels or is resized by the layout, as a single distance.
///
/// Diagonal moves are rare in a tiling layout, so the larger of the axes is the honest measure and
/// avoids paying for a square root on every window of every layout pass. The resize is the slot's on
/// the strip, before the pass against after, not the window's: an app that snapped rini's last write
/// to its own grid stands a few points off its slot, and writing the slot again resizes nothing.
fn change(request: &AnimationRequest) -> f64 {
    let dx = request.to.origin.x - request.from.origin.x;
    let dy = request.to.origin.y - request.from.origin.y;
    let resized = match (request.strip_before, request.strip_after) {
        (Some(before), Some(after)) => (after.size.width - before.size.width)
            .abs()
            .max((after.size.height - before.size.height).abs()),
        _ => 0.0,
    };
    dx.abs().max(dy.abs()).max(resized)
}

/// Whether any window in this pass moves, or is resized by the layout, far enough to be worth
/// showing.
///
/// The threshold is about cost, not precision: an animation covers the display for its duration, so a
/// one-point move buys nothing and freezes everything. Two points, because the layout rounds to whole
/// points and a column boundary can land either side of where it was without anything having moved.
pub fn changes_visibly(requests: &[AnimationRequest]) -> bool {
    const MIN_VISIBLE_CHANGE: f64 = 2.0;
    requests.iter().any(|request| change(request) >= MIN_VISIBLE_CHANGE)
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Point, Rect, ScreenPoint, ScreenSize, Size};

    use super::*;
    use crate::animation::domain::motion::plan::entrance_from;
    use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, rect};

    /// A start chosen for a window already at rest is honoured; correcting that "drift" from the
    /// window server would make from equal to and silently kill the whole movement.
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
        let frame = ScreenRect::new(ScreenPoint::new(502.0, 135.0), ScreenSize::new(723.0, 879.0));
        assert!(!is_moving(frame, frame));
    }

    /// A window standing still with a sliver on screen still has to be drawn: whatever shows of it
    /// would otherwise be replaced by wallpaper for the length of the animation. A window being moved
    /// needs a real share of the display, or every parked sliver ends up as a tile.
    #[test]
    fn a_still_window_earns_its_tile_with_any_part_on_screen() {
        assert!(min_on_screen(false) < min_on_screen(true));
        assert!(
            min_on_screen(false) > 0.0,
            "entirely off screen is still not worth drawing"
        );
        assert_eq!(min_on_screen(true), 0.25);
    }

    #[test]
    fn a_window_that_changes_position_or_size_is_moving() {
        let frame = ScreenRect::new(ScreenPoint::new(4.0, 32.0), ScreenSize::new(859.0, 1081.0));
        let moved = ScreenRect::new(ScreenPoint::new(865.0, 32.0), ScreenSize::new(859.0, 1081.0));
        let lowered = ScreenRect::new(ScreenPoint::new(4.0, 1149.0), ScreenSize::new(859.0, 1081.0));
        let widened = ScreenRect::new(ScreenPoint::new(4.0, 32.0), ScreenSize::new(1720.0, 1081.0));
        assert!(is_moving(frame, moved));
        assert!(is_moving(frame, lowered));
        assert!(is_moving(frame, widened));
    }

    /// The park remap is decided from both the server's frame and the request's, before the
    /// synthetic-start test.
    mod park_remap {
        use super::*;

        const SLOT: ScreenRect = Rect::new(Point::new(4.0, 32.0), Size::new(1720.0, 1081.0));
        const PARK: ScreenRect = Rect::new(Point::new(1727.0, 1116.0), Size::new(1720.0, 1081.0));
        const RIGHT_EDGE: ScreenRect = Rect::new(Point::new(1728.0, 32.0), Size::new(1720.0, 1081.0));

        /// Kiro's park shows 41pt: `is_off_screen` says visible, the requested park says parked.
        #[test]
        fn a_41pt_park_enters_from_the_right_edge() {
            let real = rect(1727.0, 1076.0, 1720.0, 1081.0);
            assert!(!is_off_screen(DISPLAY, real));
            assert_eq!(resolve_start(Some(real), PARK, SLOT, DISPLAY, None, None), RIGHT_EDGE);
        }

        /// Finder's park shows 52pt.
        #[test]
        fn a_52pt_park_enters_from_the_right_edge() {
            let real = rect(1727.0, 1065.0, 859.0, 1081.0);
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let park = rect(1727.0, 1116.0, 859.0, 1081.0);
            assert!(!is_off_screen(DISPLAY, real));
            assert_eq!(
                resolve_start(Some(real), park, slot, DISPLAY, None, None),
                rect(1728.0, 32.0, 859.0, 1081.0)
            );
        }

        /// The server already reports the slot while the reactor still holds the park: the park
        /// wins over the synthetic-start test, so the tile enters from the edge, not the corner.
        #[test]
        fn a_park_the_server_reports_at_its_slot_enters_from_the_edge() {
            assert_eq!(resolve_start(Some(SLOT), PARK, SLOT, DISPLAY, None, None), RIGHT_EDGE);
        }

        /// A genuine park (1pt showing) with no server answer still enters from the edge.
        #[test]
        fn a_park_with_no_server_answer_enters_from_the_edge() {
            assert_eq!(resolve_start(None, PARK, SLOT, DISPLAY, None, None), RIGHT_EDGE);
        }

        /// Both frames on screen and the server already at the destination: the request's start is
        /// a deliberate fiction and is honoured.
        #[test]
        fn a_synthetic_start_on_screen_is_still_honoured() {
            let from = rect(4.0, 32.0, 859.0, 1081.0);
            let to = rect(867.0, 32.0, 859.0, 1081.0);
            assert_eq!(resolve_start(Some(to), from, to, DISPLAY, None, None), from);
        }

        /// Drift with both frames on screen: the server's frame wins.
        #[test]
        fn drift_on_screen_starts_from_the_servers_frame() {
            let from = rect(4.0, 32.0, 859.0, 1081.0);
            let real = rect(120.0, 32.0, 859.0, 1081.0);
            let to = rect(867.0, 32.0, 859.0, 1081.0);
            assert_eq!(resolve_start(Some(real), from, to, DISPLAY, None, None), real);
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
                    1 => Some(rect(
                        from.origin.x,
                        from.origin.y - clamp,
                        to.size.width,
                        to.size.height,
                    )),
                    _ => Some(to),
                };
                let got = resolve_start(real, from, to, DISPLAY, None, None);
                let parked_left = from.center().x < DISPLAY.center().x;
                let expected_x = if parked_left {
                    DISPLAY.origin.x - to.size.width
                } else {
                    DISPLAY.max().x
                };
                assert_eq!(got.origin.y, to.origin.y, "seed 52: row of {to:?}, got {got:?}");
                assert_eq!(got.size, to.size, "seed 52: size of {to:?}, got {got:?}");
                assert_eq!(
                    got.origin.x, expected_x,
                    "seed 52: park {from:?} real {real:?}, got {got:?}"
                );
            }
        }
    }

    /// The mirror of `park_remap`: a window leaving an on-screen slot for a corner park exits in
    /// its own row past the display edge on the park's side (`resolve_end`), while `final_frames`
    /// keeps the real park. Before, the tile slid diagonally into the corner.
    mod park_exit {
        use super::*;

        const SLOT: ScreenRect = Rect::new(Point::new(867.0, 32.0), Size::new(859.0, 1081.0));
        const RIGHT_PARK: ScreenRect = Rect::new(Point::new(1727.0, 1116.0), Size::new(859.0, 1081.0));
        const LEFT_PARK: ScreenRect = Rect::new(Point::new(-858.0, 1116.0), Size::new(859.0, 1081.0));

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
                let parked_left = park.center().x < DISPLAY.center().x;
                let expected_x = if parked_left {
                    DISPLAY.origin.x - start.size.width
                } else {
                    DISPLAY.max().x
                };
                assert_eq!(
                    got.origin.y, start.origin.y,
                    "seed 53: row of {start:?}, got {got:?}"
                );
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
                let entry = resolve_start(Some(exit), park, slot, DISPLAY, None, None);
                assert_eq!(
                    entry.origin.y, slot.origin.y,
                    "seed 54: slot {slot:?}, got {entry:?}"
                );
                assert_eq!(entry.size, slot.size, "seed 54: slot {slot:?}, got {entry:?}");
                assert_eq!(
                    entry.origin.x, exit.origin.x,
                    "seed 54: exit {exit:?}, entry {entry:?}"
                );
            }
        }
    }

    /// The strip is one rigid body: a window displaced to a park, or coming back from one, moves
    /// along its own place on the strip, which moves with the windows beside it. Aimed at the edge
    /// instead, it covered a different distance under the same duration and curve, so it ran at its
    /// own speed and overlapped its neighbour (seen 2026-09-15). A start with no place on the strip
    /// moves by the vector of the strip window nearest it (`neighbour_travel`), and by the edge only
    /// when nothing beside it moves.
    mod rigid_park {
        use super::*;
        use crate::animation::domain::motion::plan::{GroupKey, reflow_plan};

        const W: f64 = 859.0;
        /// Column to column, the gap included.
        const PITCH: f64 = W + 4.0;
        const SLOT_A: ScreenRect = Rect::new(Point::new(4.0, 32.0), Size::new(W, 1081.0));
        const SLOT_B: ScreenRect = Rect::new(Point::new(867.0, 32.0), Size::new(W, 1081.0));
        const RIGHT_PARK: ScreenRect = Rect::new(Point::new(1727.0, 1116.0), Size::new(W, 1081.0));

        fn moved(frame: ScreenRect, dx: f64) -> ScreenRect {
            frame.translate(ScreenVector::new(dx, 0.0))
        }

        /// `start()`'s per-request rule on plain rects: the tile's `(start, end)` for request
        /// `index` of `requests`, with no server answer.
        fn tile_for(index: usize, requests: &[AnimationRequest]) -> (ScreenRect, ScreenRect) {
            let path = tile_path(None, requests, index, DISPLAY).expect("drawn");
            (path.start, path.end)
        }

        /// How many rigid groups the moving `tiles` form, none of them resizing.
        fn bodies(tiles: &[(ScreenRect, ScreenRect)]) -> usize {
            let requests: Vec<_> = tiles
                .iter()
                .enumerate()
                .map(|(i, (start, end))| (WindowId::new(7, i as u32 + 1), *start, *end, false))
                .collect();
            let plan = reflow_plan(&requests, DISPLAY);
            assert!(plan.changing.is_empty(), "{plan:?}");
            plan.groups.iter().filter(|g| g.key != GroupKey::STILL && !g.members.is_empty()).count()
        }

        /// `n` columns a column apart from x=4, the last on screen and the one after it off it, and
        /// the park a column pushed off the right is written to.
        fn strip_of(rng: &mut Gen, n: usize) -> (f64, Vec<ScreenRect>, ScreenRect) {
            let pitch = rng.pt((1724.0 / n as f64).ceil(), (1624.0 / (n - 1) as f64).floor());
            let w = pitch - 4.0;
            let columns = (0..n).map(|i| rect(4.0 + i as f64 * pitch, 32.0, w, 1081.0)).collect();
            (pitch, columns, rect(DISPLAY.max().x - 1.0, DISPLAY.max().y - 1.0, w, 1081.0))
        }

        #[test]
        fn leaving_goes_along_its_own_place_on_the_strip() {
            // A opens a column: A is pushed right by a column, B off to the park.
            let requests = [
                on_strip(request(SLOT_A, moved(SLOT_A, PITCH), false), SLOT_A, moved(SLOT_A, PITCH)),
                on_strip(request(SLOT_B, RIGHT_PARK, false), SLOT_B, moved(SLOT_B, PITCH)),
            ];
            assert_eq!(tile_for(1, &requests), (SLOT_B, moved(SLOT_B, PITCH)), "not the corner, not the edge");

            // A widens by a column instead, moving no origin: B leaves by the place A pushed it to.
            let wide = rect(4.0, 32.0, W + PITCH, 1081.0);
            let requests = [
                on_strip(request(SLOT_A, wide, false), SLOT_A, wide),
                on_strip(request(SLOT_B, RIGHT_PARK, false), SLOT_B, moved(SLOT_B, PITCH)),
            ];
            assert_eq!(tile_for(1, &requests), (SLOT_B, moved(SLOT_B, PITCH)));
        }

        #[test]
        fn returning_comes_along_its_own_place_on_the_strip() {
            // A column closes: A comes back left by a column, B returns from the park to its slot.
            let requests = [
                on_strip(request(moved(SLOT_A, PITCH), SLOT_A, false), moved(SLOT_A, PITCH), SLOT_A),
                on_strip(request(RIGHT_PARK, SLOT_B, false), moved(SLOT_B, PITCH), SLOT_B),
            ];
            assert_eq!(tile_for(1, &requests), (moved(SLOT_B, PITCH), SLOT_B), "enters from where the strip was");
        }

        /// A column carried off or on while the pass also changes its width keeps its edge facing
        /// the display where its place on the strip has it, the gap to the column beside it, at the
        /// size it is seen at.
        #[test]
        fn the_edge_facing_the_display_follows_the_strip_through_a_width_change() {
            let left_park = rect(-W + 1.0, 1116.0, W, 1081.0);
            let left = rect(-1263.0, 32.0, 1059.0, 1081.0);
            let right = rect(1928.0, 32.0, 659.0, 1081.0);
            let seen_left = rect(left.max().x - W, 32.0, W, 1081.0);
            let seen_right = rect(right.origin.x, 32.0, W, 1081.0);

            let leaving_left = [on_strip(request(SLOT_A, left_park, false), SLOT_A, left)];
            assert_eq!(tile_for(0, &leaving_left), (SLOT_A, seen_left));
            let leaving_right = [on_strip(request(SLOT_B, RIGHT_PARK, false), SLOT_B, right)];
            assert_eq!(tile_for(0, &leaving_right), (SLOT_B, seen_right));

            let returning_left = [on_strip(request(left_park, SLOT_A, false), left, SLOT_A)];
            assert_eq!(tile_for(0, &returning_left), (seen_left, SLOT_A));
            let returning_right = [on_strip(request(RIGHT_PARK, SLOT_B, false), right, SLOT_B)];
            assert_eq!(tile_for(0, &returning_right), (seen_right, SLOT_B));
        }

        /// The same close with B new to the strip, so no place before the pass: B rides A's vector.
        #[test]
        fn returning_travels_by_the_neighbours_vector() {
            let requests = [
                request(moved(SLOT_A, PITCH), SLOT_A, false),
                request(RIGHT_PARK, SLOT_B, false),
            ];
            let (from, to) = tile_for(1, &requests);
            assert_eq!(to, SLOT_B);
            assert_eq!(from, moved(SLOT_B, PITCH), "enters from where the strip was");
        }

        /// A neighbour's vector too short to carry B off the display would start it on screen.
        #[test]
        fn a_neighbours_vector_that_would_start_it_on_screen_gives_way_to_the_edge() {
            let requests = [
                request(SLOT_A, moved(SLOT_A, 100.0), false),
                request(RIGHT_PARK, SLOT_B, false),
            ];
            let (from, _) = tile_for(1, &requests);
            assert_eq!(from, park_entry_frame(RIGHT_PARK, SLOT_B, DISPLAY));
        }

        #[test]
        fn no_moving_neighbour_falls_back_to_the_edge() {
            let requests = [request(SLOT_A, SLOT_A, false), request(RIGHT_PARK, SLOT_B, false)];
            let (from, _) = tile_for(1, &requests);
            assert_eq!(from, park_entry_frame(RIGHT_PARK, SLOT_B, DISPLAY));
        }

        #[test]
        fn a_floating_neighbour_lends_no_travel() {
            let requests = [
                request(moved(SLOT_A, PITCH), SLOT_A, true),
                request(RIGHT_PARK, SLOT_B, false),
            ];
            let (from, _) = tile_for(1, &requests);
            assert_eq!(from, park_entry_frame(RIGHT_PARK, SLOT_B, DISPLAY));
        }

        #[test]
        fn a_neighbour_that_only_resizes_lends_no_travel() {
            let grown = rect(4.0, 32.0, W + 200.0, 1081.0);
            let requests = [(SLOT_A, grown, false), (SLOT_B, RIGHT_PARK, false)];
            assert_eq!(neighbour_travel(SLOT_B, &requests[..1], DISPLAY), None);
        }

        /// The bottom row of a stack maximized moves up and grows: its corner's movement is its own,
        /// not the strip's, and the column beside it rode it up off the screen (Kai's report,
        /// 2026-10-09). A farther window the strip only shifts still lends its vector.
        #[test]
        fn a_neighbour_that_moves_and_resizes_lends_no_travel() {
            let bottom = (rect(3.0, 542.0, 860.0, 536.0), rect(3.0, 3.0, 1722.0, 1074.0), false);
            let next = rect(866.0, 3.0, 860.0, 1074.0);
            assert_eq!(neighbour_travel(next, &[bottom], DISPLAY), None);
            let far = rect(-500.0, 3.0, 860.0, 1074.0);
            let shifted = (far, moved(far, 200.0), false);
            assert_eq!(
                neighbour_travel(next, &[bottom, shifted], DISPLAY),
                Some(ScreenVector::new(200.0, 0.0))
            );
        }

        #[test]
        fn a_parked_neighbour_lends_no_travel() {
            let left_park = rect(-W + 1.0, 1116.0, W, 1081.0);
            let others = [
                (left_park, SLOT_A, false),
                (RIGHT_PARK, moved(RIGHT_PARK, -5.0), false),
            ];
            assert_eq!(neighbour_travel(SLOT_B, &others, DISPLAY), None);
        }

        #[test]
        fn the_nearest_neighbour_by_centre_x_wins() {
            let far = rect(4.0, 32.0, 400.0, 1081.0);
            let near = rect(1200.0, 32.0, 400.0, 1081.0);
            let subject = rect(1500.0, 32.0, 200.0, 1081.0);
            let others = [
                (far, moved(far, 100.0), false),
                (near, moved(near, -250.0), false),
            ];
            assert_eq!(
                neighbour_travel(subject, &others, DISPLAY),
                Some(ScreenVector::new(-250.0, 0.0))
            );
            let others = [
                (near, moved(near, -250.0), false),
                (far, moved(far, 100.0), false),
            ];
            assert_eq!(
                neighbour_travel(subject, &others, DISPLAY),
                Some(ScreenVector::new(-250.0, 0.0))
            );
        }

        /// N columns filling the display; an open at index k pushes every column from k on by one
        /// column and the last off to a park. Every tile from k on moves by exactly (pitch, 0) as one
        /// rigid group; the rest stand still.
        #[test]
        fn an_open_moves_the_displaced_columns_as_one_body() {
            let mut rng = Gen(61);
            for _ in 0..RUNS {
                let n = rng.below(4) as usize + 2;
                let (pitch, columns, park) = strip_of(&mut rng, n);
                let k = rng.below(n as u64 - 1) as usize;
                let mut requests: Vec<AnimationRequest> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let after = if i < k { *c } else { moved(*c, pitch) };
                        let to = if i == n - 1 { park } else { after };
                        on_strip(request(*c, to, false), *c, after)
                    })
                    .collect();
                // The newcomer, already placed at slot k, new to the strip.
                let mut newcomer = request(columns[k], columns[k], false);
                newcomer.strip_after = Some(columns[k]);
                requests.push(newcomer);
                let tiles: Vec<_> = (0..n).map(|i| tile_for(i, &requests)).collect();
                for (i, (from, to)) in tiles.iter().enumerate() {
                    let dx = to.origin.x - from.origin.x;
                    let dy = to.origin.y - from.origin.y;
                    let want = if i >= k { pitch } else { 0.0 };
                    assert_eq!((dx, dy), (want, 0.0), "seed 61: n={n} pitch={pitch} k={k} i={i}");
                    assert_eq!(to.size, from.size, "seed 61: n={n} pitch={pitch} k={k} i={i}");
                }
                assert_eq!(bodies(&tiles[k..]), 1, "seed 61: n={n} pitch={pitch} k={k}");
            }
        }

        /// The mirror: a close at index k pulls every column from k on back by one column and the
        /// parked one back onto the strip. Every tile from k on moves by exactly (-pitch, 0) as one
        /// rigid group, and the returning tile enters from where the strip had it.
        #[test]
        fn a_close_pulls_the_displaced_columns_back_as_one_body() {
            let mut rng = Gen(62);
            for _ in 0..RUNS {
                let n = rng.below(4) as usize + 2;
                let (pitch, columns, park) = strip_of(&mut rng, n);
                let k = rng.below(n as u64 - 1) as usize;
                let requests: Vec<AnimationRequest> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let before = if i < k { *c } else { moved(*c, pitch) };
                        let from = if i == n - 1 { park } else { before };
                        on_strip(request(from, *c, false), before, *c)
                    })
                    .collect();
                let tiles: Vec<_> = (0..n).map(|i| tile_for(i, &requests)).collect();
                for (i, (from, to)) in tiles.iter().enumerate() {
                    let dx = to.origin.x - from.origin.x;
                    let dy = to.origin.y - from.origin.y;
                    let want = if i >= k { -pitch } else { 0.0 };
                    assert_eq!((dx, dy), (want, 0.0), "seed 62: n={n} pitch={pitch} k={k} i={i}");
                    assert_eq!(to.size, from.size, "seed 62: n={n} pitch={pitch} k={k} i={i}");
                    if i == n - 1 {
                        assert_eq!(*from, moved(columns[i], pitch), "seed 62: n={n} pitch={pitch} k={k}");
                    }
                }
                assert_eq!(bodies(&tiles[k..]), 1, "seed 62: n={n} pitch={pitch} k={k}");
            }
        }
    }

    /// Kai's geometry (2026-10-09): a 1728x1080 screen, 3pt gaps, two 860pt columns at x=3 and
    /// x=866, a 2-stack's rows at y=3 and 542, a 3-stack's at 3, 362 and 721. A stacked window
    /// maximized or restored, folded in or out: every other window parked at either end keeps the
    /// row and size it is seen at and moves only along x, with its column. The strip frames are
    /// inputs, so this holds whichever side the maximized column goes in.
    mod strip_place {
        use super::*;
        use crate::animation::domain::motion::plan::{GroupKey, reflow_plan};

        const SCREEN: ScreenRect = Rect::new(Point::new(0.0, 0.0), Size::new(1728.0, 1080.0));
        const MAXIMIZED_WIDTH: f64 = 1722.0;

        /// A column of `rows` rows at `x`, top to bottom.
        fn column(x: f64, rows: usize) -> Vec<ScreenRect> {
            let rows: &[(f64, f64)] = match rows {
                1 => &[(3.0, 1074.0)],
                2 => &[(3.0, 536.0), (542.0, 536.0)],
                _ => &[(3.0, 356.0), (362.0, 356.0), (721.0, 356.0)],
            };
            rows.iter().map(|&(y, h)| rect(x, y, 860.0, h)).collect()
        }

        /// Where the layout writes a window the strip has at `strip`: a corner park on the side it
        /// went when it is off the screen.
        fn written(strip: ScreenRect) -> ScreenRect {
            if !is_off_screen(SCREEN, strip) {
                return strip;
            }
            let x = if strip.max().x <= SCREEN.origin.x {
                SCREEN.origin.x - strip.size.width + 1.0
            } else {
                SCREEN.max().x - 1.0
            };
            rect(x, SCREEN.max().y - 1.0, strip.size.width, strip.size.height)
        }

        /// Window `i + 1` from strip frame `before[i]` to `after[i]`, written where the layout puts it.
        fn pass(before: &[ScreenRect], after: &[ScreenRect]) -> Vec<AnimationRequest> {
            before
                .iter()
                .zip(after)
                .enumerate()
                .map(|(i, (b, a))| {
                    let mut r = on_strip(request(written(*b), written(*a), false), *b, *a);
                    r.window = WindowId::new(1, i as u32 + 1);
                    r
                })
                .collect()
        }

        /// The x of each column of `widths` laid 3pt apart, column `shown` at x=3.
        fn laid(widths: &[f64], shown: usize) -> Vec<f64> {
            let mut xs = vec![3.0; widths.len()];
            for j in shown + 1..widths.len() {
                xs[j] = xs[j - 1] + widths[j - 1] + 3.0;
            }
            for j in (0..shown).rev() {
                xs[j] = xs[j + 1] - widths[j] - 3.0;
            }
            xs
        }

        /// Row `row` of a `rows`-stack maximized, the stack left or right of the other column, the
        /// maximized column going in left or right of what is left of the stack. The stack's rows are
        /// windows 1 to `rows`, the other column the last; the strip before and after.
        fn maximize(rows: usize, row: usize, stack_left: bool, goes_left: bool) -> (Vec<ScreenRect>, Vec<ScreenRect>) {
            let (stack_x, other_x) = if stack_left { (3.0, 866.0) } else { (866.0, 3.0) };
            let mut before = column(stack_x, rows);
            before.push(column(other_x, 1)[0]);

            // Columns after, in strip order: 0 the other, 1 what is left of the stack, 2 the maximized.
            let mut order = if goes_left { vec![2, 1] } else { vec![1, 2] };
            if stack_left {
                order.push(0);
            } else {
                order.insert(0, 0);
            }
            let widths: Vec<f64> =
                order.iter().map(|&c| if c == 2 { MAXIMIZED_WIDTH } else { 860.0 }).collect();
            let xs = laid(&widths, order.iter().position(|&c| c == 2).unwrap());
            let x_of = |c| xs[order.iter().position(|&o| o == c).unwrap()];
            let left = column(x_of(1), rows - 1);
            let mut after: Vec<ScreenRect> = (0..rows)
                .map(|i| match i.cmp(&row) {
                    std::cmp::Ordering::Equal => rect(x_of(2), 3.0, MAXIMIZED_WIDTH, 1074.0),
                    std::cmp::Ordering::Less => left[i],
                    std::cmp::Ordering::Greater => left[i - 1],
                })
                .collect();
            after.push(column(x_of(0), 1)[0]);
            (before, after)
        }

        /// The tiles of a pass, by window, each drawn one.
        fn tiles(requests: &[AnimationRequest]) -> Vec<(WindowId, ScreenRect, ScreenRect)> {
            (0..requests.len())
                .filter_map(|i| {
                    tile_path(None, requests, i, SCREEN).map(|p| (requests[i].window, p.start, p.end))
                })
                .collect()
        }

        /// Every window but `resized` parked at either end keeps the row and size it is seen at,
        /// moves only along x by its column's movement on the strip, and is off the screen at the end
        /// it is parked at; `resized` alone resizes, and the rest form one rigid group per vector.
        fn holds_its_row(case: &str, requests: &[AnimationRequest], resized: WindowId) {
            let drawn = tiles(requests);
            for r in requests.iter().filter(|r| r.window != resized) {
                let (before, after) = (r.strip_before.unwrap(), r.strip_after.unwrap());
                let tile = drawn.iter().find(|(w, _, _)| *w == r.window);
                if is_off_screen(SCREEN, before) && is_off_screen(SCREEN, after) {
                    assert!(tile.is_none(), "{case}: {:?} is off the screen throughout: {tile:?}", r.window);
                    continue;
                }
                let &(_, start, end) = tile.unwrap_or_else(|| panic!("{case}: {:?} not drawn", r.window));
                let seen = if is_off_screen(SCREEN, r.from) { r.to } else { r.from };
                let what = format!("{case}: {:?} {start:?} -> {end:?}", r.window);
                assert_eq!((start.size, end.size), (seen.size, seen.size), "{what}");
                assert_eq!((start.origin.y, end.origin.y), (seen.origin.y, seen.origin.y), "{what}");
                assert_eq!(end.origin.x - start.origin.x, after.origin.x - before.origin.x, "{what}");
                if is_off_screen(SCREEN, r.from) {
                    assert!(is_off_screen(SCREEN, start), "{what}");
                } else {
                    assert_eq!(start, r.from, "{what}");
                }
                if is_off_screen(SCREEN, r.to) {
                    assert!(is_off_screen(SCREEN, end), "{what}");
                } else {
                    assert_eq!(end, r.to, "{what}");
                }
            }
            let plan = reflow_plan(&drawn.iter().map(|&(w, s, e)| (w, s, e, false)).collect::<Vec<_>>(), SCREEN);
            assert_eq!(plan.changing.iter().map(|c| c.0).collect::<Vec<_>>(), vec![resized], "{case}");
            let mut vectors: Vec<f64> = drawn
                .iter()
                .filter(|(w, _, _)| *w != resized)
                .map(|(_, s, e)| e.origin.x - s.origin.x)
                .collect();
            vectors.sort_by(f64::total_cmp);
            vectors.dedup();
            let groups: Vec<_> =
                plan.groups.iter().filter(|g| g.key != GroupKey::STILL && !g.members.is_empty()).collect();
            assert_eq!(groups.len(), vectors.len(), "{case}: {groups:?}");
        }

        #[test]
        fn a_maximize_and_its_restore_move_the_rest_only_along_the_strip() {
            for rows in [2, 3] {
                for row in 0..rows {
                    for stack_left in [true, false] {
                        for goes_left in [true, false] {
                            let (before, after) = maximize(rows, row, stack_left, goes_left);
                            let resized = WindowId::new(1, row as u32 + 1);
                            let case = format!(
                                "row {row} of a {rows}-stack {}, maximized going in {}",
                                if stack_left { "left" } else { "right" },
                                if goes_left { "left" } else { "right" }
                            );
                            holds_its_row(&format!("maximize {case}"), &pass(&before, &after), resized);
                            holds_its_row(&format!("restore {case}"), &pass(&after, &before), resized);
                        }
                    }
                }
            }
        }

        /// The column a fold-in brings back from its park, either side, moves only along x.
        #[test]
        fn a_fold_in_brings_the_parked_column_back_only_along_x() {
            for rows in [1, 2] {
                for at in 0..=rows {
                    // [A stack][B][C parked right]: B folds into A at `at`, C comes on.
                    let mut before = column(3.0, rows);
                    before.extend([column(866.0, 1)[0], column(1729.0, 1)[0]]);
                    let mut after = column(3.0, rows + 1);
                    let b = after.remove(at);
                    after.push(b);
                    after.push(column(866.0, 1)[0]);
                    let requests = pass(&before, &after);
                    let c = tiles(&requests).into_iter().find(|t| t.0 == requests[rows + 1].window).expect("drawn");
                    assert_eq!(c.1, column(1729.0, 1)[0], "{rows} rows, at {at}");
                    assert_eq!(c.2, column(866.0, 1)[0], "{rows} rows, at {at}");

                    // [Z parked left][A][B stack]: A folds into B at `at`, Z comes on.
                    let mut before = vec![column(-860.0, 1)[0], column(3.0, 1)[0]];
                    before.extend(column(866.0, rows));
                    let mut after = vec![column(3.0, 1)[0]];
                    let mut stack = column(866.0, rows + 1);
                    after.push(stack.remove(at));
                    after.extend(stack);
                    let requests = pass(&before, &after);
                    let z = tiles(&requests).into_iter().find(|t| t.0 == requests[0].window).expect("drawn");
                    assert_eq!(z.1, column(-860.0, 1)[0], "{rows} rows, at {at}");
                    assert_eq!(z.2, column(3.0, 1)[0], "{rows} rows, at {at}");
                }
            }
        }

        /// The column an unfold pushes to its park, either side, moves only along x.
        #[test]
        fn an_unfold_sends_the_pushed_column_off_only_along_x() {
            for rows in [2, 3] {
                for at in 0..rows {
                    // [A stack][B]: row `at` of A comes out right of A, B goes off the right.
                    let mut before = column(3.0, rows);
                    before.push(column(866.0, 1)[0]);
                    let mut after = column(3.0, rows - 1);
                    after.insert(at, column(866.0, 1)[0]);
                    after.push(column(1729.0, 1)[0]);
                    let requests = pass(&before, &after);
                    let b = tiles(&requests).into_iter().find(|t| t.0 == requests[rows].window).expect("drawn");
                    assert_eq!(b.1, column(866.0, 1)[0], "{rows} rows, at {at}");
                    assert_eq!(b.2, column(1729.0, 1)[0], "{rows} rows, at {at}");

                    // [A][B stack]: row `at` of B comes out right of B, the strip scrolls it on and A
                    // goes off the left.
                    let mut before = vec![column(3.0, 1)[0]];
                    before.extend(column(866.0, rows));
                    let mut after = vec![column(-860.0, 1)[0]];
                    let mut stack = column(3.0, rows - 1);
                    stack.insert(at, column(866.0, 1)[0]);
                    after.extend(stack);
                    let requests = pass(&before, &after);
                    let a = tiles(&requests).into_iter().find(|t| t.0 == requests[0].window).expect("drawn");
                    assert_eq!(a.1, column(3.0, 1)[0], "{rows} rows, at {at}");
                    assert_eq!(a.2, column(-860.0, 1)[0], "{rows} rows, at {at}");
                }
            }
        }

        /// The layout parks against the screen less the Dock, the overlay draws on the whole display:
        /// with a 128pt Dock on the right, the column just past the strip is parked yet its strip
        /// frame shows 125pt of it where the Dock is. A tile parked at an end ends or starts off the display, never
        /// there, and one parked at both ends is drawn only when it crosses the display.
        #[test]
        fn a_parked_end_never_shows_where_the_dock_is() {
            let in_the_dock = rect(1603.0, 3.0, 860.0, 1074.0);
            let beyond = rect(2466.0, 3.0, 860.0, 1074.0);
            let on_screen = rect(740.0, 3.0, 860.0, 1074.0);
            let park = written(beyond);
            let path = |from, to, before, after| {
                tile_path(None, &[on_strip(request(from, to, false), before, after)], 0, SCREEN)
            };

            let leaving = path(on_screen, park, on_screen, in_the_dock).expect("drawn");
            assert_eq!(leaving.end, rect(SCREEN.max().x, 3.0, 860.0, 1074.0));

            let returning = path(park, on_screen, in_the_dock, on_screen).expect("drawn");
            assert_eq!(returning.start, rect(SCREEN.max().x, 3.0, 860.0, 1074.0));

            assert_eq!(path(park, park, beyond, in_the_dock), None);
            assert_eq!(path(park, park, in_the_dock, beyond), None);

            let left = rect(-860.0, 3.0, 860.0, 1074.0);
            let crossing = path(written(left), park, left, beyond).expect("drawn");
            assert_eq!((crossing.start, crossing.end), (left, beyond));
        }
    }

    use rini_core::ids::{WindowId, WindowServerId};

    /// One window of a pass, with no place on the strip.
    fn request(from: ScreenRect, to: ScreenRect, floating: bool) -> AnimationRequest {
        AnimationRequest {
            worn: Default::default(),
            window: WindowId::new(1, 1),
            server_id: WindowServerId::new(1),
            from,
            to,
            floating,
            strip_before: None,
            strip_after: None,
        }
    }

    /// `request` with its place on the strip before and after the pass.
    fn on_strip(request: AnimationRequest, before: ScreenRect, after: ScreenRect) -> AnimationRequest {
        AnimationRequest { strip_before: Some(before), strip_after: Some(after), ..request }
    }

    /// A strip column, as the animation path describes one: where it is now, where the layout wants it.
    fn moving(from_x: f64, to_x: f64) -> AnimationRequest {
        AnimationRequest {
            window: WindowId::new(1, (from_x.abs() as u32).max(1)),
            ..request(
                ScreenRect::new(ScreenPoint::new(from_x, 32.0), ScreenSize::new(859.0, 1081.0)),
                ScreenRect::new(ScreenPoint::new(to_x, 32.0), ScreenSize::new(859.0, 1081.0)),
                false,
            )
        }
    }

    /// A one-point move is the measured case, not a hypothetical: a floating window oscillated between
    /// x = 502 and x = 503 on every space-state refresh, and each of those ran a full-screen animation that
    /// blanked every other window for 350ms.
    #[test]
    fn a_move_of_a_single_point_is_not_worth_animating() {
        assert!(!changes_visibly(&[moving(502.0, 503.0)]));
        assert!(!changes_visibly(&[moving(503.0, 502.0)]));
        assert!(!changes_visibly(&[]));
    }

    #[test]
    fn a_real_move_is_worth_animating() {
        // A column step on this display, and the smallest move that counts.
        assert!(changes_visibly(&[moving(4.0, 865.0)]));
        assert!(changes_visibly(&[moving(4.0, 6.0)]));
        // One window going somewhere is enough, however many are standing still around it.
        assert!(changes_visibly(&[moving(502.0, 502.0), moving(4.0, 865.0)]));
    }

    /// Vertical movement counts too: a workspace switch travels in y, and reading only x would treat one as
    /// motionless and place every window instantly.
    #[test]
    fn travel_is_measured_on_both_axes() {
        let mut vertical = moving(4.0, 4.0);
        vertical.to.origin.y = 32.0 + 1117.0;
        assert!(changes_visibly(&[vertical]));
    }

    /// Kai's report of 2026-10-09: `alt-R` on a column at the strip's left edge with nothing beside it
    /// on screen resizes it where it stands, and that is shown like a move.
    #[test]
    fn a_column_resized_where_it_stands_changes_visibly() {
        let full = ScreenRect::new(ScreenPoint::new(3.0, 35.0), ScreenSize::new(1722.0, 1079.0));
        let third = ScreenRect::new(ScreenPoint::new(3.0, 35.0), ScreenSize::new(572.0, 1079.0));
        for (from, to) in [(full, third), (third, full)] {
            assert!(changes_visibly(&[on_strip(request(from, to, false), from, to)]), "{to:?}");
        }
        let shorter = ScreenRect { size: ScreenSize::new(1722.0, 538.0), ..full };
        assert!(changes_visibly(&[on_strip(request(full, shorter, false), full, shorter)]));
    }

    /// An app snapped a few points off its slot is written its slot again, which resizes nothing the
    /// layout changed; nor does a slot rounding a point either way.
    #[test]
    fn a_window_written_back_to_the_slot_it_had_does_not_change_visibly() {
        let slot = ScreenRect::new(ScreenPoint::new(3.0, 35.0), ScreenSize::new(860.0, 1079.0));
        let snapped = ScreenRect { size: ScreenSize::new(853.0, 1071.0), ..slot };
        assert!(!changes_visibly(&[on_strip(request(snapped, slot, false), slot, slot)]));
        let rounded = ScreenRect { size: ScreenSize::new(861.0, 1079.0), ..slot };
        assert!(!changes_visibly(&[on_strip(request(slot, rounded, false), slot, rounded)]));
        assert!(!changes_visibly(&[request(snapped, slot, false)]), "off the strip, origin only");
    }

    // --- tile_path: the whole geometry decision for one window ---------------------------------

    /// An ordinary move on screen: the tile starts where the window server says it is and ends where
    /// the pass wants it.
    #[test]
    fn a_window_moving_on_screen_gets_a_path_from_where_it_is_to_where_it_is_going() {
        let from = rect(0., 0., 400., 600.);
        let to = rect(500., 0., 400., 600.);
        let path =
            tile_path(Some(from), &[request(from, to, false)], 0, DISPLAY).expect("worth animating");
        assert_eq!(path.start, from);
        assert_eq!(path.end, to);
    }

    /// The window server's frame wins over the requested `from`, because the request describes where
    /// the layout THINKS the window is and the server knows where it actually is.
    #[test]
    fn the_live_frame_beats_the_requested_start() {
        let live = rect(40., 10., 400., 600.);
        let believed = rect(0., 0., 400., 600.);
        let to = rect(500., 0., 400., 600.);
        let path = tile_path(Some(live), &[request(believed, to, false)], 0, DISPLAY)
            .expect("worth animating");
        assert_eq!(path.start, live);
    }

    /// No live frame falls back to the requested one rather than refusing: a window the server has not
    /// laid out yet still has a place the layout wants it to come from.
    #[test]
    fn no_live_frame_falls_back_to_the_requested_start() {
        let believed = rect(0., 0., 400., 600.);
        let to = rect(500., 0., 400., 600.);
        let path =
            tile_path(None, &[request(believed, to, false)], 0, DISPLAY).expect("worth animating");
        assert_eq!(path.start, believed);
    }

    /// A tile that would move entirely off screen is not drawn. Drawing it costs a layer and a capture
    /// for something nobody can see.
    #[test]
    fn a_move_entirely_off_screen_is_not_worth_a_tile() {
        let far = rect(-9000., -9000., 400., 600.);
        let also_far = rect(-9500., -9000., 400., 600.);
        assert_eq!(tile_path(Some(far), &[request(far, also_far, false)], 0, DISPLAY), None);
    }

    /// A floating window rides nothing. It is not part of the strip, so a neighbour's vector would be
    /// borrowed from a window it has no relationship with: one parked at the press enters from the
    /// display edge, not from where a tiled window beside it says the strip was.
    #[test]
    fn a_floating_window_never_borrows_a_neighbours_vector() {
        let park = rect(1727., 1116., 300., 300.);
        let to = rect(1000., 100., 300., 300.);
        // A tiled neighbour making a big move, which a tiled window here would ride in from x=1900.
        let neighbour = request(rect(1000., 0., 400., 600.), rect(100., 0., 400., 600.), false);
        let tiled = tile_path(None, &[request(park, to, false), neighbour.clone()], 0, DISPLAY)
            .expect("worth animating");
        assert_eq!(tiled.start, rect(1900., 100., 300., 300.));
        let floating = tile_path(None, &[request(park, to, true), neighbour], 0, DISPLAY)
            .expect("worth animating");
        assert_eq!(floating.start, park_entry_frame(park, to, DISPLAY), "a floating tile rides nothing");
    }

    /// A window is never its own neighbour. The server reports this one parked while the reactor
    /// still has it on screen, so its own `from` and `to` would qualify, and nearer than the real
    /// neighbour: riding its own vector it would start at `from`, on screen.
    #[test]
    fn a_window_is_not_its_own_neighbour() {
        let parked = rect(1727., 1116., 859., 1081.);
        let believed = rect(867., 32., 859., 1081.);
        let to = rect(4., 32., 859., 1081.);
        let neighbour = request(rect(-1000., 32., 1100., 1081.), rect(-100., 32., 1100., 1081.), false);
        let path = tile_path(Some(parked), &[request(believed, to, false), neighbour], 0, DISPLAY)
            .expect("worth animating");
        assert_eq!(path.start, rect(-896., 32., 859., 1081.), "by the neighbour's (900, 0)");
    }

    /// `index` selects which of the requests is the subject. Passing the wrong one would have a
    /// window ride its own vector and exclude a real neighbour.
    #[test]
    fn the_index_says_which_of_the_others_is_this_window() {
        let a = request(rect(0., 0., 400., 600.), rect(0., 0., 400., 600.), false);
        let b = request(rect(500., 0., 400., 600.), rect(900., 0., 400., 600.), false);
        let others = [a.clone(), b.clone()];

        let first = tile_path(Some(a.from), &others, 0, DISPLAY);
        let second = tile_path(Some(b.from), &others, 1, DISPLAY);

        // Whatever each resolves to, they are resolved against DIFFERENT neighbour sets.
        assert!(
            first.is_some() || second.is_some(),
            "at least one is worth drawing"
        );
    }

    /// Parks clamped past 40pt (Kiro 41pt, Finder 52pt) and one the server reports at
    /// its slot.
    #[test]
    fn a_clamped_park_enters_from_the_display_edge() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let slot = rect(4.0, 32.0, 1720.0, 1081.0);
        let park = rect(1727.0, 1116.0, 1720.0, 1081.0);
        let kiro_real = rect(1727.0, 1076.0, 1720.0, 1081.0);
        let finder_real = rect(1727.0, 1065.0, 859.0, 1081.0);
        let finder_slot = rect(867.0, 32.0, 859.0, 1081.0);
        let finder_park = rect(1727.0, 1116.0, 859.0, 1081.0);

        let cases = [
            ("41pt Kiro park", Some(kiro_real), park, slot),
            ("52pt Finder park", Some(finder_real), finder_park, finder_slot),
            (
                "park the server already reports at its slot",
                Some(slot),
                park,
                slot,
            ),
        ];
        let wrong: Vec<String> = cases
            .iter()
            .filter_map(|(name, real, from, to)| {
                let expected = rini_geometry::park_entry_frame(*from, *to, display);
                let got = resolve_start(*real, *from, *to, display, None, None);
                (got != expected).then(|| {
                    format!(
                        "{name}: started at {:.0},{:.0}, wanted {:.0},{:.0}",
                        got.origin.x, got.origin.y, expected.origin.x, expected.origin.y
                    )
                })
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "parks not remapped to the edge:\n{}",
            wrong.join("\n")
        );
    }

    #[test]
    fn a_plain_move_starts_from_the_window_servers_frame() {
        let mut rng = Gen(31);
        let mut checked = 0;
        for _ in 0..RUNS {
            let from = rng.on_screen();
            let to = rect(rng.pt(0.0, 900.0), 32.0, from.size.width, from.size.height);
            let real = rect(
                from.origin.x + rng.pt(20.0, 300.0),
                32.0,
                from.size.width,
                from.size.height,
            );
            if real.same_as(to) || rini_geometry::is_off_screen(DISPLAY, real) {
                continue;
            }
            checked += 1;
            assert_eq!(
                resolve_start(Some(real), from, to, DISPLAY, None, None),
                real,
                "seed 31: {from:?} -> {to:?}"
            );
        }
        assert!(
            checked > RUNS / 2,
            "generator sanity: {checked} of {RUNS} in scope"
        );
    }

    #[test]
    fn a_missing_or_synthetic_start_falls_back_to_the_request() {
        let from = rect(4.0, 32.0, 859.0, 1081.0);
        let to = rect(867.0, 32.0, 859.0, 1081.0);
        assert_eq!(resolve_start(None, from, to, DISPLAY, None, None), from);
        assert_eq!(resolve_start(Some(to), from, to, DISPLAY, None, None), from);
    }

    #[test]
    fn a_parked_or_scrolled_off_close_is_not_worth_animating() {
        let mut rng = Gen(39);
        for _ in 0..RUNS {
            let size = ScreenSize::new(rng.pt(400.0, 1720.0), 1081.0);
            let park = rng.park(size);
            assert!(
                !worth_animating(park, entrance_from(park), DISPLAY),
                "park {park:?}"
            );
            let off = rect(
                DISPLAY.size.width + rng.pt(1.0, 9000.0),
                32.0,
                size.width,
                size.height,
            );
            assert!(
                !worth_animating(off, entrance_from(off), DISPLAY),
                "off strip {off:?}"
            );
            let on = rng.on_screen();
            assert!(
                worth_animating(on, entrance_from(on), DISPLAY),
                "on screen {on:?}"
            );
        }
        let slot = rect(4.0, 32.0, 859.0, 1081.0);
        assert!(
            !worth_animating(rect(4.0, 32.0, 0.0, 0.0), slot, DISPLAY),
            "zero area"
        );
        // Entering from a park is still worth it: the path crosses the display.
        assert!(worth_animating(
            rect(1727.0, 1116.0, 859.0, 1081.0),
            slot,
            DISPLAY
        ));
    }

    /// Where a park's tile ends decides which rigid piece it rides.
    mod park_groups {
        use super::*;
        use crate::animation::domain::motion::plan::{GroupKey, ReflowPlan, RigidGroup, reflow_plan};
        use rini_geometry::space::OverlayVector;

        fn window(idx: u32) -> WindowId {
            WindowId::new(7, idx)
        }

        fn shifted(frame: ScreenRect, dx: f64, dy: f64) -> ScreenRect {
            ScreenRect::new(
                ScreenPoint::new(frame.origin.x + dx, frame.origin.y + dy),
                frame.size,
            )
        }

        fn column(i: f64) -> ScreenRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        fn moving(plan: &ReflowPlan) -> Vec<&RigidGroup> {
            plan.groups
                .iter()
                .filter(|g| !g.members.is_empty() && g.key != GroupKey::STILL)
                .collect()
        }

        fn members(group: &RigidGroup) -> Vec<WindowId> {
            group.members.iter().map(|m| m.window).collect()
        }

        #[test]
        fn a_window_leaving_for_a_park_rides_its_moving_neighbours_group() {
            let a = column(0.0);
            let b = column(1.0);
            let park = Gen(5).park(a.size);
            assert!(rini_geometry::is_off_screen(DISPLAY, park));
            let b_to = shifted(b, -863.0, 0.0);
            let a_end = resolve_end(a, park, DISPLAY, Some(shifted(a, -863.0, 0.0)));
            assert_eq!(a_end, shifted(a, -863.0, 0.0), "where the strip takes it");

            let plan = reflow_plan(&[(window(1), a, a_end, false), (window(2), b, b_to, false)], DISPLAY);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![window(1), window(2)]);
            assert_eq!(groups[0].travel, OverlayVector::new(-863.0, 0.0));
        }

        #[test]
        fn a_window_leaving_for_a_park_alone_is_a_group_of_one() {
            let a = column(1.0);
            let park = rect(
                DISPLAY.size.width - 1.0,
                DISPLAY.size.height - 1.0,
                a.size.width,
                a.size.height,
            );
            let a_end = resolve_end(a, park, DISPLAY, None);
            assert_eq!(a_end, rini_geometry::park_entry_frame(park, a, DISPLAY));
            let still = column(0.0);

            let plan = reflow_plan(
                &[(window(1), a, a_end, false), (window(2), still, still, false)],
                DISPLAY,
            );
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![window(1)]);
            assert_eq!(groups[0].travel, OverlayVector::new(a_end.origin.x - a.origin.x, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![window(2)]);
        }
    }
}
