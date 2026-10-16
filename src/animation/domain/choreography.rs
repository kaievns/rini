//! Looks: how a flight moves. A look stages a press on the strip before the flight is asked for
//! (`Look::stage`), and for each flight its `Choreography` makes the plan, poses frame zero, composes
//! the motion, folds a press before it and bends the flight at a press after it, answering with the
//! score's edits (`motion::score::Edit`), which the renderer plays as they are. `Look::ALL` is every
//! look, and the tests below are the contract each keeps; `slide` is the default. See "The overlay
//! engine" in `src/animation/docs/animation-smoothness.md`.

pub mod slide;

use std::fmt::Debug;

use rini_core::ids::WindowId;
use rini_geometry::space::{OverlayRect, OverlayVector, SurfaceRect};
use rini_ipc::protocol::Direction;

use self::slide::Slide;
use super::motion::accents::Accent;
use super::motion::plan::{FlightPlan, PlanDelta, ReflowPlan};
use super::motion::score::{Edit, Score};

/// How a flight moves, chosen when it begins: a later pass's look is not asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    #[default]
    Slide,
}

impl Look {
    pub const ALL: [Look; 1] = [Look::Slide];

    /// The look's choreography for one flight.
    pub fn choreography(self) -> Box<dyn Choreography> {
        match self {
            Look::Slide => Box::new(Slide::default()),
        }
    }

    /// What the look does with a press on `strip`: which windows start elsewhere, which are held,
    /// and the accent riding the movement.
    pub fn stage(self, cue: &Cue, strip: &Strip) -> Staging {
        match self {
            Look::Slide => slide::stage(cue, strip),
        }
    }
}

/// The press a flight shows, as the reactor names it; passed through to the look untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    Plain,
    /// `window` moved along the strip, `direction` the way it went.
    Move {
        window: WindowId,
        direction: Direction,
    },
}

impl Cue {
    pub fn moved(&self) -> Option<(WindowId, Direction)> {
        match *self {
            Cue::Move { window, direction } => Some((window, direction)),
            Cue::Plain => None,
        }
    }
}

/// Where one window of a moved pair started on the strip, and whether it is of the column passed
/// rather than the moved one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairStart {
    pub window: WindowId,
    pub start: SurfaceRect,
    pub passed: bool,
}

/// The screen a strip is shown on, and its outer gaps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub frame: OverlayRect,
    pub outer_left: f64,
    pub outer_right: f64,
}

/// A strip movement as a look stages it: every tiled window at its new frame, the pair a move
/// swapped, the screen, the viewport's travel, which is where the camera starts, and how long the
/// movement runs, plain.
#[derive(Clone, Copy, Debug)]
pub struct Strip<'a> {
    pub windows: &'a [(WindowId, SurfaceRect)],
    pub pair: &'a [PairStart],
    pub area: Area,
    pub travel: OverlayVector,
    pub seconds: f64,
}

/// What a look staged: windows that start elsewhere and cross from there, windows drawn held at a
/// frame on the strip while their real windows go to their layout frames, and the accent riding the
/// movement.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Staging {
    pub from: Vec<(WindowId, SurfaceRect)>,
    pub held: Vec<(WindowId, SurfaceRect)>,
    pub accent: Option<Accent>,
}

/// A flight as a look composes it, at frame zero or as its motion starts.
#[derive(Clone, Copy, Debug)]
pub struct Stage<'a> {
    pub plan: &'a FlightPlan,
    /// The windows the flight draws a tile of.
    pub drawn: &'a [WindowId],
    /// How long the motion runs, plain: a look stretches it for an accent that asks it to.
    pub seconds: f64,
    /// The press frame zero shows: the latest folded into it.
    pub cue: Cue,
    /// What waited for the motion to start: at most one of each kind.
    pub accents: &'a [Accent],
}

/// A later pass, folded into a flight before it moves or bending one that moves.
#[derive(Clone, Copy, Debug)]
pub struct Pass<'a> {
    pub plan: &'a ReflowPlan,
    /// The strip's travel, for a strip movement.
    pub pan: Option<OverlayVector>,
    /// The window gaining focus, which wins a tied vote.
    pub focus: Option<WindowId>,
    /// The display the overlay covers: what a member's destination is judged against.
    pub viewport: OverlayRect,
    /// The windows the flight draws a tile of once the pass is in.
    pub drawn: &'a [WindowId],
    /// How long what the pass moves runs, plain.
    pub seconds: f64,
    pub cue: Cue,
    pub accent: Option<Accent>,
}

/// A pass folded into a moving flight: the merged plan, what the merge changed, and the edits that
/// carry it out on the score.
#[derive(Clone, Debug)]
pub struct Retarget {
    pub plan: FlightPlan,
    pub delta: PlanDelta,
    pub edits: Vec<Edit>,
}

/// One flight's look at work. It answers from what it is given and the state it keeps for the
/// flight, and never reads the renderer.
pub trait Choreography: Debug {
    /// The flight's plan, from its first pass.
    fn plan(&self, pass: ReflowPlan, cue: &Cue) -> FlightPlan;

    /// A pass folded into the flight before it moves; frame zero is posed again from what it gives.
    fn fold(&self, plan: &FlightPlan, pass: &Pass) -> FlightPlan;

    /// Frame zero, nothing playing.
    fn pose(&self, stage: &Stage) -> Score;

    /// The motion from frame zero, from `begin`, with the accents that waited for it.
    fn compose(&mut self, playing: &Score, stage: &Stage, begin: f64) -> Vec<Edit>;

    /// A pass bending the flight that moves, at `now`, from where `playing` draws it.
    fn retarget(&mut self, playing: &Score, plan: &FlightPlan, pass: &Pass, now: f64) -> Retarget;

    /// A push against `wall` reaching the flight that moves, at `now`.
    fn push(
        &mut self,
        playing: &Score,
        cue: &Cue,
        wall: OverlayVector,
        seconds: f64,
        now: f64,
    ) -> Vec<Edit>;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rini_geometry::space::{OverlayPoint, Point, Rect, Respace, ScreenRect, ScreenVector};
    use rini_geometry::{from_overlay_space, is_off_screen, to_overlay_space};

    use super::*;
    use crate::animation::domain::motion::bounce::Bounce;
    use crate::animation::domain::motion::plan::{
        EDGE_BOUNCE_OVERSHOOT, GROUP_TOLERANCE, GroupKey, Member, Resolved, carries_out_and_back,
        on_overlay, overlay_of, reflow_plan,
    };
    use crate::animation::domain::motion::score::{Node, Value};
    use crate::animation::domain::motion::testing::{
        DISPLAY, Gen, RUNS, VIEWPORT, random_requests, rect,
    };

    const PLAIN: f64 = 0.35;
    const BEGIN: f64 = 10.0;

    fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

    fn shifted<U>(frame: Rect<U>, dx: f64) -> Rect<U> {
        rect(
            frame.origin.x + dx,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        )
    }

    /// A frame on `DISPLAY`, which sits at the origin, on the overlay over it.
    fn on(frame: ScreenRect) -> OverlayRect { to_overlay_space(frame, DISPLAY) }

    /// An overlay frame back on `DISPLAY`.
    fn screen(frame: OverlayRect) -> ScreenRect { from_overlay_space(frame, DISPLAY) }

    /// The largest of the four differences between two frames.
    fn gap<U>(a: Rect<U>, b: Rect<U>) -> f64 {
        [
            a.origin.x - b.origin.x,
            a.origin.y - b.origin.y,
            a.size.width - b.size.width,
            a.size.height - b.size.height,
        ]
        .into_iter()
        .fold(0.0f64, |worst, d| worst.max(d.abs()))
    }

    /// The farthest apart two frames' edges are.
    fn edges_apart<U>(a: Rect<U>, b: Rect<U>) -> f64 {
        [
            a.min().x - b.min().x,
            a.min().y - b.min().y,
            a.max().x - b.max().x,
            a.max().y - b.max().y,
        ]
        .into_iter()
        .fold(0.0f64, |worst, d| worst.max(d.abs()))
    }

    /// One flight of a look, every edit it answers folded into its score.
    struct Flight {
        choreography: Box<dyn Choreography>,
        plan: FlightPlan,
        drawn: Vec<WindowId>,
        score: Score,
    }

    impl Flight {
        /// `requests` posed at frame zero by `choreography`, the windows `drawn` keeps drawn.
        fn posed(
            choreography: Box<dyn Choreography>,
            requests: &[Resolved],
            drawn: impl Fn(WindowId) -> bool,
        ) -> Flight {
            let plan = choreography.plan(reflow_plan(requests, DISPLAY), &Cue::Plain);
            let drawn: Vec<WindowId> = plan.windows().into_iter().filter(|w| drawn(*w)).collect();
            let score = choreography.pose(&stage(&plan, &drawn, &[]));
            Flight {
                choreography,
                plan,
                drawn,
                score,
            }
        }

        fn fold(&mut self, edits: &[Edit]) {
            for edit in edits {
                self.score.apply(edit);
            }
        }

        /// The motion from frame zero at `BEGIN`, with `accents` waiting for it.
        fn start(&mut self, accents: &[Accent]) {
            let stage = stage(&self.plan, &self.drawn, accents);
            let edits = self.choreography.compose(&self.score, &stage, BEGIN);
            self.fold(&edits);
        }

        /// `incoming` merged at `now`.
        fn merge(&mut self, incoming: &ReflowPlan, pan: Option<OverlayVector>, now: f64) {
            let pass = Pass {
                plan: incoming,
                pan,
                focus: None,
                viewport: VIEWPORT,
                drawn: &self.drawn,
                seconds: PLAIN,
                cue: Cue::Plain,
                accent: None,
            };
            let retarget = self.choreography.retarget(&self.score, &self.plan, &pass, now);
            self.fold(&retarget.edits);
            self.plan = retarget.plan;
        }
    }

    fn stage<'a>(plan: &'a FlightPlan, drawn: &'a [WindowId], accents: &'a [Accent]) -> Stage<'a> {
        Stage {
            plan,
            drawn,
            seconds: PLAIN,
            cue: Cue::Plain,
            accents,
        }
    }

    /// Frame zero draws every window the flight draws where it starts, each edge within the half point
    /// placing on whole points allows, plays nothing, and draws nothing of a window the flight does
    /// not draw.
    #[test]
    fn every_look_poses_each_drawn_window_where_it_starts() {
        for look in Look::ALL {
            let mut rng = Gen(29);
            for run in 0..RUNS {
                let requests = random_requests(&mut rng);
                let left_out: Vec<WindowId> =
                    requests.iter().map(|r| r.window).filter(|_| rng.below(4) == 0).collect();
                let flight =
                    Flight::posed(look.choreography(), &requests, |w| !left_out.contains(&w));
                assert!(
                    flight.score.settled(BEGIN),
                    "{look:?} run {run}: frame zero plays"
                );
                for &Resolved { window, start: from, .. } in &requests {
                    let drawn = flight.score.drawn(window, BEGIN);
                    if left_out.contains(&window) {
                        assert_eq!(drawn, None, "{look:?} run {run}: {window:?} is not drawn");
                        continue;
                    }
                    let drawn = drawn.unwrap_or_else(|| panic!("{look:?} run {run}: {window:?}"));
                    assert!(
                        edges_apart(drawn, on(from)) <= 0.5,
                        "{look:?} run {run}: {window:?} at {drawn:?}, starts at {from:?}"
                    );
                }
            }
        }
    }

    /// The motion lands every window the flight draws where its plan sends it, whichever accent
    /// waited for it, and once every track has ended nothing plays: each edge within a point, a half
    /// for its tile and a half for its container, each placed on whole points.
    #[test]
    fn every_look_lands_each_node_on_its_plan() {
        let waiting = [
            Vec::new(),
            vec![Accent::Step {
                offset: OverlayVector::new(-576.0, 0.0),
                seconds: PLAIN,
            }],
            vec![Accent::Push {
                wall: OverlayVector::new(72.0, 0.0),
                seconds: PLAIN,
            }],
        ];
        for look in Look::ALL {
            let mut rng = Gen(31);
            for run in 0..RUNS {
                let mut flight =
                    Flight::posed(look.choreography(), &random_requests(&mut rng), |_| true);
                let accents = &waiting[rng.below(waiting.len() as u64) as usize];
                flight.start(accents);
                let end = BEGIN + flight.score.remaining(BEGIN);
                assert!(flight.score.settled(end), "{look:?} run {run}");
                for &window in &flight.drawn {
                    let drawn = flight.score.drawn(window, end).expect("drawn");
                    let lands = flight.plan.destination(window).expect("in the plan");
                    assert!(
                        edges_apart(drawn, lands) <= 1.0,
                        "{look:?} run {run} {accents:?}: {window:?} at {drawn:?}, lands at {lands:?}"
                    );
                }
            }
        }
    }

    /// Whatever a pass brings and whenever it lands, every window the flight drew before it is drawn
    /// where it was, and a window it brings rides beside the windows it sits beside in the layout:
    /// drawn where the pass has it, moved by as far as the container it rides is drawn from where the
    /// last pass sent it. Each within the half point placing on whole points allows: passes built as
    /// the reactor builds them, from where the last pass sends every window, merged into a flight
    /// playing from its score, 200 cases from a fixed seed.
    #[test]
    fn every_look_bends_a_flight_without_a_jump() {
        for look in Look::ALL {
            let mut rng = Gen(163);
            let mut brought = 0;
            for run in 0..RUNS {
                let mut flight =
                    Flight::posed(look.choreography(), &random_requests(&mut rng), |_| true);
                flight.start(&[]);
                let mut now = BEGIN;
                for step in 0..1 + rng.below(4) {
                    let tag = format!("{look:?} seed 163 run {run} step {step}");
                    now += rng.pt(10.0, 400.0) / 1000.0;
                    let windows = flight.plan.windows();
                    let newcomer = wid(500 + step as u32);
                    let (incoming, pan) = random_pass(&mut rng, &mut flight, newcomer);
                    let before: Vec<(WindowId, OverlayRect)> = windows
                        .iter()
                        .filter_map(|w| Some((*w, flight.score.drawn(*w, now)?)))
                        .collect();
                    let sent = flight.plan.clone();
                    flight.merge(&incoming, pan, now);
                    if let Some(Member::Rigid { key, .. }) = flight.plan.member(newcomer)
                        && let Some(start) = sent.positions.get(&key)
                    {
                        let from = incoming.groups.iter().find_map(|g| {
                            g.members.iter().find(|m| m.window == newcomer).map(|m| m.rel)
                        });
                        let from = from.expect("the pass brings it");
                        let Some(Value::Point(container)) =
                            flight.score.sample(Node::Container(key), now)
                        else {
                            panic!("{tag}: {key:?} is not drawn");
                        };
                        let want = OverlayRect::new(
                            OverlayPoint::new(
                                from.origin.x + container.x - start.x,
                                from.origin.y + container.y - start.y,
                            ),
                            from.size.respace(),
                        );
                        let is = flight.score.drawn(newcomer, now).expect("drawn");
                        let gap = gap(is, want);
                        assert!(
                            gap <= 0.5 + 1e-6,
                            "{tag}: {newcomer:?} starts {gap} from beside the strip: {is:?}, wants {want:?}"
                        );
                        brought += 1;
                    }
                    for (window, was) in before {
                        let is = flight
                            .score
                            .drawn(window, now)
                            .unwrap_or_else(|| panic!("{tag}: {window:?} gone"));
                        let gap = gap(is, was);
                        assert!(
                            gap <= 0.5 + 1e-6,
                            "{tag}: {window:?} jumped {gap}: {was:?} then {is:?}"
                        );
                    }
                }
            }
            assert!(brought > 0, "{look:?} brought no window beside the strip");
        }
    }

    /// A pass as the reactor builds one for `flight`, from where its last pass sends every window: a
    /// pan of every rigid window, or a layout pass over some of the windows, moving or resizing each;
    /// either at times bringing `newcomer`, which the flight then draws. The pan, for a pan.
    fn random_pass(
        rng: &mut Gen,
        flight: &mut Flight,
        newcomer: WindowId,
    ) -> (ReflowPlan, Option<OverlayVector>) {
        let windows = flight.plan.windows();
        let position = |plan: &FlightPlan, key: GroupKey| plan.position_of(key);
        let mut pan = None;
        let incoming = match rng.below(3) {
            0 => {
                let d = OverlayVector::new(rng.pt(-9.0, 9.0) * 100.0 + 50.0, 0.0);
                pan = Some(d);
                let mut p = ReflowPlan::empty();
                let mut requests = Vec::new();
                for w in &windows {
                    if let Some(Member::Rigid { key, rel }) = flight.plan.member(*w) {
                        let from = screen(overlay_of(rel, position(&flight.plan, key)));
                        requests.push(Resolved::tiled(*w, from, shifted(from, d.x)));
                    }
                }
                if rng.coin() {
                    bring(&mut requests, newcomer, rng);
                    flight.drawn.push(newcomer);
                }
                let pieces = reflow_plan(&requests, DISPLAY);
                p.groups = pieces.groups;
                p
            }
            _ => {
                let v1 = ScreenVector::new(rng.pt(-9.0, 9.0) * 50.0, 0.0);
                let (dx, dy) = (rng.pt(-9.0, 9.0), rng.pt(-2.0, 2.0));
                let v2 = ScreenVector::new(dx * 50.0, dy * 50.0);
                let mut requests = Vec::new();
                for w in &windows {
                    if rng.coin() {
                        continue;
                    }
                    let v = if rng.coin() { v1 } else { v2 };
                    match flight.plan.member(*w) {
                        Some(Member::Rigid { key, rel }) => {
                            let at = position(&flight.plan, key);
                            let from = screen(overlay_of(rel, at));
                            let to = if rng.below(4) > 0 {
                                rect(
                                    from.origin.x + v.x,
                                    from.origin.y + v.y,
                                    from.size.width,
                                    from.size.height,
                                )
                            } else {
                                rect(
                                    from.origin.x,
                                    from.origin.y,
                                    from.size.width + 300.0,
                                    from.size.height,
                                )
                            };
                            requests.push(Resolved::tiled(*w, from, to));
                        }
                        Some(Member::Changing { from, to }) => {
                            let loose = position(&flight.plan, GroupKey::Loose);
                            let at = |rel| screen(overlay_of(rel, loose));
                            requests.push(Resolved::tiled(*w, at(from), shifted(at(to), v.x)));
                        }
                        _ => {}
                    }
                }
                if rng.coin() {
                    if requests.is_empty() {
                        let from = rng.on_screen();
                        requests.push(Resolved::tiled(newcomer, from, shifted(from, v1.x)));
                    } else {
                        bring(&mut requests, newcomer, rng);
                    }
                    flight.drawn.push(newcomer);
                }
                reflow_plan(&requests, DISPLAY)
            }
        };
        (incoming, pan)
    }

    /// A window brought beside the last rigid one of `requests` as the last pass left it, three
    /// points to its right, travelling as it does.
    fn bring(requests: &mut Vec<Resolved>, newcomer: WindowId, rng: &mut Gen) {
        let Some(&Resolved { start: from, end: to, .. }) =
            requests.iter().rev().find(|r| r.start.size == r.end.size)
        else {
            let from = rng.on_screen();
            requests.push(Resolved::tiled(newcomer, from, from));
            return;
        };
        let beside = shifted(from, from.size.width + 3.0);
        let lands = ScreenRect::new(
            Point::new(
                beside.origin.x + to.origin.x - from.origin.x,
                beside.origin.y + to.origin.y - from.origin.y,
            ),
            beside.size,
        );
        requests.push(Resolved::tiled(newcomer, beside, lands));
    }

    /// Where `incoming` sends `window` on the overlay.
    fn sent_by(incoming: &ReflowPlan, window: WindowId) -> Option<OverlayRect> {
        Some(match incoming.member(window)? {
            Member::Rigid { key, rel } => {
                let group = incoming.groups.iter().find(|g| g.key == key)?;
                overlay_of(rel, group.travel.to_point())
            }
            Member::Changing { to, .. }
            | Member::Entrance { to, .. }
            | Member::Floating { to, .. } => on_overlay(to),
        })
    }

    /// Once every track of `flight` has ended, from `now`: nothing plays, and every window it draws
    /// is where its plan sends it, each edge within a point.
    fn lands_on_its_plan(flight: &Flight, now: f64, tag: &str) -> Result<(), String> {
        let end = now + flight.score.remaining(now);
        if !flight.score.settled(end) {
            return Err(format!("{tag}: plays past its end"));
        }
        for &window in &flight.drawn {
            let drawn = flight.score.drawn(window, end);
            let lands = flight.plan.destination(window);
            let (Some(drawn), Some(lands)) = (drawn, lands) else {
                return Err(format!("{tag}: {window:?} drawn {drawn:?}, sent to {lands:?}"));
            };
            if edges_apart(drawn, lands) > 1.0 {
                return Err(format!(
                    "{tag}: {window:?} lands at {drawn:?}, sent to {lands:?}"
                ));
            }
        }
        Ok(())
    }

    /// The fold contract, over flights of the look `make` gives: one to three passes folded in before
    /// the motion, as `random_pass` builds them, frame zero posed again after each. The plan folded
    /// names every window it named and none twice, and sends every window the pass sends onto the
    /// viewport where the pass sends it, within `GROUP_TOLERANCE` (one sent off it may ride its
    /// container out); frame zero draws every window the flight draws where the folded plan starts
    /// it, which is where its tile's picture is placed, within the half point placing on whole points
    /// allows; and the motion lands every window where the folded plan sends it.
    fn folds_each_pass_before_the_motion(
        make: &dyn Fn() -> Box<dyn Choreography>,
    ) -> Result<(), String> {
        let mut rng = Gen(167);
        for run in 0..RUNS {
            let mut flight = Flight::posed(make(), &random_requests(&mut rng), |_| true);
            for step in 0..1 + rng.below(3) {
                let tag = format!("seed 167 run {run} step {step}");
                let named = flight.plan.windows();
                let (incoming, pan) = random_pass(&mut rng, &mut flight, wid(500 + step as u32));
                let pass = Pass {
                    plan: &incoming,
                    pan,
                    focus: None,
                    viewport: VIEWPORT,
                    drawn: &flight.drawn,
                    seconds: PLAIN,
                    cue: Cue::Plain,
                    accent: None,
                };
                let folded = flight.choreography.fold(&flight.plan, &pass);
                let mut names = folded.windows();
                let count = names.len();
                names.sort();
                names.dedup();
                if names.len() != count {
                    return Err(format!("{tag}: a window named twice in {folded:?}"));
                }
                if let Some(lost) = named.iter().find(|w| folded.member(**w).is_none()) {
                    return Err(format!("{tag}: {lost:?} is no longer named"));
                }
                for window in incoming.windows() {
                    let Some(want) = sent_by(&incoming, window) else {
                        continue;
                    };
                    if is_off_screen(VIEWPORT, want) {
                        continue;
                    }
                    let got = folded.destination(window);
                    let honoured = got.is_some_and(|got| {
                        (got.origin.x - want.origin.x).abs() <= GROUP_TOLERANCE + 0.01
                            && (got.origin.y - want.origin.y).abs() <= GROUP_TOLERANCE + 0.01
                    });
                    if !honoured {
                        return Err(format!(
                            "{tag}: {window:?} sent to {got:?}, the pass sends it to {want:?}"
                        ));
                    }
                }
                flight.plan = folded;
                flight.score = flight.choreography.pose(&stage(&flight.plan, &flight.drawn, &[]));
                for &window in &flight.drawn {
                    let starts = flight.plan.member(window).map(|m| on_overlay(m.start_frame()));
                    let drawn = flight.score.drawn(window, BEGIN);
                    let posed =
                        matches!((drawn, starts), (Some(d), Some(s)) if edges_apart(d, s) <= 0.5);
                    if !posed {
                        return Err(format!(
                            "{tag}: frame zero draws {window:?} at {drawn:?}, it starts at {starts:?}"
                        ));
                    }
                }
            }
            flight.start(&[]);
            lands_on_its_plan(&flight, BEGIN, &format!("seed 167 run {run}"))?;
        }
        Ok(())
    }

    /// The push contract, over flights of the look `make` gives: one to three pushes against an end
    /// reaching a flight that moves, each 10-300ms after the one before. No window the flight draws
    /// jumps as a push reaches it, within the half point placing on whole points allows; some push
    /// draws a window more than a point further toward its wall than the flight without it draws it;
    /// and once every track has ended, every window is home where its plan sends it.
    fn pushes_out_and_home_without_a_jump(
        make: &dyn Fn() -> Box<dyn Choreography>,
    ) -> Result<(), String> {
        let walls = [
            OverlayVector::new(-EDGE_BOUNCE_OVERSHOOT, 0.0),
            OverlayVector::new(EDGE_BOUNCE_OVERSHOOT, 0.0),
            OverlayVector::new(0.0, -EDGE_BOUNCE_OVERSHOOT),
            OverlayVector::new(0.0, EDGE_BOUNCE_OVERSHOOT),
        ];
        let mut rng = Gen(173);
        let mut toward = 0;
        for run in 0..RUNS {
            let mut flight = Flight::posed(make(), &random_requests(&mut rng), |_| true);
            flight.start(&[]);
            let mut now = BEGIN;
            for step in 0..1 + rng.below(3) {
                let tag = format!("seed 173 run {run} step {step}");
                now += rng.pt(10.0, 300.0) / 1000.0;
                let wall = walls[rng.below(4) as usize];
                let without = flight.score.clone();
                let edits = flight.choreography.push(&flight.score, &Cue::Plain, wall, PLAIN, now);
                flight.fold(&edits);
                for &window in &flight.drawn {
                    let (was, is) = (without.drawn(window, now), flight.score.drawn(window, now));
                    let (Some(was), Some(is)) = (was, is) else {
                        return Err(format!("{tag}: {window:?} drawn {was:?} then {is:?}"));
                    };
                    if gap(is, was) > 0.5 + 1e-6 {
                        return Err(format!(
                            "{tag}: {window:?} jumped at the push against {wall:?}: {was:?} then {is:?}"
                        ));
                    }
                }
                let further = |at: f64, window: &WindowId| {
                    let (Some(was), Some(is)) =
                        (without.drawn(*window, at), flight.score.drawn(*window, at))
                    else {
                        return false;
                    };
                    let (dx, dy) = (is.origin.x - was.origin.x, is.origin.y - was.origin.y);
                    (dx * wall.x + dy * wall.y) / EDGE_BOUNCE_OVERSHOOT > 1.0
                };
                let pushed = (1..8).any(|k| {
                    let at = now + PLAIN * k as f64 / 8.0;
                    flight.drawn.iter().any(|window| further(at, window))
                });
                toward += usize::from(pushed);
            }
            lands_on_its_plan(&flight, now, &format!("seed 173 run {run}"))?;
        }
        if toward == 0 {
            return Err(format!("no push in {RUNS} runs drew a window toward its wall"));
        }
        Ok(())
    }

    /// Every look keeps the fold contract.
    #[test]
    fn every_look_folds_each_pass_before_the_motion() {
        for look in Look::ALL {
            if let Err(broken) = folds_each_pass_before_the_motion(&|| look.choreography()) {
                panic!("{look:?}: {broken}");
            }
        }
    }

    /// Every look keeps the push contract.
    #[test]
    fn every_look_pushes_out_and_home_without_a_jump() {
        for look in Look::ALL {
            if let Err(broken) = pushes_out_and_home_without_a_jump(&|| look.choreography()) {
                panic!("{look:?}: {broken}");
            }
        }
    }

    /// The fold contract rejects a look whose fold keeps the plan it is given.
    #[test]
    fn the_fold_contract_rejects_a_look_that_ignores_the_pass() {
        let broken = folds_each_pass_before_the_motion(&|| Flawed::boxed(Flaw::IgnoresThePass))
            .expect_err("a fold that ignores the pass");
        assert!(broken.contains("the pass sends it to"), "{broken}");
    }

    /// The push contract rejects a look that plays no push, and one that starts every bounce from
    /// rest wherever the one before is drawn.
    #[test]
    fn the_push_contract_rejects_a_look_that_plays_no_push_or_pushes_from_rest() {
        let still = pushes_out_and_home_without_a_jump(&|| Flawed::boxed(Flaw::PlaysNoPush))
            .expect_err("a push that plays nothing");
        assert!(still.contains("toward its wall"), "{still}");
        let from_rest = pushes_out_and_home_without_a_jump(&|| Flawed::boxed(Flaw::PushesFromRest))
            .expect_err("a push from rest over a bounce");
        assert!(from_rest.contains("jumped"), "{from_rest}");
    }

    /// The one contract a `Flawed` look breaks.
    #[derive(Clone, Copy, Debug)]
    enum Flaw {
        /// Its fold gives back the plan it is given.
        IgnoresThePass,
        /// Its push plays nothing.
        PlaysNoPush,
        /// Its push starts every carrier's bounce from rest, wherever the one before is drawn.
        PushesFromRest,
    }

    /// Slide but for one flaw, so a contract's check is seen to reject a look that breaks it.
    #[derive(Debug)]
    struct Flawed {
        flaw: Flaw,
        slide: Slide,
    }

    impl Flawed {
        fn boxed(flaw: Flaw) -> Box<dyn Choreography> {
            Box::new(Flawed { flaw, slide: Slide::default() })
        }
    }

    impl Choreography for Flawed {
        fn plan(&self, pass: ReflowPlan, cue: &Cue) -> FlightPlan { self.slide.plan(pass, cue) }

        fn fold(&self, plan: &FlightPlan, pass: &Pass) -> FlightPlan {
            match self.flaw {
                Flaw::IgnoresThePass => plan.clone(),
                Flaw::PlaysNoPush | Flaw::PushesFromRest => self.slide.fold(plan, pass),
            }
        }

        fn pose(&self, stage: &Stage) -> Score { self.slide.pose(stage) }

        fn compose(&mut self, playing: &Score, stage: &Stage, begin: f64) -> Vec<Edit> {
            self.slide.compose(playing, stage, begin)
        }

        fn retarget(
            &mut self,
            playing: &Score,
            plan: &FlightPlan,
            pass: &Pass,
            now: f64,
        ) -> Retarget {
            self.slide.retarget(playing, plan, pass, now)
        }

        fn push(
            &mut self,
            playing: &Score,
            cue: &Cue,
            wall: OverlayVector,
            seconds: f64,
            now: f64,
        ) -> Vec<Edit> {
            let bounce = Bounce::from_rest(wall, Duration::from_secs_f64(seconds));
            match self.flaw {
                Flaw::IgnoresThePass => self.slide.push(playing, cue, wall, seconds, now),
                Flaw::PlaysNoPush => Vec::new(),
                Flaw::PushesFromRest => playing
                    .containers()
                    .filter(|key| carries_out_and_back(*key, wall))
                    .map(|key| Edit::Add(bounce.track(key, now)))
                    .collect(),
            }
        }
    }

    /// Whatever the press and the strip, a look stages no window but those of the pair it is given,
    /// and each once: every other window stays where the reactor put it, at its new frame.
    #[test]
    fn every_look_stages_only_the_pair_it_is_given() {
        let directions = [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ];
        let area = Area {
            frame: VIEWPORT,
            outer_left: 4.0,
            outer_right: 4.0,
        };
        for look in Look::ALL {
            let mut rng = Gen(37);
            let mut staged = 0;
            for run in 0..RUNS {
                let count = 1 + rng.below(6) as u32;
                let mut x = rng.pt(-1800.0, 0.0);
                let windows: Vec<(WindowId, SurfaceRect)> = (1..=count)
                    .map(|i| {
                        let width = if rng.coin() {
                            1720.0
                        } else {
                            rng.pt(400.0, 1720.0)
                        };
                        let frame = rect(x, 32.0, width, 1081.0);
                        x += width + 3.0;
                        (wid(i), frame)
                    })
                    .collect();
                let mut pair: Vec<PairStart> = Vec::new();
                for &(window, frame) in &windows {
                    if rng.below(3) == 0 {
                        pair.push(PairStart {
                            window,
                            start: shifted(frame, rng.pt(-900.0, 900.0)),
                            passed: rng.coin(),
                        });
                    }
                }
                let cue = match rng.below(3) {
                    0 => Cue::Plain,
                    _ => Cue::Move {
                        window: wid(1 + rng.below(count as u64 + 1) as u32),
                        direction: directions[rng.below(4) as usize],
                    },
                };
                let strip = Strip {
                    windows: &windows,
                    pair: &pair,
                    area,
                    travel: OverlayVector::new(rng.pt(-900.0, 900.0), 0.0),
                    seconds: PLAIN,
                };
                let staging = look.stage(&cue, &strip);
                let mut seen: Vec<WindowId> = Vec::new();
                for &(window, _) in staging.from.iter().chain(&staging.held) {
                    assert!(
                        pair.iter().any(|p| p.window == window),
                        "{look:?} run {run} {cue:?}: {window:?} is not of the pair {pair:?}"
                    );
                    assert!(!seen.contains(&window), "{look:?} run {run}: {window:?} twice");
                    seen.push(window);
                }
                staged += seen.len();
            }
            assert!(staged > 0, "{look:?} staged nothing in {RUNS} runs");
        }
    }
}
