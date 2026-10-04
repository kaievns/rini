//! Looks: how a flight moves. A look stages a press on the strip before the flight is asked for
//! (`Look::stage`), and for each flight its `Choreography` makes the plan, poses frame zero, composes
//! the motion, folds a press before it and bends the flight at a press after it, answering with the
//! score's edits (`motion::score::Edit`), which the renderer plays as they are. `Look::ALL` is every
//! look, and the tests below are the contract each keeps; `slide` is the default. See "The overlay
//! engine" in `src/animation/docs/animation-smoothness.md`.

pub mod slide;

use std::fmt::Debug;

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::WindowId;
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
    pub start: CGRect,
    pub passed: bool,
}

/// The screen a strip is shown on, in the overlay's space, and its outer gaps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub frame: CGRect,
    pub outer_left: f64,
    pub outer_right: f64,
}

/// A strip movement as a look stages it: every tiled window at its new frame, the pair a move
/// swapped, the screen, and the viewport's travel, which is where the camera starts.
#[derive(Clone, Copy, Debug)]
pub struct Strip<'a> {
    pub windows: &'a [(WindowId, CGRect)],
    pub pair: &'a [PairStart],
    pub area: Area,
    pub travel: CGPoint,
}

/// What a look staged: windows that start elsewhere and cross from there, windows drawn held at a
/// frame on the strip while their real windows go to their layout frames, and the accent riding the
/// movement.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Staging {
    pub from: Vec<(WindowId, CGRect)>,
    pub held: Vec<(WindowId, CGRect)>,
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
    pub pan: Option<CGPoint>,
    /// The window gaining focus, which wins a tied vote.
    pub focus: Option<WindowId>,
    /// The display in the overlay's space: what a member's destination is judged against.
    pub viewport: CGRect,
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
        wall: CGPoint,
        seconds: f64,
        now: f64,
    ) -> Vec<Edit>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::domain::motion::plan::{GroupKey, Member, overlay_of, reflow_plan};
    use crate::animation::domain::motion::score::{Node, Value};
    use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, random_requests, rect};

    const PLAIN: f64 = 0.35;
    const BEGIN: f64 = 10.0;

    fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

    fn shifted(frame: CGRect, dx: f64) -> CGRect {
        rect(
            frame.origin.x + dx,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        )
    }

    /// The largest of the four differences between two frames.
    fn gap(a: CGRect, b: CGRect) -> f64 {
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
    fn edges_apart(a: CGRect, b: CGRect) -> f64 {
        [
            a.min().x - b.min().x,
            a.min().y - b.min().y,
            a.max().x - b.max().x,
            a.max().y - b.max().y,
        ]
        .into_iter()
        .fold(0.0f64, |worst, d| worst.max(d.abs()))
    }

    /// Where `plan` lands `window`, in the overlay's space.
    fn destination(plan: &FlightPlan, window: WindowId) -> CGRect {
        match plan.member(window).expect("in the plan") {
            Member::Rigid { key, rel } => overlay_of(rel, plan.position_of(key)),
            Member::Changing { to, .. } | Member::Entrance { to, .. } => to,
            Member::Floating { to, .. } => overlay_of(to, plan.position_of(GroupKey::Floating)),
        }
    }

    /// One flight of a look, every edit it answers folded into its score.
    struct Flight {
        choreography: Box<dyn Choreography>,
        plan: FlightPlan,
        drawn: Vec<WindowId>,
        score: Score,
    }

    impl Flight {
        /// `requests` posed at frame zero, the windows `drawn` keeps drawn.
        fn posed(
            look: Look,
            requests: &[(WindowId, CGRect, CGRect, bool)],
            drawn: impl Fn(WindowId) -> bool,
        ) -> Flight {
            let choreography = look.choreography();
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
        fn merge(&mut self, incoming: &ReflowPlan, pan: Option<CGPoint>, now: f64) {
            let pass = Pass {
                plan: incoming,
                pan,
                focus: None,
                viewport: DISPLAY,
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
                    requests.iter().map(|r| r.0).filter(|_| rng.below(4) == 0).collect();
                let flight = Flight::posed(look, &requests, |w| !left_out.contains(&w));
                assert!(
                    flight.score.settled(BEGIN),
                    "{look:?} run {run}: frame zero plays"
                );
                for &(window, from, _, _) in &requests {
                    let drawn = flight.score.drawn(window, BEGIN);
                    if left_out.contains(&window) {
                        assert_eq!(drawn, None, "{look:?} run {run}: {window:?} is not drawn");
                        continue;
                    }
                    let drawn = drawn.unwrap_or_else(|| panic!("{look:?} run {run}: {window:?}"));
                    assert!(
                        edges_apart(drawn, from) <= 0.5,
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
                offset: CGPoint::new(-576.0, 0.0),
            }],
            vec![Accent::Push {
                wall: CGPoint::new(72.0, 0.0),
                seconds: PLAIN,
            }],
        ];
        for look in Look::ALL {
            let mut rng = Gen(31);
            for run in 0..RUNS {
                let mut flight = Flight::posed(look, &random_requests(&mut rng), |_| true);
                let accents = &waiting[rng.below(waiting.len() as u64) as usize];
                flight.start(accents);
                let end = BEGIN + flight.score.remaining(BEGIN);
                assert!(flight.score.settled(end), "{look:?} run {run}");
                for &window in &flight.drawn {
                    let drawn = flight.score.drawn(window, end).expect("drawn");
                    let lands = destination(&flight.plan, window);
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
                let mut flight = Flight::posed(look, &random_requests(&mut rng), |_| true);
                flight.start(&[]);
                let mut now = BEGIN;
                for step in 0..1 + rng.below(4) {
                    let tag = format!("{look:?} seed 163 run {run} step {step}");
                    now += rng.pt(10.0, 400.0) / 1000.0;
                    let windows = flight.plan.windows();
                    let position = |plan: &FlightPlan, key: GroupKey| plan.position_of(key);
                    let newcomer = wid(500 + step as u32);
                    let mut pan = None;
                    let incoming = match rng.below(3) {
                        0 => {
                            let d = CGPoint::new(rng.pt(-9.0, 9.0) * 100.0 + 50.0, 0.0);
                            pan = Some(d);
                            let mut p = ReflowPlan::empty();
                            let mut requests = Vec::new();
                            for w in &windows {
                                if let Some(Member::Rigid { key, rel }) = flight.plan.member(*w) {
                                    let from = overlay_of(rel, position(&flight.plan, key));
                                    requests.push((*w, from, shifted(from, d.x), false));
                                }
                            }
                            if rng.coin() {
                                bring(&mut requests, newcomer, &mut rng);
                                flight.drawn.push(newcomer);
                            }
                            let pieces = reflow_plan(&requests, DISPLAY);
                            p.groups = pieces.groups;
                            p
                        }
                        _ => {
                            let v1 = CGPoint::new(rng.pt(-9.0, 9.0) * 50.0, 0.0);
                            let v2 =
                                CGPoint::new(rng.pt(-9.0, 9.0) * 50.0, rng.pt(-2.0, 2.0) * 50.0);
                            let mut requests = Vec::new();
                            for w in &windows {
                                if rng.coin() {
                                    continue;
                                }
                                let v = if rng.coin() { v1 } else { v2 };
                                match flight.plan.member(*w) {
                                    Some(Member::Rigid { key, rel }) => {
                                        let from = overlay_of(rel, position(&flight.plan, key));
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
                                        requests.push((*w, from, to, false));
                                    }
                                    Some(Member::Changing { from, to }) => {
                                        requests.push((*w, from, shifted(to, v.x), false));
                                    }
                                    _ => {}
                                }
                            }
                            if rng.coin() {
                                if requests.is_empty() {
                                    let from = rng.on_screen();
                                    requests.push((newcomer, from, shifted(from, v1.x), false));
                                } else {
                                    bring(&mut requests, newcomer, &mut rng);
                                }
                                flight.drawn.push(newcomer);
                            }
                            reflow_plan(&requests, DISPLAY)
                        }
                    };
                    let before: Vec<(WindowId, CGRect)> = windows
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
                        let want = CGRect::new(
                            CGPoint::new(
                                from.origin.x + container.x - start.x,
                                from.origin.y + container.y - start.y,
                            ),
                            from.size,
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

    /// A window brought beside the last rigid one of `requests` as the last pass left it, three
    /// points to its right, travelling as it does.
    fn bring(
        requests: &mut Vec<(WindowId, CGRect, CGRect, bool)>,
        newcomer: WindowId,
        rng: &mut Gen,
    ) {
        let Some(&(_, from, to, _)) =
            requests.iter().rev().find(|(_, from, to, _)| from.size == to.size)
        else {
            let from = rng.on_screen();
            requests.push((newcomer, from, from, false));
            return;
        };
        let beside = shifted(from, from.size.width + 3.0);
        let lands = CGRect::new(
            CGPoint::new(
                beside.origin.x + to.origin.x - from.origin.x,
                beside.origin.y + to.origin.y - from.origin.y,
            ),
            beside.size,
        );
        requests.push((newcomer, beside, lands, false));
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
            frame: DISPLAY,
            outer_left: 4.0,
            outer_right: 4.0,
        };
        for look in Look::ALL {
            let mut rng = Gen(37);
            let mut staged = 0;
            for run in 0..RUNS {
                let count = 1 + rng.below(6) as u32;
                let mut x = rng.pt(-1800.0, 0.0);
                let windows: Vec<(WindowId, CGRect)> = (1..=count)
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
                    travel: CGPoint::new(rng.pt(-900.0, 900.0), 0.0),
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
