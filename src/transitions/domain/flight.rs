//! What a flight is.
//!
//! One pass over the layout becomes a flight: whether a pass is worth an overlay, and how far the real
//! windows are at the handover. Decided over ids and frames, with nothing of Core Animation in it.
//! When each step of a flight happens is `crate::transitions::domain::lifecycle`; when the pictures
//! service makes the captures a flight asks for is `crate::pictures::domain::schedule`. Design in
//! `src/transitions/docs/coordination.md`.

use std::collections::HashMap;

use rini_core::ids::WindowId;
use rini_geometry::space::{Rect, ScreenRect};

use super::timing::HANDOVER_THRESHOLD_PT;
use super::travel::is_moving;

/// How far the real windows were from their tiles at lift. See `report_handover_error`.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::transitions) struct HandoverReport {
    /// Windows measured: tiled, answered by the server, intended on screen.
    pub(in crate::transitions) total: usize,
    /// Windows more than `HANDOVER_THRESHOLD_PT` off.
    pub(in crate::transitions) count_over: usize,
    /// The largest error among the windows the report counts.
    pub(in crate::transitions) worst_visible_pt: f64,
    pub(in crate::transitions) worst_wsid: u32,
}

/// Measures every tiled window's real frame against its intended one. Parks are excluded: macOS
/// clamps them. See "Real windows land before lift" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn handover_report(
    final_frames: &[(WindowId, ScreenRect)],
    tiled: &[WindowId],
    real: &HashMap<WindowId, ScreenRect>,
    display: ScreenRect,
) -> HandoverReport {
    let mut report = HandoverReport {
        total: 0,
        count_over: 0,
        worst_visible_pt: 0.0,
        worst_wsid: 0,
    };
    for (window, intended) in final_frames {
        if !tiled.contains(window) {
            continue;
        }
        if rini_geometry::is_off_screen(display, *intended) {
            continue;
        }
        let Some(actual) = real.get(window) else { continue };
        report.total += 1;
        let dx = (actual.origin.x - intended.origin.x).abs();
        let dy = (actual.origin.y - intended.origin.y).abs();
        let error = dx.max(dy);
        if error > HANDOVER_THRESHOLD_PT {
            report.count_over += 1;
        }
        if error > report.worst_visible_pt {
            report.worst_visible_pt = error;
            report.worst_wsid = window.idx.get();
        }
    }
    report
}

/// Whether a composed pass, its drawable tiles going from and to `tiles`, is worth an overlay
/// flight: one of them moves, or a flight is `running` for it to join.
/// See "Layout changes" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn worth_flying<U>(
    mut tiles: impl Iterator<Item = (Rect<U>, Rect<U>)>,
    running: bool,
) -> bool {
    running || tiles.any(|(from, to)| is_moving(from, to))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(idx: u32) -> WindowId { WindowId::new(7, idx) }

    mod handover {
        use rini_geometry::space::{ScreenPoint, ScreenSize};

        use super::*;
        use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, rect};

        type Measured = (
            Vec<(WindowId, ScreenRect)>,
            Vec<WindowId>,
            HashMap<WindowId, ScreenRect>,
        );

        fn measured(frames: &[(WindowId, ScreenRect, ScreenRect)]) -> Measured {
            let final_frames = frames.iter().map(|(w, intended, _)| (*w, *intended)).collect();
            let tiled = frames.iter().map(|(w, _, _)| *w).collect();
            let real = frames.iter().map(|(w, _, actual)| (*w, *actual)).collect();
            (final_frames, tiled, real)
        }

        /// A park clamped by macOS is not the flight's error.
        #[test]
        fn the_handover_report_excludes_off_screen_intents_and_counts_misses() {
            let display = rect(0.0, 0.0, 1728.0, 1117.0);
            let parked = window(108);
            let strip = window(200);
            let final_frames = vec![
                (parked, rect(1727.0, 1116.0, 859.0, 1081.0)),
                (strip, rect(867.0, 32.0, 859.0, 1081.0)),
            ];
            let real: HashMap<WindowId, ScreenRect> = [
                (parked, rect(1727.0, 1051.0, 859.0, 1081.0)),
                (strip, rect(870.0, 32.0, 859.0, 1081.0)),
            ]
            .into_iter()
            .collect();
            let report = handover_report(&final_frames, &[parked, strip], &real, display);
            assert!(
                report.count_over == 1
                    && report.worst_visible_pt == 3.0
                    && report.worst_wsid == 200,
                "expected count_over=1 worst=3pt wsid=200; got {report:?}"
            );
        }

        /// The log's cases: a clamped park excluded, a leaving window's park miss excluded, two
        /// on-screen misses counted.
        #[test]
        fn handover_report_counts_on_screen_misses_only() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            // The park: clamped by macOS, not the flight's error.
            let clamp = [
                (
                    window(108),
                    rect(1727.0, 1116.0, 859.0, 1081.0),
                    rect(1727.0, 1051.0, 859.0, 1081.0),
                ),
                (window(200), slot, rect(870.0, 32.0, 859.0, 1081.0)),
            ];
            let (f, t, r) = measured(&clamp);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 1, "108 is excluded from the measured set");
            assert_eq!(report.count_over, 1);
            assert_eq!(report.worst_visible_pt, 3.0);
            assert_eq!(report.worst_wsid, 200);

            // The leaving window: intended at its park past the right edge, still in its slot.
            let park_miss = [
                (
                    window(1),
                    rect(1728.0, 32.0, 1720.0, 1081.0),
                    rect(-1718.0, 32.0, 1720.0, 1081.0),
                ),
                (
                    window(2),
                    rect(4.0, 32.0, 1720.0, 1081.0),
                    rect(4.0, 32.0, 1720.0, 1081.0),
                ),
            ];
            let (f, t, r) = measured(&park_miss);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 1);
            assert_eq!(report.count_over, 0, "a park intent is not measured");
            assert_eq!(report.worst_visible_pt, 0.0);

            let two = [
                (
                    window(1),
                    rect(4.0, 32.0, 859.0, 1081.0),
                    rect(9.0, 32.0, 859.0, 1081.0),
                ),
                (window(2), slot, rect(867.0, 40.0, 859.0, 1081.0)),
                (
                    window(3),
                    rect(1730.0, 32.0, 859.0, 1081.0),
                    rect(1730.0, 32.0, 859.0, 1081.0),
                ),
            ];
            let (f, t, r) = measured(&two);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 2, "wid 3 starts past the edge: excluded");
            assert_eq!(report.count_over, 2);
            assert_eq!(report.worst_visible_pt, 8.0);
            assert_eq!(report.worst_wsid, 2);

            let clean = [
                (window(1), slot, slot),
                (
                    window(2),
                    rect(4.0, 32.0, 859.0, 1081.0),
                    rect(5.0, 32.0, 859.0, 1081.0),
                ),
            ];
            let (f, t, r) = measured(&clean);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 2);
            assert_eq!(report.count_over, 0, "1pt is within the threshold");
            assert_eq!(report.worst_visible_pt, 1.0);
        }

        #[test]
        fn handover_report_matches_the_brute_force_over_on_screen_intents() {
            let mut rng = Gen(97);
            let mut over_seen = 0usize;
            for _ in 0..RUNS {
                let count = rng.below(6) as usize + 1;
                let mut frames = Vec::with_capacity(count);
                for i in 1..=count {
                    let intended = if rng.coin() {
                        rng.on_screen()
                    } else {
                        rng.park(ScreenSize::new(859.0, 1081.0))
                    };
                    let shift = if rng.coin() { 0.0 } else { rng.pt(-80.0, 80.0) };
                    let (x, y) = (intended.origin.x + shift, intended.origin.y + shift / 2.0);
                    let actual = ScreenRect::new(ScreenPoint::new(x, y), intended.size);
                    frames.push((window(i as u32), intended, actual));
                }
                let (f, t, r) = measured(&frames);
                let report = handover_report(&f, &t, &r, DISPLAY);

                let visible: Vec<_> = frames
                    .iter()
                    .filter(|(_, intended, _)| !rini_geometry::is_off_screen(DISPLAY, *intended))
                    .collect();
                let error = |intended: &ScreenRect, actual: &ScreenRect| {
                    (actual.origin.x - intended.origin.x)
                        .abs()
                        .max((actual.origin.y - intended.origin.y).abs())
                };
                let expected_over = visible.iter().filter(|(_, i, a)| error(i, a) > 2.0).count();
                let expected_worst =
                    visible.iter().map(|(_, i, a)| error(i, a)).fold(0.0, f64::max);
                assert_eq!(report.total, visible.len(), "seed 97");
                assert_eq!(report.count_over, expected_over, "seed 97");
                assert_eq!(report.worst_visible_pt, expected_worst, "seed 97");
                if report.worst_visible_pt > 0.0 {
                    let worst = frames
                        .iter()
                        .find(|(w, _, _)| w.idx.get() == report.worst_wsid)
                        .expect("seed 97: the worst names a measured window");
                    assert!(
                        !rini_geometry::is_off_screen(DISPLAY, worst.1),
                        "seed 97: the worst came from a park"
                    );
                }
                over_seen += expected_over;
            }
            assert!(over_seen > 0, "generator sanity: no misses in {RUNS} runs");
        }
    }

    mod worth_flying {
        use rini_geometry::from_overlay_space;
        use rini_geometry::space::{ScreenPoint, SurfacePoint};

        use super::*;
        use crate::animation::domain::motion::plan::{
            GroupKey, ReflowPlan, RigidGroup, reflow_plan,
        };
        use crate::animation::domain::motion::surface::surface_travel;
        use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, rect};

        const NO_TILES: [(ScreenRect, ScreenRect); 0] = [];

        fn shifted(frame: ScreenRect, dx: f64, dy: f64) -> ScreenRect {
            ScreenRect::new(
                ScreenPoint::new(frame.origin.x + dx, frame.origin.y + dy),
                frame.size,
            )
        }

        fn column(i: f64) -> ScreenRect { rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0) }

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
        fn a_pass_where_nothing_drawable_moves_does_not_fly() {
            assert!(
                !worth_flying(NO_TILES.into_iter(), false),
                "a still-only composition flew, hiding the new window until its picture landed"
            );
        }

        /// A window with no picture is an entrance, not a tile; nothing drawable moves.
        #[test]
        fn a_floating_open_over_a_still_strip_does_not_fly() {
            let s1: ScreenRect = rect(0.0, 32.0, 860.0, 1081.0);
            let s2 = rect(867.0, 32.0, 859.0, 1081.0);
            let tiles = [(s1, s1), (s2, s2)];
            assert!(
                !worth_flying(tiles.iter().copied(), false),
                "the entrance, a floating window with no picture, is reserved and never drawn here"
            );
        }

        #[test]
        fn a_strip_open_that_moves_a_neighbour_flies() {
            let before: ScreenRect = rect(0.0, 32.0, 1720.0, 1081.0);
            let after = rect(0.0, 32.0, 860.0, 1081.0);
            let tiles = [(before, after)];
            assert!(
                worth_flying(tiles.iter().copied(), false),
                "the opened window's own reservation aside"
            );
        }

        #[test]
        fn a_pan_flies() {
            let frame = rect(867.0, 32.0, 859.0, 1081.0);
            let (from, to) = surface_travel(
                frame,
                SurfacePoint::origin(),
                SurfacePoint::new(867.0, 0.0),
                false,
            );
            let tiles = [(
                from_overlay_space(from, DISPLAY),
                from_overlay_space(to, DISPLAY),
            )];
            assert!(worth_flying(tiles.iter().copied(), false));
        }

        #[test]
        fn a_still_pass_joining_a_running_flight_flies() {
            let s1: ScreenRect = rect(0.0, 32.0, 860.0, 1081.0);
            let tiles = [(s1, s1)];
            assert!(worth_flying(tiles.iter().copied(), true));
            assert!(
                worth_flying(NO_TILES.into_iter(), true),
                "even with nothing drawable at all"
            );
        }

        #[test]
        fn any_moving_tile_is_enough_to_fly() {
            let mut rng = Gen(81);
            let mut grounded = 0usize;
            for _ in 0..RUNS {
                let count = rng.below(4) as usize + 1;
                // Half the passes are still-only, so the grounded branch is exercised often.
                let all_still = rng.coin();
                let mut tiles = Vec::with_capacity(count);
                let mut any_moving = false;
                for _ in 0..count {
                    let from: ScreenRect = rng.on_screen();
                    let to = if all_still || rng.coin() {
                        from
                    } else {
                        rect(
                            from.origin.x + rng.pt(1.0, 400.0),
                            32.0,
                            from.size.width,
                            from.size.height,
                        )
                    };
                    any_moving |= is_moving(from, to);
                    tiles.push((from, to));
                }
                let running = rng.coin();
                let flies = worth_flying(tiles.iter().copied(), running);
                assert_eq!(flies, any_moving || running, "seed 81");
                if !flies {
                    grounded += 1;
                }
            }
            assert!(grounded > RUNS / 20, "generator sanity: {grounded} grounded");
        }

        /// The closed window is not in the pass at all.
        #[test]
        fn a_close_composes_only_the_survivors_and_worth_flying_needs_a_mover() {
            let w = 863.0;
            let closed = window(2);
            let survivors: Vec<(WindowId, ScreenRect, ScreenRect, bool)> = [1u32, 3, 4]
                .iter()
                .map(|&i| {
                    let f = column(i as f64);
                    let to = if i > 2 { shifted(f, -w, 0.0) } else { f };
                    (window(i), f, to, false)
                })
                .collect();
            let plan = reflow_plan(&survivors, DISPLAY);
            assert!(
                plan.member(closed).is_none(),
                "nothing is drawn for the closed window"
            );
            assert_eq!(moving(&plan).len(), 1);
            assert_eq!(members(moving(&plan)[0]), vec![window(3), window(4)]);
            let tiles = survivors.iter().map(|(_, from, to, _)| (*from, *to));
            assert!(worth_flying(tiles, false));
            assert!(!worth_flying(NO_TILES.into_iter(), false));
            assert!(worth_flying(NO_TILES.into_iter(), true));
        }
    }
}
