//! A press on the strip, staged: where the two columns a move swaps start (`swap_starts`), the
//! look's staging of it put on the strip surface (`stage_move`), and where a window that was away
//! from where the travel draws it starts (`start_where_standing`). Which column is held and whether
//! the strip steps are the look's (`crate::animation::domain::choreography::Look::stage`). See "The
//! move flight, staged" in `src/transitions/docs/coordination.md`.

use rini_core::ids::WindowId;
use rini_geometry::space::{OverlayVector, Point, SurfacePoint, SurfaceRect};
use rini_ipc::protocol::Direction;

use crate::animation::domain::choreography::{Area, Cue, Look, PairStart, Strip};
use crate::animation::domain::motion::accents::Accent;
use crate::transitions::domain::request::SurfaceWindow;

/// Frames this close in x are one column.
const COLUMN_TOLERANCE: f64 = 0.5;

/// How far from where the travel draws it at the start a window may stand and still ride the strip:
/// the layout rounds to whole points, as `travel::travels_visibly` allows.
const STANDING_TOLERANCE: f64 = 2.0;

/// Where the moved window's column and the column it passed started on the strip.
///
/// `strip` is every tiled window at its new frame. Together the two columns fill the span they fill
/// now, in the order they had before, so the gap between them is the one they have now. Every window
/// of a column moves by the same amount. Empty for a vertical move, or one with no column to pass.
pub fn swap_starts(
    strip: &[(WindowId, SurfaceRect)],
    moved: WindowId,
    direction: Direction,
) -> Vec<PairStart> {
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
    let in_column = |frame: &SurfaceRect, x: f64| (frame.origin.x - x).abs() <= COLUMN_TOLERANCE;
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
            let (x, passed) = if in_column(&frame, moved_x) {
                (moved_start, false)
            } else if in_column(&frame, passed_x) {
                (passed_start, true)
            } else {
                return None;
            };
            Some(PairStart {
                window,
                start: SurfaceRect::new(Point::new(x, frame.origin.y), frame.size),
                passed,
            })
        })
        .collect()
}

/// Starts each strip window that was away from where the travel from `from_offset` draws it at the
/// start, from where it was. A window on screen at its own height starts where it stands: the one a
/// left-edge drag resized already sits where the release's scroll puts it. Any other is parked,
/// since a scroll never moves a window up or down, however much of it macOS leaves on screen, and
/// starts where the strip had it at the last pass: the columns after a left-edge shrink keep their
/// place on screen as the ones before it scroll. `standing` is where each window was at the press,
/// `laid_out` where the strip had it, and `viewport` the display, all on the surface at rest. A
/// parked window the strip did not have rides the strip.
pub fn start_where_standing(
    windows: &mut [SurfaceWindow],
    standing: &[(WindowId, SurfaceRect)],
    laid_out: &[(WindowId, SurfaceRect)],
    from_offset: SurfacePoint,
    viewport: SurfaceRect,
) {
    let find = |frames: &[(WindowId, SurfaceRect)], window| {
        frames.iter().find(|(w, _)| *w == window).map(|&(_, at)| at)
    };
    for window in windows.iter_mut().filter(|w| !w.floating && w.from.is_none()) {
        let drawn = window.frame.origin - from_offset.to_vector();
        let on_screen = find(standing, window.window).filter(|at| {
            !rini_geometry::is_off_screen(viewport, *at)
                && (drawn.y - at.origin.y).abs() <= STANDING_TOLERANCE
        });
        let Some(at) = on_screen.or_else(|| find(laid_out, window.window)) else {
            continue;
        };
        if (drawn.x - at.origin.x).abs() <= STANDING_TOLERANCE
            && (drawn.y - at.origin.y).abs() <= STANDING_TOLERANCE
        {
            continue;
        }
        let start = at.origin + from_offset.to_vector();
        window.from = Some(SurfaceRect::new(start, window.frame.size));
    }
}

/// Puts `cue`'s press on the strip surface, every window of which is at its new frame, as `look`
/// stages it, and returns the accent the look asks to ride the movement. A window the look starts
/// elsewhere crosses from there; one it holds is drawn at that frame on the strip, riding the strip,
/// while its real window still goes to its layout frame. `area` is the screen, `travel` the
/// viewport's, and `seconds` how long the movement runs, plain.
pub fn stage_move(
    windows: &mut [SurfaceWindow],
    cue: &Cue,
    area: Area,
    travel: OverlayVector,
    seconds: f64,
    look: Look,
) -> Option<Accent> {
    let strip: Vec<(WindowId, SurfaceRect)> =
        windows.iter().filter(|w| !w.floating).map(|w| (w.window, w.frame)).collect();
    let pair = cue
        .moved()
        .map(|(window, direction)| swap_starts(&strip, window, direction))
        .unwrap_or_default();
    let staging = look.stage(cue, &Strip {
        windows: &strip,
        pair: &pair,
        area,
        travel,
        seconds,
    });
    for (window, start) in staging.from {
        if let Some(surface) = windows.iter_mut().find(|w| w.window == window) {
            surface.from = Some(start);
        }
    }
    for (window, frame) in staging.held {
        if let Some(surface) = windows.iter_mut().find(|w| w.window == window) {
            surface.frame = frame;
        }
    }
    staging.accent
}

#[cfg(test)]
pub(crate) mod tests {
    use rini_geometry::space::{OverlayRect, Size, SurfacePoint};

    use super::*;
    use crate::animation::domain::motion::surface::{SurfaceGeometry, pan_travel};
    use crate::animation::domain::motion::easing::{CubicBezier, nudge_displacement};
    use crate::animation::domain::motion::nudge::{NUDGE_STRETCH, flight_curve};

    const GAP: f64 = 3.0;
    const HEIGHT: f64 = 1081.0;

    fn wid(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    fn column(x: f64, width: f64) -> SurfaceRect {
        SurfaceRect::new(Point::new(x, 32.0), Size::new(width, HEIGHT))
    }

    fn start_of(starts: &[PairStart], window: WindowId) -> Option<f64> {
        starts.iter().find(|p| p.window == window).map(|p| p.start.origin.x)
    }

    /// A move of `moved` staged by Slide on the live display, 1728pt with 3pt outer gaps, the
    /// viewport travelling from `from_offset` home: the step it takes.
    fn slid(
        windows: &mut [SurfaceWindow],
        moved: WindowId,
        direction: Direction,
        from_offset: SurfacePoint,
    ) -> Option<OverlayVector> {
        let cue = Cue::Move { window: moved, direction };
        let travel = pan_travel(from_offset, SurfacePoint::origin());
        match stage_move(windows, &cue, area(), travel, PLAIN, Look::Slide) {
            Some(Accent::Step { offset, .. }) => Some(offset),
            Some(Accent::Push { .. }) => panic!("a move pushes nothing"),
            None => None,
        }
    }

    fn on_surface(window: WindowId, frame: SurfaceRect) -> SurfaceWindow {
        SurfaceWindow {
            window,
            server_id: rini_core::ids::WindowServerId::new(window.idx.get()),
            frame,
            pinned: false,
            floating: false,
            from: None,
        }
    }

    /// A B, B's left edge dragged 90pt left and let go: the release scrolls the strip 90pt, so A
    /// pans from where it stands, and B, already where the scroll puts it, starts there. Panned
    /// from its end shifted back by the scroll, B jumped right by the drag's travel and slid back.
    #[test]
    fn a_window_standing_where_the_scroll_puts_it_starts_there() {
        let viewport = column(0.0, 1728.0);
        let from_offset = SurfacePoint::new(-90.0, 0.0);
        let mut windows =
            [on_surface(wid(1), column(-87.0, 600.0)), on_surface(wid(2), column(516.0, 951.0))];
        let standing = [(wid(1), column(3.0, 600.0)), (wid(2), column(515.0, 952.0))];
        let laid_out = [(wid(1), column(3.0, 600.0)), (wid(2), column(606.0, 861.0))];
        start_where_standing(&mut windows, &standing, &laid_out, from_offset, viewport);
        assert_eq!(windows[0].from, None, "A rides the strip");
        assert_eq!(windows[1].from, Some(column(425.0, 951.0)), "where it stands, its end's size");
        let (start, _) = windows[1].travel(from_offset, SurfacePoint::origin());
        assert_eq!(start.origin.x, 515.0);
    }

    /// A column parked in a corner at the press, where the strip had it as far back as the scroll,
    /// rides the strip in, as on any pan, as does one parked that the strip did not have; a window
    /// within the layout's rounding of where the travel draws it is left riding the strip.
    #[test]
    fn a_parked_or_rounded_window_rides_the_strip() {
        let viewport = column(0.0, 1728.0);
        let from_offset = SurfacePoint::new(864.0, 0.0);
        let mut windows = [
            on_surface(wid(1), column(864.0, 861.0)),
            on_surface(wid(2), column(1728.0, 861.0)),
            on_surface(wid(3), column(2592.0, 861.0)),
        ];
        let parked = SurfaceRect::new(Point::new(1727.0, 1112.0), Size::new(861.0, HEIGHT));
        let standing = [(wid(1), column(1.5, 861.0)), (wid(2), parked), (wid(3), parked)];
        let laid_out = [(wid(1), column(0.0, 861.0)), (wid(2), column(864.0, 861.0))];
        start_where_standing(&mut windows, &standing, &laid_out, from_offset, viewport);
        assert_eq!(windows[0].from, None, "within rounding");
        assert_eq!(windows[1].from, None, "parked, where the scroll carries it from");
        assert_eq!(windows[2].from, None, "parked, and not on the strip before");
    }

    /// A B C, B flush with the right edge and shrunk 90pt by its left edge: the release scrolls the
    /// strip 90pt right, carrying A, while B stays where it was let go and C, parked beyond the
    /// right edge, keeps its place on the strip. Riding the strip, C started 90pt on screen, under
    /// B, and slid off.
    #[test]
    fn a_parked_column_the_scroll_does_not_carry_starts_where_the_strip_had_it() {
        let viewport = column(0.0, 1728.0);
        let from_offset = SurfacePoint::new(90.0, 0.0);
        let mut windows = [
            on_surface(wid(1), column(-410.0, 600.0)),
            on_surface(wid(2), column(690.0, 1035.0)),
            on_surface(wid(3), column(1728.0, 861.0)),
        ];
        let parked = SurfaceRect::new(Point::new(1727.0, 1112.0), Size::new(861.0, HEIGHT));
        let standing =
            [(wid(1), column(-500.0, 600.0)), (wid(2), column(690.0, 1035.0)), (wid(3), parked)];
        let laid_out = [
            (wid(1), column(-500.0, 600.0)),
            (wid(2), column(600.0, 1125.0)),
            (wid(3), column(1728.0, 861.0)),
        ];
        start_where_standing(&mut windows, &standing, &laid_out, from_offset, viewport);
        assert_eq!(windows[0].from, None, "A rides the strip");
        let start = |window: &SurfaceWindow| window.travel(from_offset, SurfacePoint::origin()).0;
        assert_eq!(start(&windows[1]).origin.x, 690.0, "B where it was let go");
        assert_eq!(windows[2].from, Some(column(1818.0, 861.0)));
        assert_eq!(start(&windows[2]).origin.x, 1728.0, "C off screen, where it ends");
    }

    /// macOS keeps more of some parked windows on screen than `is_off_screen` allows: a Chrome
    /// window parked at the bottom-right corner showed 52pt of its height. It is still parked, and
    /// rides the strip in rather than flying from the corner.
    #[test]
    fn a_window_parked_with_more_than_the_clamp_showing_rides_the_strip() {
        let viewport = column(0.0, 1728.0);
        let from_offset = SurfacePoint::new(864.0, 0.0);
        let mut windows = [on_surface(wid(1), column(864.0, 861.0))];
        let parked = SurfaceRect::new(Point::new(1727.0, 1065.0), Size::new(861.0, HEIGHT));
        assert!(!rini_geometry::is_off_screen(viewport, parked), "the case is the one the clamp misses");
        let laid_out = [(wid(1), column(0.0, 861.0))];
        start_where_standing(&mut windows, &[(wid(1), parked)], &laid_out, from_offset, viewport);
        assert_eq!(windows[0].from, None);
    }

    /// A floating window is pinned, not carried, and a window a move staged keeps its own start.
    #[test]
    fn floating_and_staged_windows_are_left_alone() {
        let viewport = column(0.0, 1728.0);
        let from_offset = SurfacePoint::new(-90.0, 0.0);
        let mut floating = on_surface(wid(1), column(400.0, 600.0));
        floating.floating = true;
        floating.pinned = true;
        let mut staged = on_surface(wid(2), column(516.0, 951.0));
        staged.from = Some(column(0.0, 951.0));
        let mut windows = [floating, staged];
        let standing = [(wid(1), column(400.0, 600.0)), (wid(2), column(515.0, 952.0))];
        start_where_standing(&mut windows, &standing, &standing, from_offset, viewport);
        assert_eq!(windows[0].from, None);
        assert_eq!(windows[1].from, Some(column(0.0, 951.0)));
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
        let passed: Vec<(WindowId, bool)> = starts.iter().map(|p| (p.window, p.passed)).collect();
        assert_eq!(
            passed,
            [(wid(2), true), (wid(3), false)],
            "B is the column passed"
        );
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
        let top = SurfaceRect::new(Point::new(0.0, 32.0), Size::new(861.0, 539.0));
        let bottom = SurfaceRect::new(Point::new(0.0, 574.0), Size::new(861.0, 539.0));
        let strip = [
            (wid(1), top),
            (wid(2), bottom),
            (wid(3), column(864.0, 861.0)),
            (wid(4), column(1728.0, 861.0)),
        ];
        let starts = swap_starts(&strip, wid(3), Direction::Right);
        assert_eq!(starts.len(), 3);
        for (window, row) in [(wid(1), top), (wid(2), bottom)] {
            let start = starts.iter().find(|p| p.window == window).expect("moved");
            assert!(start.passed);
            assert_eq!(start.start.origin, Point::new(864.0, row.origin.y));
            assert_eq!(start.start.size, row.size);
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

    /// The live display's screen.
    fn area() -> Area {
        Area {
            frame: OverlayRect::new(Point::new(0.0, 0.0), Size::new(V, 1117.0)),
            outer_left: GAP,
            outer_right: GAP,
        }
    }

    fn surface_window(window: WindowId, frame: SurfaceRect, floating: bool) -> SurfaceWindow {
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
    fn full_width_move(direction: Direction) -> (Vec<SurfaceWindow>, SurfacePoint) {
        let right = [
            (wid(P), column(-1723.0, 860.0)),
            (wid(N), column(-860.0, 860.0)),
            (wid(W), column(3.0, 1722.0)),
            (wid(C), column(1728.0, 1148.0)),
        ];
        let mirror = |frame: SurfaceRect| {
            SurfaceRect::new(
                Point::new(V - frame.origin.x - frame.size.width, frame.origin.y),
                frame.size,
            )
        };
        let (frames, travel): (Vec<(WindowId, SurfaceRect)>, f64) = match direction {
            Direction::Right => (right.to_vec(), -862.0),
            _ => (right.iter().map(|&(w, f)| (w, mirror(f))).collect(), 862.0),
        };
        let mut windows: Vec<SurfaceWindow> =
            frames.into_iter().map(|(w, f)| surface_window(w, f, false)).collect();
        let settings = SurfaceRect::new(Point::new(500.0, 300.0), Size::new(700.0, 500.0));
        windows.push(surface_window(wid(F), settings, true));
        (windows, SurfacePoint::new(travel, 0.0))
    }

    /// A move's flight as the engine flies it: every container along its leg over the stretched
    /// duration on `curve`, the step riding the containers in `stepped`.
    struct Flight {
        plan: FlightPlan,
        legs: Vec<(GroupKey, Leg)>,
        step: OverlayVector,
        seconds: f64,
        stepped: Vec<GroupKey>,
    }

    impl Flight {
        fn fly(
            windows: &[SurfaceWindow],
            from_offset: SurfacePoint,
            step: OverlayVector,
            curve: CubicBezier,
            steps: impl Fn(&FlightPlan, GroupKey) -> bool,
        ) -> Self {
            let plan = FlightPlan::from(surface_plan(windows, from_offset, SurfacePoint::origin()));
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
        let step = slid(&mut windows, wid(W), direction, from_offset).expect("it steps");
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
            let step = slid(&mut windows, wid(W), direction, from_offset);
            let away = if direction == Direction::Right {
                -V / 3.0
            } else {
                V / 3.0
            };
            assert_eq!(step, Some(OverlayVector::new(away, 0.0)), "{direction:?}");
            let strip: Vec<(WindowId, SurfaceRect)> =
                before.iter().filter(|w| !w.floating).map(|w| (w.window, w.frame)).collect();
            let starts = swap_starts(&strip, wid(W), direction);
            let start = |window| starts.iter().find(|p| p.window == window).map(|p| p.start);
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
            let plan = surface_plan(&windows, from_offset, SurfacePoint::origin());
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
                slid(&mut windows, wid(W), Direction::Right, SurfacePoint::origin()),
                None
            );
            let starts = swap_starts(&strip, wid(W), Direction::Right);
            assert_eq!(starts.len(), 2);
            for window in &windows {
                let (_, frame) = strip.iter().find(|(w, _)| *w == window.window).unwrap();
                assert_eq!(window.frame, *frame, "{width}: every window at its new frame");
                assert_eq!(
                    window.from,
                    starts.iter().find(|p| p.window == window.window).map(|p| p.start),
                    "{width}"
                );
            }
        }
    }

    /// What the look staged is what lands on the surface: a window it starts elsewhere crosses from
    /// there, one it holds is drawn at that frame with no start of its own, every other window stays at
    /// its new frame, and the accent is the look's. A plain press stages nothing.
    #[test]
    fn stage_move_applies_what_the_look_staged() {
        let area = area();
        let travel_from = |from_offset| pan_travel(from_offset, SurfacePoint::origin());
        let narrower = vec![
            surface_window(wid(N), column(3.0, 860.0), false),
            surface_window(wid(W), column(866.0, 1148.0), false),
        ];
        let cases = [
            (full_width_move(Direction::Right), Direction::Right),
            (full_width_move(Direction::Left), Direction::Left),
            (full_width_move(Direction::Right), Direction::Up),
            ((narrower, SurfacePoint::origin()), Direction::Right),
        ];
        for look in Look::ALL {
            let (mut held, mut crossed) = (0, 0);
            for ((before, from_offset), direction) in cases.clone() {
                let travel = travel_from(from_offset);
                let cue = Cue::Move { window: wid(W), direction };
                let strip: Vec<(WindowId, SurfaceRect)> =
                    before.iter().filter(|w| !w.floating).map(|w| (w.window, w.frame)).collect();
                let pair = swap_starts(&strip, wid(W), direction);
                let staging = look.stage(&cue, &Strip {
                    windows: &strip,
                    pair: &pair,
                    area,
                    travel,
                    seconds: PLAIN,
                });
                let mut windows = before.clone();
                let accent = stage_move(&mut windows, &cue, area, travel, PLAIN, look);
                assert_eq!(accent, staging.accent, "{look:?} {direction:?}");
                for (was, is) in before.iter().zip(&windows) {
                    let staged = |list: &[(WindowId, SurfaceRect)]| {
                        list.iter().find(|(w, _)| *w == was.window).map(|(_, f)| *f)
                    };
                    let expected = SurfaceWindow {
                        from: staged(&staging.from).or(was.from),
                        frame: staged(&staging.held).unwrap_or(was.frame),
                        ..*was
                    };
                    assert_eq!(*is, expected, "{look:?} {direction:?}");
                }
                held += staging.held.len();
                crossed += staging.from.len();
            }
            assert!(
                held > 0 && crossed > 0,
                "{look:?}: {held} held, {crossed} crossed"
            );

            let (before, from_offset) = full_width_move(Direction::Right);
            let mut windows = before.clone();
            let travel = travel_from(from_offset);
            assert_eq!(stage_move(&mut windows, &Cue::Plain, area, travel, PLAIN, look), None);
            assert_eq!(windows, before, "{look:?}: a plain press");
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
        let step = slid(&mut windows, wid(W), Direction::Right, from_offset).unwrap();
        let w_alone = |plan: &FlightPlan, key| match plan.member(wid(W)) {
            Some(Member::Rigid { key: moved, .. }) => moved == key,
            _ => false,
        };
        let alone = Flight::fly(&windows, from_offset, step, flight_curve(true), w_alone);
        let desktop = (1..=9).any(|i| alone.open(&[N, C], i as f64 / 10.0) > GAP + 1.0);
        assert!(desktop, "W stepping alone opens the desktop");
    }

    /// Moves as `start_strip_pan` stages them, and bursts of them merging mid-flight. The engine's
    /// tests stage with these too.
    pub(crate) mod staging {
        use std::collections::HashMap;

        use rini_core::ids::{WindowId, WindowServerId};
        use rini_geometry::space::{OverlayPoint, OverlayRect, OverlayVector, SurfacePoint, SurfaceRect};
        use rini_geometry::{SameAs, to_overlay_space};
        use rini_ipc::protocol::Direction;

        use crate::animation::domain::choreography::{Area, Cue, Look};
        use crate::animation::domain::motion::accents::Accent;
        use crate::animation::domain::motion::easing::MOTION_CURVE;
        use crate::animation::domain::motion::glide::Leg;
        use crate::animation::domain::motion::plan::*;
        use crate::animation::domain::motion::surface::{SurfaceGeometry, pan_travel, viewport};
        use crate::animation::domain::motion::testing::{DISPLAY, VIEWPORT, rect};
        use crate::transitions::domain::request::SurfaceWindow;
        use crate::transitions::domain::strip_move::stage_move;

        /// A full-width column and the gap after it.
        pub(crate) const WIDTH: f64 = 1720.0;

        pub(crate) const STEP: f64 = 1723.0;

        /// A half-width column, which is seen moving and never steps.
        pub(crate) const HALF: f64 = 858.0;

        /// The outer gaps either side of a full-width column.
        pub(crate) const OUTER: f64 = 4.0;

        /// Where the viewport starts for one full-width move the camera follows, ending at `REST`.
        pub(crate) const OFFSET: SurfacePoint = SurfacePoint::new(-STEP, 0.0);

        pub(crate) const REST: SurfacePoint = SurfacePoint::new(0.0, 0.0);

        pub(crate) fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
        }

        pub(crate) fn at(x: f64, width: f64) -> SurfaceRect {
            rect(x, 32.0, width, 1081.0)
        }

        /// The surface of one move as `start_strip_pan` builds it: every window at its new frame,
        /// the pair staged by `stage_move`, and the step the move takes.
        pub(crate) fn staged(
            frames: &[(WindowId, SurfaceRect)],
            moved: WindowId,
            direction: Direction,
        ) -> (Vec<SurfaceWindow>, Option<OverlayVector>) {
            let mut windows: Vec<SurfaceWindow> = frames
                .iter()
                .map(|&(window, frame)| SurfaceWindow {
                    window,
                    server_id: WindowServerId::new(window.idx.get()),
                    frame,
                    pinned: false,
                    floating: false,
                    from: None,
                })
                .collect();
            let cue = Cue::Move { window: moved, direction };
            let area = Area {
                frame: VIEWPORT,
                outer_left: OUTER,
                outer_right: OUTER,
            };
            let travel = pan_travel(OFFSET, REST);
            let step = match stage_move(&mut windows, &cue, area, travel, super::PLAIN, Look::Slide) {
                Some(Accent::Step { offset, .. }) => Some(offset),
                _ => None,
            };
            (windows, step)
        }

        pub(crate) fn surface(
            frames: &[(WindowId, SurfaceRect)],
            moved: WindowId,
            direction: Direction,
        ) -> Vec<SurfaceWindow> {
            staged(frames, moved, direction).0
        }

        pub(crate) fn dest(plan: &FlightPlan, window: WindowId) -> Option<OverlayRect> {
            match plan.member(window)? {
                Member::Rigid { key, rel } => Some(overlay_of(rel, plan.position_of(key))),
                _ => None,
            }
        }

        pub(crate) fn key_of(plan: &FlightPlan, window: WindowId) -> Option<GroupKey> {
            match plan.member(window)? {
                Member::Rigid { key, .. } => Some(key),
                _ => None,
            }
        }

        /// Columns 0 A W B C (`wid(1)` to `wid(5)`), `width` wide, W moved right twice in a row
        /// with the camera following it: the flight of the first press, the frames and plan of the
        /// second, and the viewport's travel of each.
        pub(crate) fn two_presses(
            width: f64,
        ) -> (FlightPlan, [(WindowId, SurfaceRect); 5], ReflowPlan, SurfacePoint) {
            let step = width + 3.0;
            let offset = SurfacePoint::new(-step, 0.0);
            let (zero, a, w, b, c) = (wid(1), wid(2), wid(3), wid(4), wid(5));
            let first = [
                (zero, at(4.0 - 2.0 * step, width)),
                (a, at(4.0 - step, width)),
                (w, at(4.0, width)),
                (b, at(4.0 + step, width)),
                (c, at(4.0 + 2.0 * step, width)),
            ];
            let current =
                FlightPlan::from(surface_plan(&surface(&first, w, Direction::Right), offset, REST));
            let second = [
                (zero, at(4.0 - 3.0 * step, width)),
                (a, at(4.0 - 2.0 * step, width)),
                (b, at(4.0 - step, width)),
                (w, at(4.0, width)),
                (c, at(4.0 + step, width)),
            ];
            let incoming = surface_plan(&surface(&second, w, Direction::Right), offset, REST);
            (current, second, incoming, offset)
        }

        /// Every container of `plan` half-way along its travel.
        pub(crate) fn halfway(plan: &FlightPlan) -> HashMap<GroupKey, OverlayPoint> {
            plan.groups
                .iter()
                .map(|g| (g.key, OverlayPoint::new(g.travel.x / 2.0, g.travel.y / 2.0)))
                .collect()
        }

        /// The frame `window` is drawn at, its container at `presented`.
        pub(crate) fn drawn(
            plan: &FlightPlan,
            presented: &HashMap<GroupKey, OverlayPoint>,
            window: WindowId,
        ) -> OverlayRect {
            match plan.member(window) {
                Some(Member::Rigid { key, rel }) => overlay_of(rel, presented[&key]),
                other => panic!("{window:?} is {other:?}"),
            }
        }

        /// Half-width columns, W at the right edge moving right past B, the strip scrolling one column
        /// with it. B is two windows stacked. W stands still on screen, B crosses it, the rest pans.
        #[test]
        fn the_moved_column_and_the_one_it_passed_are_pieces_of_their_own() {
            let top = rect(4.0, 32.0, 860.0, 539.0);
            let bottom = rect(4.0, 574.0, 860.0, 539.0);
            let frames = [
                (wid(1), at(4.0 - 864.0, 860.0)),
                (wid(2), top),
                (wid(3), bottom),
                (wid(4), at(868.0, 860.0)),
                (wid(5), at(1732.0, 860.0)),
            ];
            let (windows, step) = staged(&frames, wid(4), Direction::Right);
            assert_eq!(step, None, "a half-width move does not step");
            let (from_offset, to_offset) = (SurfacePoint::new(-864.0, 0.0), SurfacePoint::origin());
            let plan = surface_plan(&windows, from_offset, to_offset);
            let group = |window| plan.group_of(window).expect("rigid").clone();

            assert_eq!(group(wid(4)).key, GroupKey::STILL, "the camera followed it");
            assert_eq!(group(wid(4)).members.len(), 1);
            let passed = group(wid(2));
            assert_eq!(passed.key, group(wid(3)).key, "a stacked column is one piece");
            assert_eq!(passed.travel, OverlayVector::new(-1728.0, 0.0));
            let rest = group(wid(1));
            assert_eq!(rest.key, group(wid(5)).key, "the rest of the strip is one piece");
            assert_eq!(rest.travel, pan_travel(from_offset, to_offset));
            assert_eq!(plan.groups.iter().filter(|g| !g.members.is_empty()).count(), 3);
            for window in &windows {
                let (from, to) = window.travel(from_offset, to_offset);
                let member = group(window.window);
                let rel = member.members.iter().find(|m| m.window == window.window).unwrap().rel;
                assert_eq!(rel, installed(from), "installs where it was on screen");
                assert_eq!(overlay_of(rel, member.travel.to_point()), to, "lands at its new frame");
            }
        }

        /// Full-width columns: the second press passes B while the first press's step plays. B is
        /// held on the strip as A was, so nothing leaves the strip's piece and no container opens:
        /// the step riding every container carries on over all of them. W keeps its container, B
        /// lands beneath W where it was, A pans on from beneath W with the strip, and the rest
        /// lands at its frames.
        #[test]
        fn a_second_full_width_move_mid_flight_holds_its_column_on_the_strip() {
            let (zero, a, w, b, c) = (wid(1), wid(2), wid(3), wid(4), wid(5));
            let (current, second, incoming, offset) = two_presses(WIDTH);
            let strip = key_of(&current, zero).unwrap();
            assert_eq!(key_of(&current, a), Some(strip), "the first press held A");
            let (merged, delta) = merge_plans(
                &current,
                &incoming,
                Some(pan_travel(offset, REST)),
                &halfway(&current).into(),
                Some(w),
                VIEWPORT,
            );

            assert!(delta.new_groups.is_empty(), "{:?}", delta.new_groups);
            assert!(delta.reparented.is_empty(), "{:?}", delta.reparented);
            let w_key = key_of(&current, w).unwrap();
            assert_eq!(key_of(&merged, w), Some(w_key), "W keeps its container");
            for window in [zero, a, b, c] {
                assert_eq!(
                    key_of(&merged, window),
                    Some(strip),
                    "{window:?} rides the strip"
                );
            }
            let frame_of = |window| {
                let frame = second.iter().find(|(w, _)| *w == window).unwrap().1;
                viewport(REST).transform_rect(&frame)
            };
            for window in [zero, w, c] {
                assert!(
                    dest(&merged, window).unwrap().same_as(frame_of(window)),
                    "{window:?}"
                );
            }
            let under_w = frame_of(w);
            let lands = dest(&merged, b).unwrap();
            assert!(
                lands.origin.x >= under_w.origin.x && lands.max().x <= under_w.max().x,
                "B lands beneath W: {lands:?}"
            );
            let pan = pan_travel(offset, REST);
            let a_lands = dest(&merged, a).unwrap();
            assert!(
                a_lands.same_as(under_w.translate(pan)),
                "A was held beneath W and pans on with the strip: {a_lands:?}"
            );
        }

        /// Half-width columns: the column the second press passes leaves the strip's container for
        /// one of its own, from where it is drawn and exactly as fast as the strip was going. A
        /// curve from rest there launched it at many times the speed it had a frame before.
        #[test]
        fn the_column_a_second_move_passes_keeps_its_speed() {
            let (w, b) = (wid(3), wid(4));
            let (current, _, incoming, offset) = two_presses(HALF);
            let seconds = 0.35;
            let legs: HashMap<GroupKey, Leg> = current
                .groups
                .iter()
                .filter(|g| !g.is_still())
                .map(|g| {
                    let leg = Leg::Curve {
                        from: OverlayPoint::origin(),
                        to: g.travel.to_point(),
                        begin: 0.0,
                        seconds,
                        curve: MOTION_CURVE,
                    };
                    (g.key, leg)
                })
                .collect();
            let now = 0.12;
            let along: HashMap<GroupKey, OverlayPoint> = current
                .groups
                .iter()
                .map(|g| {
                    (
                        g.key,
                        legs.get(&g.key).map_or(g.travel.to_point(), |leg| leg.position_at(now)),
                    )
                })
                .collect();
            let (merged, delta) = merge_plans(
                &current,
                &incoming,
                Some(pan_travel(offset, REST)),
                &along.clone().into(),
                Some(w),
                VIEWPORT,
            );
            let strip = key_of(&current, b).unwrap();
            let key = key_of(&merged, b).unwrap();
            let opened =
                delta.new_groups.iter().find(|g| g.key == key).expect("a container of its own");
            assert_eq!(opened.leaving, Some(strip));

            let omega = crate::animation::domain::motion::glide::spring_omega(seconds);
            let to = merged.position_of(key);
            let leg = Leg::toward(legs.get(&strip), opened.install, to, now, omega);
            let before = drawn(&current, &along, b);
            let after = drawn(&merged, &[(key, leg.position_at(now))].into_iter().collect(), b);
            assert!(after.same_as(before), "no jump: {before:?} then {after:?}");
            let speed = legs[&strip].velocity_at(now).x;
            assert!(speed.abs() > 500.0, "the strip was moving: {speed}");
            assert!((leg.velocity_at(now).x - speed).abs() < 1e-6, "no kick");
            assert!(
                (leg.position_at(now + leg.seconds()).x - to.x).abs() <= 0.5,
                "and it lands"
            );

            let from_rest = Leg::Curve {
                from: opened.install,
                to,
                begin: now,
                seconds,
                curve: MOTION_CURVE,
            };
            assert!(
                from_rest.velocity_at(now).x / speed > 5.0,
                "what it replaced kicked"
            );
        }

        /// The whole strip steps, so a still window joining mid-step takes the still piece and
        /// steps with the strip, as it would had it been there when the step began.
        #[test]
        fn a_still_window_joining_mid_step_steps_with_the_strip() {
            let w = wid(3);
            let (current, _, _, _) = two_presses(WIDTH);
            let key = key_of(&current, w).unwrap();
            let stands = rect(900.0, 200.0, 400.0, 300.0);
            let pass = reflow_plan(&[(wid(40), stands, stands, false)], DISPLAY);
            let presented = Presented::from(halfway(&current));

            let (merged, _) = merge_plans(&current, &pass, None, &presented, None, VIEWPORT);
            assert_eq!(key_of(&merged, wid(40)), Some(key), "it shares the still piece");
            assert!(carries_out_and_back(key, OverlayVector::new(-576.0, 0.0)));
            assert!(dest(&merged, wid(40)).unwrap().same_as(to_overlay_space(stands, DISPLAY)));
        }
    }
}
