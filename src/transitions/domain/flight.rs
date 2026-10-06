//! What a flight is.
//!
//! One pass over the layout becomes a flight: which unmanaged window traces a tile as its border,
//! whether a pass is worth an overlay, and how far the real windows are at the handover. Decided over
//! ids and frames, with nothing of Core Animation in it. When each step of a flight happens is
//! `crate::transitions::domain::lifecycle`; when the pictures service makes the captures a flight asks
//! for is `crate::pictures::domain::schedule`. Design in `src/transitions/docs/coordination.md`.

use std::collections::HashMap;

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::ScreenRect;

use super::timing::{COMPANION_CENTER_SLACK, COMPANION_EXPANSION, HANDOVER_THRESHOLD_PT};

/// The unmanaged window tracing `frame` as its border, if any. A parked window never traces and is
/// never traced. See "Border companions" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn companion_of(
    frame: ScreenRect,
    candidates: &[(WindowServerId, ScreenRect)],
    display: ScreenRect,
) -> Option<(WindowServerId, ScreenRect)> {
    if rini_geometry::is_off_screen(display, frame) {
        return None;
    }
    let center =
        |r: ScreenRect| (r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0);
    let (cx, cy) = center(frame);
    candidates
        .iter()
        .filter(|(_, candidate)| !rini_geometry::is_off_screen(display, *candidate))
        .find(|(_, candidate)| {
            let dw = candidate.size.width - frame.size.width;
            let dh = candidate.size.height - frame.size.height;
            let (kx, ky) = center(*candidate);
            (-1.0..=COMPANION_EXPANSION).contains(&dw)
                && (-1.0..=COMPANION_EXPANSION).contains(&dh)
                && (kx - cx).abs() <= COMPANION_CENTER_SLACK
                && (ky - cy).abs() <= COMPANION_CENTER_SLACK
        })
        .copied()
}

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

/// Whether a composed pass is worth an overlay flight.
/// See "Layout changes" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn worth_flying(moving_drawable: bool, running: bool) -> bool {
    moving_drawable || running
}

/// The window server ids a border companion may never be: everything rini manages, the pass's windows
/// and every window the pictures service holds or owes a picture of. Synthetic (companion) ids are
/// left out, or a border seen once could never match again.
pub(in crate::transitions) fn managed_server_ids(
    pass: &std::collections::HashSet<u32>,
    pictured: impl Iterator<Item = WindowId>,
) -> std::collections::HashSet<u32> {
    let mut ids = pass.clone();
    ids.extend(crate::pictures::domain::identity::managed_server_ids(pictured));
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(idx: u32) -> WindowId {
        WindowId::new(7, idx)
    }

    mod companions {
        use super::*;
        use crate::animation::domain::motion::testing::{DISPLAY, rect};

        /// Border geometry from the user's bordersrc: a concentric sibling window a few points larger.
        #[test]
        fn a_border_window_tracing_a_window_is_its_companion() {
            let window = rect(4.0, 32.0, 859.0, 1081.0);
            let border = (WindowServerId::new(9001), rect(1.0, 29.0, 865.0, 1087.0));
            let neighbor = (WindowServerId::new(9002), rect(867.0, 32.0, 859.0, 1081.0));
            let zoom = (WindowServerId::new(9003), rect(224.0, 95.0, 1280.0, 960.0));
            let found = companion_of(window, &[neighbor, zoom, border], DISPLAY);
            assert_eq!(found.map(|(id, _)| id.as_u32()), Some(9001));
        }

        #[test]
        fn only_a_concentric_hug_counts_as_a_border() {
            let window = rect(4.0, 32.0, 859.0, 1081.0);
            let exact = (WindowServerId::new(1), rect(4.0, 32.0, 859.0, 1081.0));
            assert!(companion_of(window, &[exact], DISPLAY).is_some());
            let shifted = (WindowServerId::new(2), rect(24.0, 32.0, 859.0, 1081.0));
            let larger = (WindowServerId::new(3), rect(-16.0, 12.0, 899.0, 1121.0));
            let smaller = (WindowServerId::new(4), rect(6.0, 34.0, 855.0, 1077.0));
            assert!(companion_of(window, &[shifted, larger, smaller], DISPLAY).is_none());
        }

        /// Parked windows share a frame macOS clamps to 41pt visible, past `is_off_screen`; only the
        /// managed-id exclusion separates them. A synthetic companion id (pid 0) stays a candidate.
        #[test]
        fn a_managed_window_is_never_a_border_candidate() {
            let pass: std::collections::HashSet<u32> = [102698].into_iter().collect();
            let pictured = [WindowId::new(82799, 102682), WindowId::new(0, 9001), WindowId::new(872, 51462)];
            let managed = managed_server_ids(&pass, pictured.into_iter());
            assert!(managed.contains(&102698) && managed.contains(&102682) && managed.contains(&51462));
            assert!(
                !managed.contains(&9001),
                "a border seen before is still a border"
            );
            // The clamped park is not judged off screen, so geometry alone cannot exclude it.
            let park = rect(1727.0, 1076.0, 1720.0, 1081.0);
            assert!(!rini_geometry::is_off_screen(DISPLAY, park));
            assert!(companion_of(park, &[(WindowServerId::new(102682), park)], DISPLAY).is_some());
        }

        #[test]
        fn a_parked_window_neither_traces_nor_is_traced() {
            let park = rect(
                DISPLAY.size.width - 1.0,
                DISPLAY.size.height - 1.0,
                859.0,
                1081.0,
            );
            assert!(rini_geometry::is_off_screen(DISPLAY, park));
            let twin = (WindowServerId::new(7), park);
            assert!(companion_of(park, &[twin], DISPLAY).is_none(), "a parked anchor");
            let on_screen = rect(4.0, 32.0, 859.0, 1081.0);
            let border = (WindowServerId::new(8), rect(1.0, 29.0, 865.0, 1087.0));
            assert!(
                companion_of(on_screen, &[twin, border], DISPLAY).map(|(id, _)| id.as_u32()) == Some(8)
            );
        }
    }

    mod handover {
        use rini_geometry::space::{ScreenPoint, ScreenSize};

        use super::*;
        use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, rect};

        fn measured(
            frames: &[(WindowId, ScreenRect, ScreenRect)],
        ) -> (Vec<(WindowId, ScreenRect)>, Vec<WindowId>, HashMap<WindowId, ScreenRect>) {
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
        use crate::animation::domain::motion::plan::{GroupKey, ReflowPlan, RigidGroup, reflow_plan};
        use crate::animation::domain::motion::surface::surface_travel;
        use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, rect};
        use crate::transitions::domain::travel::is_moving;

        fn moving_drawable(tiles: &[(ScreenRect, ScreenRect)]) -> bool {
            tiles.iter().any(|(from, to)| is_moving(*from, *to))
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
        fn a_pass_where_nothing_drawable_moves_does_not_fly() {
            assert!(
                !worth_flying(false, false),
                "a still-only composition flew, hiding the new window until its picture landed"
            );
        }

        /// A window with no picture is an entrance, not a tile; nothing drawable moves.
        #[test]
        fn a_floating_open_over_a_still_strip_does_not_fly() {
            let s1 = rect(0.0, 32.0, 860.0, 1081.0);
            let s2 = rect(867.0, 32.0, 859.0, 1081.0);
            let tiles = vec![
                (s1, s1),
                (s2, s2),
            ];
            assert!(
                !worth_flying(moving_drawable(&tiles), false),
                "the entrance, a floating window with no picture, is reserved and never drawn here"
            );
        }

        #[test]
        fn a_strip_open_that_moves_a_neighbour_flies() {
            let before = rect(0.0, 32.0, 1720.0, 1081.0);
            let after = rect(0.0, 32.0, 860.0, 1081.0);
            let tiles = vec![(before, after)];
            assert!(worth_flying(moving_drawable(&tiles), false), "the opened window's own reservation aside");
        }

        #[test]
        fn a_pan_flies() {
            let frame = rect(867.0, 32.0, 859.0, 1081.0);
            let (from, to) =
                surface_travel(frame, SurfacePoint::origin(), SurfacePoint::new(867.0, 0.0), false);
            let tiles = vec![(from_overlay_space(from, DISPLAY), from_overlay_space(to, DISPLAY))];
            assert!(worth_flying(moving_drawable(&tiles), false));
        }

        #[test]
        fn a_still_pass_joining_a_running_flight_flies() {
            let s1 = rect(0.0, 32.0, 860.0, 1081.0);
            let tiles = vec![(s1, s1)];
            assert!(worth_flying(moving_drawable(&tiles), true));
            assert!(worth_flying(false, true), "even with nothing drawable at all");
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
                    let from = rng.on_screen();
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
                let flies = worth_flying(moving_drawable(&tiles), running);
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
            assert!(!worth_flying(false, false));
            assert!(worth_flying(true, false));
            assert!(worth_flying(false, true));
        }
    }
}
