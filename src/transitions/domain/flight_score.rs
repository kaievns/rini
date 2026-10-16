//! A flight's score: everything it draws, as the score its look composes and bends, and the accents
//! waiting for its motion to start. The look is the flight's, chosen as it begins. Every method folds
//! what it returns into the score before returning it, so the edits handed to the renderer and the
//! score here are one stream. See "The score" in `src/animation/docs/animation-smoothness.md`, and
//! "Mid-flight passes" and "Edge bounce" in `src/transitions/docs/coordination.md` for when each
//! happens.

use std::time::Duration;

use rini_core::ids::WindowId;
use rini_geometry::space::{Group, OverlayRect, OverlayVector, Size};

use crate::animation::domain::choreography::{Choreography, Cue, Look, Pass, Retarget, Stage};
use crate::animation::domain::motion::accents::Accent;
use crate::animation::domain::motion::plan::{Banding, FlightPlan, ReflowPlan};
use crate::animation::domain::motion::score::{Channel, Edit, Node, Score, Value};
use crate::animation::domain::motion::tile;

#[derive(Debug)]
pub struct FlightScore {
    look: Look,
    choreography: Box<dyn Choreography>,
    score: Score,
    /// At most one of each kind, until the motion starts.
    waiting: Vec<Accent>,
}

impl FlightScore {
    pub fn new(look: Look) -> FlightScore {
        FlightScore {
            look,
            choreography: look.choreography(),
            score: Score::default(),
            waiting: Vec::new(),
        }
    }

    pub fn score(&self) -> &Score { &self.score }

    /// The look the flight began with, which stages and bends every pass merged into it.
    pub fn look(&self) -> Look { self.look }

    /// The flight's plan from its first pass, as its look makes it.
    pub fn plan(&self, pass: ReflowPlan, cue: &Cue) -> FlightPlan {
        self.choreography.plan(pass, cue)
    }

    /// Frame zero, posed again whenever it is installed again before the motion: nothing playing,
    /// banded by `banding`. The accents waiting keep waiting.
    pub fn pose(&mut self, stage: &Stage, banding: Banding) -> &Score {
        self.score = self.choreography.pose(stage);
        self.score.apply(&Edit::Bank(banding));
        &self.score
    }

    /// Keeps `accent` until the motion starts, in place of one of its kind already waiting: frame zero
    /// may not be on screen yet, and every pose before the motion would drop it.
    pub fn wait(&mut self, accent: Accent) {
        self.waiting
            .retain(|waiting| std::mem::discriminant(waiting) != std::mem::discriminant(&accent));
        self.waiting.push(accent);
    }

    /// A pass folded into the flight before it moves; the pose follows.
    pub fn fold(&self, plan: &FlightPlan, pass: &Pass) -> FlightPlan {
        self.choreography.fold(plan, pass)
    }

    /// The motion from frame zero at `now`, with every accent that waited for it.
    pub fn start_moving(&mut self, stage: &Stage, now: f64) -> Vec<Edit> {
        let waiting = std::mem::take(&mut self.waiting);
        let stage = Stage { accents: &waiting, ..*stage };
        let edits = self.choreography.compose(&self.score, &stage, now);
        self.fold_in(&edits);
        edits
    }

    /// A later pass bending the flight that moves, at `now`, from where the score has its movement.
    pub fn merge(&mut self, plan: &FlightPlan, pass: &Pass, now: f64) -> Retarget {
        let retarget = self.choreography.retarget(&self.score, plan, pass, now);
        self.fold_in(&retarget.edits);
        retarget
    }

    /// A push against `wall` reaching the flight that moves, at `now`.
    pub fn push(&mut self, cue: &Cue, wall: OverlayVector, seconds: f64, now: f64) -> Vec<Edit> {
        let edits = self.choreography.push(&self.score, cue, wall, seconds, now);
        self.fold_in(&edits);
        edits
    }

    /// `window`'s tile onto `real`, its window's frame, at `now`: the coordinator has decided it.
    pub fn snap(&mut self, window: WindowId, real: OverlayRect, now: f64) -> Vec<Edit> {
        let edits = tile::snap(&self.score, window, real, now);
        self.fold_in(&edits);
        edits
    }

    /// When `window`'s tile turns the corner of its leg in two steps, on the media clock; none for a
    /// tile on one movement or with no leg.
    pub fn corner_at(&self, window: WindowId) -> Option<f64> {
        let track = self.score.track(Node::Tile(window), Channel::Base)?;
        Some(track.begin() + tile::corner_key(track)? * track.seconds())
    }

    /// The size `window`'s tile's leg starts at, where it was drawn as the leg was made; none for a
    /// tile with no leg.
    pub fn leg_start(&self, window: WindowId) -> Option<Size<Group>> {
        let track = self.score.track(Node::Tile(window), Channel::Base)?;
        match track.keys().first()?.1 {
            Value::Rect(start) => Some(start.size),
            Value::Point(_) => None,
        }
    }

    /// The size `window`'s tile is drawn at `at`: on its leg while that plays, where it rests
    /// otherwise, so a leg made at `at` starts there. None for a tile the score does not hold.
    pub fn drawn_size(&self, window: WindowId, at: f64) -> Option<Size<Group>> {
        match self.score.along(Node::Tile(window), at)? {
            Value::Rect(drawn) => Some(drawn.size),
            Value::Point(_) => None,
        }
    }

    /// `window`'s tile's leg added again as it plays, so the renderer installs it with the pictures
    /// the tile carries now; nothing for a tile with no leg. The score is as it was.
    pub fn again(&self, window: WindowId) -> Vec<Edit> {
        self.score
            .track(Node::Tile(window), Channel::Base)
            .cloned()
            .map(Edit::Add)
            .into_iter()
            .collect()
    }

    /// The depths a look's edits are drawn at, appended after them.
    pub fn bank(&mut self, banding: Banding) -> Edit {
        let edit = Edit::Bank(banding);
        self.score.apply(&edit);
        edit
    }

    /// How long from `now` until every track has ended.
    pub fn remaining(&self, now: f64) -> Duration {
        Duration::from_secs_f64(self.score.remaining(now))
    }

    /// The motion clock a merge restarts at `now`: the pass's own length, or the score's when that
    /// is longer.
    pub fn clock(&self, plain: Duration, now: f64) -> Duration { plain.max(self.remaining(now)) }

    /// How long from `now` the clock runs on after a merge that moved nothing, with `left` of it to
    /// go: as long as the score plays when that outlasts it, as a step the merge took does, and not
    /// at all otherwise, so a pass repeating the flight's destinations never holds off its lift.
    pub fn outlasting(&self, left: Duration, now: f64) -> Option<Duration> {
        let plays = self.remaining(now);
        (plays > left).then_some(plays)
    }

    pub fn settled(&self, now: f64) -> bool { self.score.settled(now) }

    fn fold_in(&mut self, edits: &[Edit]) {
        for edit in edits {
            self.score.apply(edit);
        }
    }
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::ScreenRect;

    use super::*;
    use crate::animation::domain::motion::nudge::NUDGE_STRETCH;
    use crate::animation::domain::motion::plan::{
        GroupKey, ReflowPlan, Resolved, installed, reflow_plan,
    };
    use crate::animation::domain::motion::score::{Channel, Node, Value};
    use crate::animation::domain::motion::testing::{DISPLAY, VIEWPORT, rect};

    const PLAIN: f64 = 0.35;
    const BEGIN: f64 = 10.0;

    fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

    fn column(i: f64) -> ScreenRect { rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0) }

    fn shifted(frame: ScreenRect, dx: f64) -> ScreenRect {
        rect(
            frame.origin.x + dx,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        )
    }

    /// Columns `0..count` from `first`, all stepping `dx` from `at` columns along.
    fn strip(first: u32, count: u32, at: f64, dx: f64) -> Vec<Resolved> {
        (0..count)
            .map(|i| {
                let from = column(at + i as f64);
                Resolved::tiled(wid(first + i), from, shifted(from, dx))
            })
            .collect()
    }

    struct Flight {
        score: FlightScore,
        plan: FlightPlan,
        drawn: Vec<WindowId>,
        /// The fold of everything `score` returned, as a renderer's replica is.
        replica: Score,
    }

    impl Flight {
        fn posed(requests: &[Resolved]) -> Flight {
            let mut score = FlightScore::new(Look::default());
            let plan = score.plan(reflow_plan(requests, DISPLAY), &Cue::Plain);
            let drawn = plan.windows();
            let replica = score.pose(&stage(&plan, &drawn), Banding::default()).clone();
            Flight { score, plan, drawn, replica }
        }

        fn played(&mut self, edits: &[Edit]) {
            for edit in edits {
                self.replica.apply(edit);
            }
        }

        fn start(&mut self) -> Vec<Edit> {
            let edits = self.score.start_moving(&stage(&self.plan, &self.drawn), BEGIN);
            self.played(&edits);
            edits
        }

        fn merge(
            &mut self,
            incoming: &ReflowPlan,
            pan: Option<OverlayVector>,
            seconds: f64,
            now: f64,
        ) -> Retarget {
            self.merge_with(incoming, pan, None, seconds, now)
        }

        fn merge_with(
            &mut self,
            incoming: &ReflowPlan,
            pan: Option<OverlayVector>,
            accent: Option<Accent>,
            seconds: f64,
            now: f64,
        ) -> Retarget {
            let pass = Pass {
                plan: incoming,
                pan,
                focus: None,
                viewport: VIEWPORT,
                drawn: &self.drawn,
                seconds,
                cue: Cue::Plain,
                accent,
            };
            let mut retarget = self.score.merge(&self.plan, &pass, now);
            self.plan = retarget.plan.clone();
            retarget.edits.push(self.score.bank(Banding {
                group_order: self.plan.groups.iter().map(|g| g.key).collect(),
                ..Banding::default()
            }));
            self.played(&retarget.edits);
            retarget
        }
    }

    fn stage<'a>(plan: &'a FlightPlan, drawn: &'a [WindowId]) -> Stage<'a> {
        Stage {
            plan,
            drawn,
            seconds: PLAIN,
            cue: Cue::Plain,
            accents: &[],
        }
    }

    fn key_of(plan: &FlightPlan, window: WindowId) -> GroupKey {
        plan.groups
            .iter()
            .find(|g| g.members.iter().any(|m| m.window == window))
            .map(|g| g.key)
            .expect("a rigid member")
    }

    fn pan(flight: &Flight, dx: f64) -> ReflowPlan {
        let requests: Vec<_> = flight
            .plan
            .windows()
            .into_iter()
            .filter_map(|w| match flight.plan.member(w)? {
                crate::animation::domain::motion::plan::Member::Rigid { key, rel } => {
                    let from = crate::animation::domain::motion::plan::overlay_of(
                        rel,
                        flight.plan.position_of(key),
                    );
                    let from = rini_geometry::from_overlay_space(from, DISPLAY);
                    Some(Resolved::tiled(w, from, shifted(from, dx)))
                }
                _ => None,
            })
            .collect();
        reflow_plan(&requests, DISPLAY)
    }

    /// A tile folding is on a leg in two steps, which turns its corner at its middle key's time on
    /// the media clock and plays again exactly as it was, starting at the column and drawn at the
    /// corner's size there; a strip window on one movement has no corner, and a window with no tile
    /// no leg.
    #[test]
    fn a_folds_corner_is_when_its_leg_turns_and_its_leg_plays_again_as_it_was() {
        let fold = Resolved::tiled(
            wid(9),
            rect(866.0, 35.0, 860.0, 1079.0),
            rect(3.0, 576.0, 1148.0, 538.0),
        );
        let mut requests = strip(1, 1, 3.0, -863.0);
        requests.push(fold);
        let mut flight = Flight::posed(&requests);
        flight.start();
        let track = flight
            .score
            .score()
            .track(Node::Tile(wid(9)), Channel::Base)
            .expect("a leg")
            .clone();
        let key = tile::corner_key(&track).expect("two steps");
        assert_eq!(flight.score.corner_at(wid(9)), Some(BEGIN + key * PLAIN));
        assert_eq!(flight.score.again(wid(9)), [Edit::Add(track)]);
        assert_eq!(flight.score.leg_start(wid(9)), Some(Size::new(860.0, 1079.0)));
        let turning = flight.score.drawn_size(wid(9), BEGIN + key * PLAIN).expect("drawn");
        assert!(
            (turning.width - 860.0).abs() < 1e-6 && (turning.height - 538.0).abs() < 1e-6,
            "{turning:?}"
        );
        assert_eq!(flight.score.corner_at(wid(1)), None, "one movement");
        assert!(flight.score.again(wid(42)).is_empty(), "no tile");
        assert_eq!(
            (
                flight.score.leg_start(wid(42)),
                flight.score.drawn_size(wid(42), BEGIN)
            ),
            (None, None)
        );
    }

    /// A merge bends the flight from its own score: every window is drawn where it was, the strip's
    /// container carries on from where the score had its movement, and the score is what it handed
    /// out.
    #[test]
    fn a_merge_bends_the_flight_from_its_score() {
        let mut flight = Flight::posed(&strip(1, 3, 1.0, -863.0));
        flight.start();
        let now = BEGIN + 0.15;
        let key = key_of(&flight.plan, wid(1));
        let along = flight.score.score().along(Node::Container(key), now);
        let before: Vec<Option<OverlayRect>> =
            flight.drawn.iter().map(|w| flight.score.score().drawn(*w, now)).collect();
        let incoming = pan(&flight, -863.0);
        let bent = flight.merge(&incoming, Some(OverlayVector::new(-863.0, 0.0)), PLAIN, now);
        assert!(bent.delta.retargeted_groups.iter().any(|(k, _)| *k == key));
        let track = flight.score.score().track(Node::Container(key), Channel::Base).expect("bent");
        assert_eq!((track.begin(), Some(track.keys()[0].1)), (now, along));
        let after: Vec<Option<OverlayRect>> =
            flight.drawn.iter().map(|w| flight.score.score().drawn(*w, now)).collect();
        assert_eq!(before, after);
        assert_eq!(flight.score.score(), &flight.replica);
    }

    /// A merge that opens a container, moves a member into it, takes a window in and re-bands is
    /// carried whole by its edits: the container's first `Place` opens it, the member's later `Ride`
    /// moves it, the newcomer's first `Ride` comes with its `Place` and the container's track, and
    /// the `Bank` last; whatever folds them has the flight's score.
    #[test]
    fn a_merge_is_carried_whole_by_its_edits() {
        let mut flight = Flight::posed(&strip(1, 2, 1.0, -863.0));
        flight.start();
        let strip_key = key_of(&flight.plan, wid(1));
        let now = BEGIN + 0.08;
        let (first, second) = (column(1.0), column(2.0));
        let newcomer = rect(500.0, 400.0, 300.0, 200.0);
        flight.drawn.push(wid(9));
        let incoming = reflow_plan(
            &[
                Resolved::tiled(wid(1), first, shifted(first, -863.0)),
                Resolved::tiled(wid(2), second, shifted(second, -563.0)),
                Resolved::tiled(wid(9), newcomer, shifted(newcomer, 120.0)),
            ],
            DISPLAY,
        );
        let before = flight.replica.clone();
        let bent = flight.merge(&incoming, None, PLAIN, now);
        let (left, joined) = (key_of(&flight.plan, wid(2)), key_of(&flight.plan, wid(9)));
        assert!(left != strip_key && joined != strip_key && left != joined);
        assert_eq!(before.parent(wid(2)), Some(strip_key));
        assert_eq!(before.parent(wid(9)), None);
        assert_eq!(before.model(Node::Container(left)), None);

        let position = |edit: &Edit| bent.edits.iter().position(|e| e == edit);
        let first_place = bent
            .edits
            .iter()
            .position(|e| matches!(e, Edit::Place(Node::Container(k), _) if *k == left));
        let ride = position(&Edit::Ride(wid(2), left)).expect("a later ride");
        assert!(
            first_place.expect("opened by a place") < ride,
            "opened before it is ridden"
        );
        let joins = position(&Edit::Ride(wid(9), joined)).expect("a first ride");
        let place = bent
            .edits
            .iter()
            .position(|e| matches!(e, Edit::Place(Node::Tile(w), _) if *w == wid(9)));
        assert!(joins < place.expect("its place"));
        assert!(bent
            .edits
            .iter()
            .any(|e| matches!(e, Edit::Add(t) if t.node() == Node::Container(joined) && t.channel() == Channel::Base)));
        assert!(matches!(bent.edits.last(), Some(Edit::Bank(_))));

        let mut folded = before;
        for edit in &bent.edits {
            folded.apply(edit);
        }
        assert_eq!(&folded, flight.score.score());
        assert_eq!(folded.parent(wid(2)), Some(left));
        assert_eq!(folded.parent(wid(9)), Some(joined));
    }

    /// The clock a merge restarts covers everything the score plays: a bounce outlasting the pass
    /// holds it, rather than restarting at the pass's legs and lifting before the bounce comes home.
    #[test]
    fn the_clock_after_a_merge_covers_the_score() {
        let mut flight = Flight::posed(&strip(1, 2, 1.0, -863.0));
        flight.start();
        let pushed = BEGIN + 0.3;
        let edits = flight.score.push(&Cue::Plain, OverlayVector::new(-72.0, 0.0), PLAIN, pushed);
        flight.played(&edits);
        let home = pushed + PLAIN;
        let now = BEGIN + 0.31;
        let incoming = pan(&flight, -863.0);
        flight.merge(&incoming, Some(OverlayVector::new(-863.0, 0.0)), 0.05, now);
        let clock = flight.score.clock(Duration::from_secs_f64(0.05), now);
        assert!(clock.as_secs_f64() >= home - now - 1e-9, "{clock:?}");
        assert_eq!(clock, flight.score.remaining(now));
        assert!(!flight.score.settled(home - 1e-6));
        let quick = FlightScore::new(Look::default());
        assert_eq!(
            quick.clock(Duration::from_millis(350), now),
            Duration::from_millis(350),
            "nothing playing: the pass's"
        );
    }

    /// A merge that moves nothing runs the clock on only for what outlasts it: a step it takes runs
    /// it on as long as the step plays, and a pass repeating the flight's destinations past the
    /// clock's end runs it on not at all. Run on for every such pass, the end moved up to each pass
    /// and the lift's grace with it, so a window that never landed held the overlay up for as long
    /// as the passes kept coming.
    #[test]
    fn a_merge_that_moves_nothing_runs_the_clock_on_only_for_what_outlasts_it() {
        let mut flight = Flight::posed(&strip(1, 2, 1.0, -863.0));
        flight.start();
        let late = BEGIN + PLAIN + 0.2;
        let again = pan(&flight, 0.0);
        let confirmed = flight.merge(&again, None, PLAIN, late);
        assert!(!confirmed.delta.moves_anything(), "{:?}", confirmed.delta);
        assert_eq!(flight.score.outlasting(Duration::ZERO, late), None);

        let now = BEGIN + 0.2;
        let step = Accent::Step {
            offset: OverlayVector::new(-576.0, 0.0),
            seconds: PLAIN,
        };
        let stepped = flight.merge_with(&ReflowPlan::empty(), None, Some(step), PLAIN, now);
        assert!(!stepped.delta.moves_anything(), "{:?}", stepped.delta);
        let left = Duration::from_secs_f64(BEGIN + PLAIN - now);
        assert!(
            stepped
                .edits
                .iter()
                .any(|e| matches!(e, Edit::Add(t) if t.channel() == Channel::Nudge))
        );
        let runs_on = flight.score.outlasting(left, now).expect("the step outlasts the clock");
        assert!(
            runs_on.as_secs_f64() >= PLAIN * NUDGE_STRETCH - 1e-9,
            "{runs_on:?}"
        );
        assert_eq!(runs_on, flight.score.remaining(now));
    }

    /// An accent arriving before the motion waits for it, a later one of its kind in place of the
    /// earlier, a step and a push side by side, kept through every pose; both begin with the motion,
    /// and a push once it moves plays at once.
    #[test]
    fn an_accent_before_the_motion_waits_for_it() {
        let mut flight = Flight::posed(&strip(1, 2, 1.0, -863.0));
        let (left, right) = (OverlayVector::new(-72.0, 0.0), OverlayVector::new(72.0, 0.0));
        let step = OverlayVector::new(-576.0, 0.0);
        flight.score.wait(Accent::Push { wall: left, seconds: PLAIN });
        flight.score.wait(Accent::Step { offset: step, seconds: PLAIN });
        flight.score.wait(Accent::Push { wall: right, seconds: PLAIN });
        let plan = flight.plan.clone();
        flight.score.pose(&stage(&plan, &flight.drawn), Banding::default());
        let edits = flight.start();
        let key = key_of(&flight.plan, wid(1));
        let accents: Vec<&crate::animation::domain::motion::score::Track> = edits
            .iter()
            .filter_map(|e| match e {
                Edit::Add(t) if t.channel() != Channel::Base => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(accents.len(), 2, "one step and the latest push: {accents:?}");
        let bounce = crate::animation::domain::motion::bounce::Bounce::from_rest(
            right,
            Duration::from_secs_f64(PLAIN),
        );
        assert!(accents.contains(&&bounce.track(key, BEGIN)), "the latest push");
        assert!(accents.iter().any(|t| t.channel() == Channel::Nudge && t.begin() == BEGIN));

        let now = BEGIN + 0.1;
        let pushed = flight.score.push(&Cue::Plain, left, PLAIN, now);
        assert!(!pushed.is_empty(), "once it moves, at once");
        assert!(pushed.iter().all(|e| matches!(e, Edit::Add(t) if t.begin() == now)));
    }

    /// A snap lands in the score at once, so the next merge bends the tile from where the snap put
    /// it, not from where it was going.
    #[test]
    fn a_snap_lands_in_the_score_before_the_next_merge() {
        let from = column(1.0);
        let resized = rect(from.origin.x, from.origin.y, 1200.0, from.size.height);
        let mut flight = Flight::posed(&[Resolved::tiled(wid(1), from, resized)]);
        flight.start();
        let real: OverlayRect = rect(from.origin.x, from.origin.y, 1199.0, from.size.height);
        let snapped = flight.score.snap(wid(1), real, BEGIN + 0.1);
        flight.played(&snapped);
        let node = Node::Tile(wid(1));
        assert_eq!(
            flight.score.score().model(node),
            Some(Value::Rect(installed(real)))
        );
        let now = BEGIN + 0.2;
        let drawn = flight.score.score().sample(node, now).expect("drawn");
        let incoming = reflow_plan(
            &[Resolved::tiled(
                wid(1),
                from,
                rect(from.origin.x, from.origin.y, 1500.0, from.size.height),
            )],
            DISPLAY,
        );
        flight.merge(&incoming, None, PLAIN, now);
        let track = flight.score.score().track(node, Channel::Base).expect("bent");
        assert_eq!(track.keys()[0].1, drawn, "from the snapped track");
        assert_eq!(flight.score.score(), &flight.replica);
    }

    /// A flight's score begins with nothing playing and nothing waiting, and a step waited for its
    /// motion is taken as it starts. Every accent is the flight's own, so the lift drops it with the
    /// flight; the engine keeps none between flights.
    #[test]
    fn a_fresh_flight_has_nothing_playing_or_waiting_and_takes_a_waited_step() {
        let mut plain = Flight::posed(&strip(1, 2, 1.0, -863.0));
        assert!(!plain.score.score().plays(Channel::Nudge, BEGIN));
        let edits = plain.start();
        assert!(
            edits.iter().all(|e| !matches!(e, Edit::Add(t) if t.channel() != Channel::Base)),
            "nothing waited"
        );
        let mut stepping = Flight::posed(&strip(1, 2, 1.0, -863.0));
        stepping.score.wait(Accent::Step {
            offset: OverlayVector::new(-576.0, 0.0),
            seconds: PLAIN,
        });
        let edits = stepping.start();
        assert!(
            edits.iter().any(|e| matches!(e, Edit::Add(t) if t.channel() == Channel::Nudge)),
            "taken"
        );
    }
}
