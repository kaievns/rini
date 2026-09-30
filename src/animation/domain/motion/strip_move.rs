//! A window moved along the strip: where the two columns that change places start, which of them is
//! held on the strip, and the step that shows a move of a column filling the viewport. See "The
//! move flight" in `src/animation/docs/animation-smoothness.md`.

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::WindowId;
use rini_ipc::protocol::Direction;

use super::easing::{CubicBezier, EASE_IN_OUT, MOTION_CURVE, nudge_displacement};
use super::surface::SurfaceWindow;

/// Frames this close in x are one column.
const COLUMN_TOLERANCE: f64 = 0.5;

/// A column this close to the tiling area's width fills the viewport.
const FILLS_TOLERANCE: f64 = 1.0;

/// How far the strip steps away from where a moved window went, as a share of the viewport's width.
pub const NUDGE_SHARE: f64 = 1.0 / 3.0;

/// How much longer than a plain flight a flight carrying a nudge runs, so the nudge's peak speed
/// stays moderate.
pub const NUDGE_STRETCH: f64 = 1.3;

/// Where the moved window's column and the column it passed started, in the strip's coordinates.
///
/// `strip` is every tiled window at its new frame. Together the two columns fill the span they fill
/// now, in the order they had before, so the gap between them is the one they have now. Every window
/// of a column moves by the same amount. Empty for a vertical move, or one with no column to pass.
pub fn swap_starts(
    strip: &[(WindowId, CGRect)],
    moved: WindowId,
    direction: Direction,
) -> Vec<(WindowId, CGRect)> {
    let Some(&(_, at)) = strip.iter().find(|(window, _)| *window == moved) else {
        return Vec::new();
    };
    let moved_x = at.origin.x;
    let xs = strip.iter().map(|(_, frame)| frame.origin.x);
    let passed_x = match direction {
        Direction::Right => xs.filter(|x| *x < moved_x - COLUMN_TOLERANCE).reduce(f64::max),
        Direction::Left => xs.filter(|x| *x > moved_x + COLUMN_TOLERANCE).reduce(f64::min),
        Direction::Up | Direction::Down => None,
    };
    let Some(passed_x) = passed_x else {
        return Vec::new();
    };
    let in_column = |frame: &CGRect, x: f64| (frame.origin.x - x).abs() <= COLUMN_TOLERANCE;
    let Some(passed_width) = strip
        .iter()
        .find(|(_, frame)| in_column(frame, passed_x))
        .map(|(_, f)| f.size.width)
    else {
        return Vec::new();
    };
    let moved_width = at.size.width;
    let (moved_start, passed_start) = match direction {
        Direction::Right => (passed_x, moved_x + moved_width - passed_width),
        _ => (passed_x + passed_width - moved_width, moved_x),
    };
    strip
        .iter()
        .filter_map(|&(window, frame)| {
            let x = if in_column(&frame, moved_x) {
                moved_start
            } else if in_column(&frame, passed_x) {
                passed_start
            } else {
                return None;
            };
            Some((window, CGRect::new(CGPoint::new(x, frame.origin.y), frame.size)))
        })
        .collect()
}

/// Whether a column `width` wide fills the viewport: it is the tiling area's width, the display's
/// usable width less both outer gaps, within a point. The camera follows such a column, so nothing
/// on screen says it moved; a narrower one is seen moving.
pub fn fills_viewport(width: f64, screen_width: f64, outer_left: f64, outer_right: f64) -> bool {
    (width - (screen_width - outer_left - outer_right)).abs() <= FILLS_TOLERANCE
}

/// The step of a move: a share of the viewport away from where the moved window went, so the side
/// it opens is the side the column it passed comes from.
pub fn nudge(viewport_width: f64, direction: Direction) -> Option<CGPoint> {
    let step = viewport_width * NUDGE_SHARE;
    match direction {
        Direction::Right => Some(CGPoint::new(-step, 0.0)),
        Direction::Left => Some(CGPoint::new(step, 0.0)),
        Direction::Up | Direction::Down => None,
    }
}

/// Puts a move's pair on the strip surface, every window of which is at its new frame, and returns
/// the step the move takes. The moved column starts from its old slot. The column it passed starts
/// from its own and crosses, unless the moved column fills the viewport: then the whole strip
/// steps, and the passed column is drawn held at its old slot on the strip, riding the strip
/// beneath the moved window. Its real window still goes to its layout frame.
pub fn stage_move(
    windows: &mut [SurfaceWindow],
    moved: WindowId,
    direction: Direction,
    screen_width: f64,
    outer_left: f64,
    outer_right: f64,
) -> Option<CGPoint> {
    let strip: Vec<(WindowId, CGRect)> =
        windows.iter().filter(|w| !w.floating).map(|w| (w.window, w.frame)).collect();
    let at = strip.iter().find(|(window, _)| *window == moved)?.1;
    let starts = swap_starts(&strip, moved, direction);
    let steps =
        !starts.is_empty() && fills_viewport(at.size.width, screen_width, outer_left, outer_right);
    for (window, start) in starts {
        let Some(surface) = windows.iter_mut().find(|w| w.window == window) else {
            continue;
        };
        let passed = (surface.frame.origin.x - at.origin.x).abs() > COLUMN_TOLERANCE;
        if steps && passed {
            surface.frame = start;
        } else {
            surface.from = Some(start);
        }
    }
    steps.then(|| nudge(screen_width, direction)).flatten()
}

/// The curve a flight's containers fly on: the ease-in-out while the strip steps, so the column the
/// move passed lingers in the side the step opens; `MOTION_CURVE` otherwise.
pub fn flight_curve(stepping: bool) -> CubicBezier {
    if stepping { EASE_IN_OUT } else { MOTION_CURVE }
}

/// The nudge as evenly spaced keyframes about `step` seconds apart over `seconds`, from rest to rest.
pub fn nudge_samples(offset: CGPoint, seconds: f64, step: f64) -> Vec<CGPoint> {
    let steps = (seconds / step).ceil().max(1.0) as usize;
    (0..=steps)
        .map(|k| {
            let d = nudge_displacement(k as f64 / steps as f64);
            CGPoint::new(offset.x * d, offset.y * d)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;
    use crate::animation::domain::motion::easing::nudge_rate;

    const GAP: f64 = 3.0;
    const HEIGHT: f64 = 1081.0;

    fn wid(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    fn column(x: f64, width: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, 32.0), CGSize::new(width, HEIGHT))
    }

    fn start_of(starts: &[(WindowId, CGRect)], window: WindowId) -> Option<f64> {
        starts.iter().find(|(w, _)| *w == window).map(|(_, frame)| frame.origin.x)
    }

    /// A B W, just after W moved right past B: W started where B is now, B where W's right edge
    /// leaves room for it. A is where it was.
    #[test]
    fn a_move_right_starts_the_pair_in_each_others_slots() {
        let strip = [
            (wid(1), column(0.0, 861.0)),
            (wid(2), column(864.0, 861.0)),
            (wid(3), column(1728.0, 861.0)),
        ];
        let starts = swap_starts(&strip, wid(3), Direction::Right);
        assert_eq!(start_of(&starts, wid(3)), Some(864.0));
        assert_eq!(start_of(&starts, wid(2)), Some(1728.0));
        assert_eq!(
            start_of(&starts, wid(1)),
            None,
            "the rest of the strip is untouched"
        );
    }

    #[test]
    fn a_move_left_starts_the_pair_in_each_others_slots() {
        let strip = [
            (wid(1), column(0.0, 861.0)),
            (wid(2), column(864.0, 861.0)),
            (wid(3), column(1728.0, 861.0)),
        ];
        let starts = swap_starts(&strip, wid(1), Direction::Left);
        assert_eq!(start_of(&starts, wid(1)), Some(864.0));
        assert_eq!(start_of(&starts, wid(2)), Some(0.0));
        assert_eq!(start_of(&starts, wid(3)), None);
    }

    /// The formulas the move is specified by, W of width a and N of width b with gap g: moving right
    /// W started at N's new x and N at N's new x + a + g; moving left W started at W's new x + b + g
    /// and N at W's new x.
    #[test]
    fn unequal_widths_keep_the_gap_between_the_pair() {
        let (a, b) = (600.0, 1000.0);
        let right = [(wid(2), column(0.0, b)), (wid(1), column(b + GAP, a))];
        let starts = swap_starts(&right, wid(1), Direction::Right);
        assert_eq!(start_of(&starts, wid(1)), Some(0.0));
        assert_eq!(start_of(&starts, wid(2)), Some(a + GAP));

        let left = [(wid(1), column(0.0, a)), (wid(2), column(a + GAP, b))];
        let starts = swap_starts(&left, wid(1), Direction::Left);
        assert_eq!(start_of(&starts, wid(1)), Some(b + GAP));
        assert_eq!(start_of(&starts, wid(2)), Some(0.0));
    }

    /// Every window of the passed column moves by one amount and keeps its row.
    #[test]
    fn a_stacked_neighbour_moves_as_one_column() {
        let top = CGRect::new(CGPoint::new(0.0, 32.0), CGSize::new(861.0, 539.0));
        let bottom = CGRect::new(CGPoint::new(0.0, 574.0), CGSize::new(861.0, 539.0));
        let strip = [
            (wid(1), top),
            (wid(2), bottom),
            (wid(3), column(864.0, 861.0)),
            (wid(4), column(1728.0, 861.0)),
        ];
        let starts = swap_starts(&strip, wid(3), Direction::Right);
        assert_eq!(starts.len(), 3);
        for (window, row) in [(wid(1), top), (wid(2), bottom)] {
            let (_, start) = starts.iter().find(|(w, _)| *w == window).copied().expect("moved");
            assert_eq!(start.origin, CGPoint::new(864.0, row.origin.y));
            assert_eq!(start.size, row.size);
        }
        assert_eq!(start_of(&starts, wid(3)), Some(0.0));
        assert_eq!(start_of(&starts, wid(4)), None);
    }

    #[test]
    fn nothing_starts_elsewhere_for_a_vertical_move_or_with_no_column_to_pass() {
        let strip = [(wid(1), column(0.0, 861.0)), (wid(2), column(864.0, 861.0))];
        assert!(swap_starts(&strip, wid(1), Direction::Up).is_empty());
        assert!(
            swap_starts(&strip, wid(1), Direction::Right).is_empty(),
            "nothing left of it"
        );
        assert!(
            swap_starts(&strip, wid(9), Direction::Right).is_empty(),
            "not on the strip"
        );
    }

    /// The live display, 1728pt with 3pt outer gaps: only a column the tiling area's 1722pt wide
    /// fills it, whether the full-width toggle or a width preset made it so. Half, two-thirds and
    /// anything off by more than a point do not.
    #[test]
    fn only_a_column_as_wide_as_the_tiling_area_fills_the_viewport() {
        assert!(fills_viewport(1722.0, 1728.0, GAP, GAP));
        assert!(fills_viewport(1722.8, 1728.0, GAP, GAP), "within a point");
        for width in [860.0, 1148.0, 1151.0, 1720.5, 1728.0] {
            assert!(!fills_viewport(width, 1728.0, GAP, GAP), "{width}");
        }
        assert!(fills_viewport(1728.0, 1728.0, 0.0, 0.0), "no gaps");
        assert!(fills_viewport(1719.0, 1728.0, 4.0, 5.0), "uneven gaps");
    }

    /// The step goes away from where the moved window went: a move right steps the strip left and
    /// opens the right side, where the column it passed was.
    #[test]
    fn the_step_goes_away_from_where_the_window_went() {
        assert_eq!(nudge(1728.0, Direction::Right), Some(CGPoint::new(-576.0, 0.0)));
        assert_eq!(nudge(1728.0, Direction::Left), Some(CGPoint::new(576.0, 0.0)));
        assert_eq!(nudge(1728.0, Direction::Up), None);
        assert_eq!(nudge(1728.0, Direction::Down), None);
    }

    #[test]
    fn the_nudge_keyframes_run_rest_to_rest_through_the_offset() {
        let offset = CGPoint::new(576.0, 0.0);
        let samples = nudge_samples(offset, 0.455, 1.0 / 120.0);
        assert_eq!(samples.first(), Some(&CGPoint::new(0.0, 0.0)));
        assert!(samples.last().expect("some").x.abs() < 1e-9);
        let furthest = samples.iter().map(|p| p.x).fold(0.0, f64::max);
        assert!((furthest - 576.0).abs() < 1.0, "{furthest}");
        assert!(samples.iter().all(|p| p.y == 0.0 && p.x >= 0.0));
    }

    /// The numbers "The move flight" quotes: a third of the laptop's 1720pt and of the 3008pt
    /// external, over the stretched default flight, peak well under the 28,338pt/s already reported
    /// as a jerk and near the edge bounce's own launch.
    #[test]
    fn the_nudge_peaks_at_a_moderate_speed() {
        let seconds = 0.35 * NUDGE_STRETCH;
        let peak = |offset: f64| offset * nudge_rate(0.25) / seconds;
        assert!((peak(573.0) - 3956.0).abs() < 1.0, "{}", peak(573.0));
        assert!((peak(1003.0) - 6925.0).abs() < 1.0, "{}", peak(1003.0));
        for i in 0..=1000 {
            let t = i as f64 / 1000.0;
            assert!(1003.0 * nudge_rate(t).abs() / seconds <= peak(1003.0) + 1e-6);
        }
    }

    use crate::animation::domain::motion::glide::Leg;
    use crate::animation::domain::motion::plan::{
        AnimationTarget, FlightPlan, GroupKey, Member, animation_targets, carries_out_and_back,
        surface_plan,
    };
    use rini_core::ids::WindowServerId;

    /// The display the design was simulated on, 2026-09-30.
    const V: f64 = 1728.0;
    const PLAIN: f64 = 0.35;
    const W: u32 = 1;
    const N: u32 = 2;
    const C: u32 = 3;
    const P: u32 = 4;
    const F: u32 = 5;

    fn surface_window(window: WindowId, frame: CGRect, floating: bool) -> SurfaceWindow {
        SurfaceWindow {
            window,
            server_id: WindowServerId::new(window.idx.get()),
            frame,
            pinned: floating,
            floating,
            from: None,
        }
    }

    /// The simulated move, at its new layout: full-width W (1722pt) moved right past N (860pt), C
    /// (1148pt) after N and P before it, a pinned floating window F over the strip. The camera
    /// follows W, the view travelling -862 as measured. `Left` is its mirror image.
    fn full_width_move(direction: Direction) -> (Vec<SurfaceWindow>, CGPoint) {
        let right = [
            (wid(P), column(-1723.0, 860.0)),
            (wid(N), column(-860.0, 860.0)),
            (wid(W), column(3.0, 1722.0)),
            (wid(C), column(1728.0, 1148.0)),
        ];
        let mirror = |frame: CGRect| {
            CGRect::new(
                CGPoint::new(V - frame.origin.x - frame.size.width, frame.origin.y),
                frame.size,
            )
        };
        let (frames, travel): (Vec<(WindowId, CGRect)>, f64) = match direction {
            Direction::Right => (right.to_vec(), -862.0),
            _ => (right.iter().map(|&(w, f)| (w, mirror(f))).collect(), 862.0),
        };
        let mut windows: Vec<SurfaceWindow> =
            frames.into_iter().map(|(w, f)| surface_window(w, f, false)).collect();
        let settings = CGRect::new(CGPoint::new(500.0, 300.0), CGSize::new(700.0, 500.0));
        windows.push(surface_window(wid(F), settings, true));
        (windows, CGPoint::new(travel, 0.0))
    }

    /// A move's flight as the engine flies it: every container along its leg over the stretched
    /// duration on `curve`, the step riding the containers in `stepped`.
    struct Flight {
        plan: FlightPlan,
        legs: Vec<(GroupKey, Leg)>,
        step: CGPoint,
        seconds: f64,
        stepped: Vec<GroupKey>,
    }

    impl Flight {
        fn fly(
            windows: &[SurfaceWindow],
            from_offset: CGPoint,
            step: CGPoint,
            curve: CubicBezier,
            steps: impl Fn(&FlightPlan, GroupKey) -> bool,
        ) -> Self {
            let plan = FlightPlan::from(surface_plan(windows, from_offset, CGPoint::new(0.0, 0.0)));
            let seconds = PLAIN * NUDGE_STRETCH;
            let legs = animation_targets(&plan)
                .into_iter()
                .filter_map(|target| match target {
                    AnimationTarget::Container { key, from, to } => {
                        let leg = Leg::Curve {
                            from,
                            to,
                            begin: 0.0,
                            seconds,
                            curve,
                        };
                        Some((key, leg))
                    }
                    AnimationTarget::Tile { .. } => None,
                })
                .collect();
            let mut keys: Vec<GroupKey> =
                plan.groups.iter().filter(|g| !g.members.is_empty()).map(|g| g.key).collect();
            keys.push(GroupKey::Floating);
            let stepped = keys.into_iter().filter(|key| steps(&plan, *key)).collect();
            Flight {
                plan,
                legs,
                step,
                seconds,
                stepped,
            }
        }

        fn container_at(&self, key: GroupKey, t: f64) -> f64 {
            let along = self
                .legs
                .iter()
                .find(|(k, _)| *k == key)
                .map_or(self.plan.position_of(key).x, |(_, leg)| {
                    leg.position_at(t * self.seconds).x
                });
            let stepped = if self.stepped.contains(&key) {
                self.step.x * nudge_displacement(t)
            } else {
                0.0
            };
            along + stepped
        }

        /// Where `window` is drawn at progress `t`, from its left edge to its right.
        fn span(&self, window: u32, t: f64) -> (f64, f64) {
            let (key, rel) = match self.plan.member(wid(window)) {
                Some(Member::Rigid { key, rel }) => (key, rel),
                Some(Member::Floating { from, .. }) => (GroupKey::Floating, from),
                other => panic!("{window} is {other:?}"),
            };
            let x = rel.origin.x + self.container_at(key, t);
            (x, x + rel.size.width)
        }

        /// The side of the display the step has opened at progress `t`.
        fn opened(&self, t: f64) -> (f64, f64) {
            let d = self.step.x.abs() * nudge_displacement(t);
            if self.step.x < 0.0 {
                (V - d, V)
            } else {
                (0.0, d)
            }
        }

        /// How much of `side` the windows `by` cover between them, at progress `t`.
        fn covered(&self, side: (f64, f64), by: &[u32], t: f64) -> f64 {
            let mut spans: Vec<(f64, f64)> = by
                .iter()
                .map(|w| self.span(*w, t))
                .map(|(lo, hi)| (lo.max(side.0), hi.min(side.1)))
                .filter(|(lo, hi)| hi > lo)
                .collect();
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            let (mut total, mut reach) = (0.0, f64::MIN);
            for (lo, hi) in spans {
                let lo = lo.max(reach);
                if hi > lo {
                    total += hi - lo;
                    reach = hi;
                }
            }
            total
        }

        /// How much of the side the step opens at progress `t` the windows `by` leave uncovered.
        fn open(&self, by: &[u32], t: f64) -> f64 {
            let side = self.opened(t);
            side.1 - side.0 - self.covered(side, by, t)
        }
    }

    /// The move as the reactor stages it and the engine flies it: the passed column held on the
    /// strip, the strip on `flight_curve`, the step on every container the engine steps.
    fn staged(direction: Direction, stepping: bool) -> Flight {
        let (mut windows, from_offset) = full_width_move(direction);
        let step = stage_move(&mut windows, wid(W), direction, V, GAP, GAP).expect("it steps");
        Flight::fly(
            &windows,
            from_offset,
            step,
            flight_curve(stepping),
            move |_, key| carries_out_and_back(key, step),
        )
    }

    /// Only the column passed is held: its frame is its old slot and it has no start of its own, so
    /// it rides the strip's container. The moved column still starts from its old slot, and nothing
    /// else changes.
    #[test]
    fn a_full_width_move_holds_the_column_it_passes_on_the_strip() {
        for direction in [Direction::Right, Direction::Left] {
            let (mut windows, from_offset) = full_width_move(direction);
            let before = windows.clone();
            let step = stage_move(&mut windows, wid(W), direction, V, GAP, GAP);
            assert_eq!(step, nudge(V, direction), "{direction:?}");
            let strip: Vec<(WindowId, CGRect)> =
                before.iter().filter(|w| !w.floating).map(|w| (w.window, w.frame)).collect();
            let starts = swap_starts(&strip, wid(W), direction);
            let start = |window| starts.iter().find(|(w, _)| *w == window).map(|(_, f)| *f);
            let get = |list: &[SurfaceWindow], window: u32| {
                *list.iter().find(|s| s.window == wid(window)).unwrap()
            };
            assert_eq!(
                get(&windows, N).frame,
                start(wid(N)).unwrap(),
                "held at its old slot"
            );
            assert_eq!(get(&windows, N).from, None);
            assert_eq!(
                get(&windows, W).from,
                start(wid(W)),
                "W still starts from its old slot"
            );
            for other in [P, C, F] {
                assert_eq!(get(&windows, other), get(&before, other));
            }
            let plan = surface_plan(&windows, from_offset, CGPoint::new(0.0, 0.0));
            let key = |window| plan.group_of(wid(window)).unwrap().key;
            assert_eq!(key(N), key(C), "N rides the strip");
            assert_ne!(key(W), key(N));
        }
    }

    /// A narrower column is seen moving: both columns cross, per `swap_starts`, and nothing steps.
    #[test]
    fn a_narrower_move_crosses_both_columns_and_does_not_step() {
        for width in [860.0, 1148.0, 1151.0] {
            let strip = [(wid(N), column(3.0, 860.0)), (wid(W), column(866.0, width))];
            let mut windows: Vec<SurfaceWindow> =
                strip.iter().map(|&(w, f)| surface_window(w, f, false)).collect();
            assert_eq!(
                stage_move(&mut windows, wid(W), Direction::Right, V, GAP, GAP),
                None
            );
            let starts = swap_starts(&strip, wid(W), Direction::Right);
            assert_eq!(starts.len(), 2);
            for window in &windows {
                let (_, frame) = strip.iter().find(|(w, _)| *w == window.window).unwrap();
                assert_eq!(window.frame, *frame, "{width}: every window at its new frame");
                assert_eq!(
                    window.from,
                    starts.iter().find(|(w, _)| *w == window.window).map(|(_, f)| *f),
                    "{width}"
                );
            }
        }
    }

    /// The simulated flight, both ways: the side the step opens is covered by N, then by N and C
    /// with only the 3pt gap between them showing, never the desktop. N covers all of it until the
    /// step is nearly out, then goes beneath W, which covers it at the end. F stands where it is.
    #[test]
    fn the_column_passed_fills_the_side_the_step_opens() {
        for direction in [Direction::Right, Direction::Left] {
            let flight = staged(direction, true);
            assert!(!flight.stepped.contains(&GroupKey::Floating));
            for i in 1..=9 {
                let t = i as f64 / 10.0;
                let open = flight.open(&[N, C], t);
                assert!(open <= GAP + 1e-6, "{direction:?} t={t}: {open:.1}pt of desktop");
                if i <= 4 {
                    let open = flight.open(&[N], t);
                    assert!(open < 1e-6, "{direction:?} t={t}: N leaves {open:.1}pt");
                }
                assert_eq!(flight.span(F, t), flight.span(F, 0.0), "F stands still");
            }
            let (w, n) = (flight.span(W, 1.0), flight.span(N, 1.0));
            assert!(
                w.0 <= n.0 + 1.0 && n.1 <= w.1 + 1.0,
                "{direction:?}: W {w:?} over N {n:?}"
            );
            assert!((w.0 - 3.0).abs() <= 1.0 && (w.1 - 1725.0).abs() <= 1.0, "{w:?}");
        }
        let flight = staged(Direction::Right, true);
        assert_eq!(flight.span(N, 0.0), (1727.0, 2587.0), "N starts where it was");
        assert_eq!(flight.span(N, 1.0), (865.0, 1725.0), "and ends beneath W");
    }

    /// The table "The move flight" quotes: per tenth of the flight, the side the step opens, how
    /// much of it N and C cover, and the desktop left between them.
    #[test]
    fn the_side_the_step_opens_is_the_simulated_one() {
        let table = [
            (55.0, 55.0, 0.0, 0.0),
            (199.0, 199.0, 0.0, 0.0),
            (377.0, 377.0, 0.0, 0.0),
            (521.0, 521.0, 0.0, 0.0),
            (576.0, 428.0, 145.0, 3.0),
            (521.0, 283.0, 235.0, 3.0),
            (377.0, 159.0, 215.0, 3.0),
            (199.0, 67.0, 129.0, 3.0),
            (55.0, 14.0, 38.0, 3.0),
        ];
        let flight = staged(Direction::Right, true);
        for (i, (gap, n, c, desk)) in table.into_iter().enumerate() {
            let t = (i + 1) as f64 / 10.0;
            let side = flight.opened(t);
            let measured = [
                ("gap", side.1 - side.0, gap),
                ("N", flight.covered(side, &[N], t), n),
                ("C", flight.covered(side, &[C], t), c),
                ("desk", flight.open(&[N, C], t), desk),
            ];
            for (what, got, want) in measured {
                assert!((got - want).abs() <= 1.0, "t={t} {what}: {got:.1}, table {want}");
            }
        }
    }

    /// Neither half of the design is enough alone. On `MOTION_CURVE` the strip has carried N
    /// beneath W before the step is out; with the step on W's container alone, the side it opens
    /// shows the desktop.
    #[test]
    fn the_side_the_step_opens_needs_the_whole_strip_and_the_ease_in_out() {
        let n_short = |flight: &Flight| (1..=4).any(|i| flight.open(&[N], i as f64 / 10.0) > 1.0);
        assert!(!n_short(&staged(Direction::Right, true)));
        let on_motion = staged(Direction::Right, false);
        assert!(n_short(&on_motion), "on MOTION_CURVE");
        let by_n = on_motion.covered(on_motion.opened(0.3), &[N], 0.3);
        assert!((by_n - 103.0).abs() <= 1.0, "N covers {by_n:.1} of 377 at t=0.3");

        let (mut windows, from_offset) = full_width_move(Direction::Right);
        let step = stage_move(&mut windows, wid(W), Direction::Right, V, GAP, GAP).unwrap();
        let w_alone = |plan: &FlightPlan, key| match plan.member(wid(W)) {
            Some(Member::Rigid { key: moved, .. }) => moved == key,
            _ => false,
        };
        let alone = Flight::fly(&windows, from_offset, step, flight_curve(true), w_alone);
        let desktop = (1..=9).any(|i| alone.open(&[N, C], i as f64 / 10.0) > GAP + 1.0);
        assert!(desktop, "W stepping alone opens the desktop");
    }
}
