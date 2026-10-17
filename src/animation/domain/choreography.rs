//! Looks: how a flight moves. A look stages a press on the strip as the strip movement reaches the
//! flight engine, the look of the flight it joins or begins (`Look::stage`), and for each flight
//! its `Choreography` makes the plan, poses frame zero, composes the motion, folds a press before
//! it and bends the flight at a press after it, answering with the score's edits
//! (`motion::score::Edit`), which the renderer plays as they are. A bend's structure is every
//! look's alike (`Merged::bend`), and a look gives only each node's movement in it (`Bending`).
//! `Look::ALL` is every look, and the tests below are the contract each keeps; `slide` is the
//! default, `zoom` the arc a press along the strip plays zoomed out, the config's
//! `animations › look` picks the one every flight takes, and `linear` and `probe`, looks only the
//! tests build, keep the contract through the same seam. See "The overlay engine" in
//! `src/animation/docs/animation-smoothness.md`.

#[cfg(test)]
pub mod linear;
pub mod slide;
pub mod zoom;

use std::fmt::Debug;

use rini_core::ids::WindowId;
use rini_geometry::Round;
use rini_geometry::space::{
    GroupRect, OverlayPoint, OverlayRect, OverlayVector, Point, SurfaceRect,
};
use rini_ipc::protocol::Direction;

use self::slide::Slide;
use self::zoom::ZoomArc;
use super::motion::accents::Accent;
use super::motion::plan::{
    FlightPlan, GroupKey, Member, PlanDelta, ReflowPlan, merge_plans, stale_overlay_layers,
};
use super::motion::score::{Edit, Node, Score, Value};

/// How a flight moves, chosen when it begins: a later pass's look is not asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    #[default]
    Slide,
    Zoom,
    #[cfg(test)]
    Linear,
    /// Slide's choreography and staging but for the step, so a test can tell a chosen look from
    /// the default and a flight's staging from another look's.
    #[cfg(test)]
    Probe,
}

impl Look {
    #[cfg(test)]
    pub const ALL: [Look; 4] = [Look::Slide, Look::Zoom, Look::Linear, Look::Probe];

    /// The look's choreography for one flight.
    pub fn choreography(self) -> Box<dyn Choreography> {
        match self {
            Look::Slide => Box::new(Slide::default()),
            Look::Zoom => Box::new(ZoomArc::default()),
            #[cfg(test)]
            Look::Linear => Box::new(linear::Linear),
            #[cfg(test)]
            Look::Probe => Box::new(Slide::default()),
        }
    }

    /// How far apart, beyond the usable height, the look draws the strips above and below the one
    /// a flight moves, in its strip movements; `None` for a look that draws none.
    pub fn stack_gap(self) -> Option<f64> {
        match self {
            Look::Zoom => Some(zoom::STACK_GAP),
            Look::Slide => None,
            #[cfg(test)]
            Look::Linear | Look::Probe => None,
        }
    }

    /// What the look does with a press on `strip`: which windows start elsewhere, which are held,
    /// and the accent riding the movement.
    pub fn stage(self, cue: &Cue, strip: &Strip) -> Staging {
        match self {
            Look::Slide => slide::stage(cue, strip),
            Look::Zoom => zoom::stage(cue, strip),
            #[cfg(test)]
            Look::Linear => linear::stage(cue, strip),
            #[cfg(test)]
            Look::Probe => Staging {
                accent: None,
                ..slide::stage(cue, strip)
            },
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
    /// Focus went along the strip from `from` to `to`; at an end, both are the window it stayed on.
    Focus {
        from: WindowId,
        to: WindowId,
    },
}

impl Cue {
    pub fn moved(&self) -> Option<(WindowId, Direction)> {
        match *self {
            Cue::Move { window, direction } => Some((window, direction)),
            Cue::Plain | Cue::Focus { .. } => None,
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

/// The screen a strip is shown on, its outer gaps, and the display the overlay covers: what a zoom
/// of the strip is centred on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub frame: OverlayRect,
    pub outer_left: f64,
    pub outer_right: f64,
    pub viewport: OverlayRect,
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
    /// The display the overlay covers: what a zoom of the strip is centred on.
    pub viewport: OverlayRect,
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

/// Edits as they are made, each folded into a copy of the score playing, so the next reads what the
/// ones before it did.
struct Making {
    score: Score,
    edits: Vec<Edit>,
}

impl Making {
    fn from(playing: &Score) -> Self {
        Making {
            score: playing.clone(),
            edits: Vec::new(),
        }
    }

    fn push(&mut self, edit: Edit) {
        self.score.apply(&edit);
        self.edits.push(edit);
    }

    /// Where the container `key` ends, if the score has it.
    fn container(&self, key: GroupKey) -> Option<OverlayPoint> {
        match self.score.model(Node::Container(key)) {
            Some(Value::Point(position)) => Some(position),
            _ => None,
        }
    }

    /// The container `key`, opened at `at` unless the score has it.
    fn open(&mut self, key: GroupKey, at: OverlayPoint) {
        if self.container(key).is_none() {
            self.push(Edit::Place(Node::Container(key), Value::Point(at)));
        }
    }
}

/// A container's movement at a bend: from where it is, already placed where it ends.
#[derive(Clone, Copy, Debug)]
struct ContainerWay {
    key: GroupKey,
    from: OverlayPoint,
    to: OverlayPoint,
}

/// A loose tile's movement at a bend, in its container: from where it is, already placed where it
/// ends.
#[derive(Clone, Copy, Debug)]
struct TileWay {
    window: WindowId,
    from: GroupRect,
    to: GroupRect,
}

/// The motion a look gives a bend, node by node, in the order `Merged::bend` asks for it: the
/// tracks. What rides where, and where every node ends, is the bend's.
trait Bending {
    /// A container the merge retargets, from where the score has its movement.
    fn retarget(&mut self, making: &mut Making, way: ContainerWay);

    /// A container the merge opens, from where it installs; `leaving` is the container its member
    /// left, none for a newcomer's.
    fn open(&mut self, making: &mut Making, way: ContainerWay, leaving: Option<GroupKey>);

    /// A loose tile joining or bent, from where it starts or the score draws it.
    fn tile(&mut self, making: &mut Making, way: TileWay);

    /// The container `key` opens again or is dropped: any movement the look keeps of it goes.
    fn forget(&mut self, _key: GroupKey) {}
}

/// A loose tile placed where it ends, and moved there by `look`.
fn place_tile(making: &mut Making, look: &mut impl Bending, way: TileWay) {
    making.push(Edit::Place(Node::Tile(way.window), Value::Rect(way.to)));
    look.tile(making, way);
}

/// A pass merged into a flight that moves, at `now`: `merge_plans` over where the score draws every
/// container, its delta, and the edits made of it so far.
struct Merged<'a> {
    before: &'a FlightPlan,
    plan: FlightPlan,
    delta: PlanDelta,
    drawn: &'a [WindowId],
    now: f64,
    making: Making,
}

impl<'a> Merged<'a> {
    fn new(playing: &Score, plan: &'a FlightPlan, pass: &Pass<'a>, now: f64) -> Self {
        let (merged, delta) = merge_plans(
            plan,
            pass.plan,
            pass.pan,
            &playing.presented(now),
            pass.focus,
            pass.viewport,
        );
        Merged {
            before: plan,
            plan: merged,
            delta,
            drawn: pass.drawn,
            now,
            making: Making::from(playing),
        }
    }

    fn drawn(&self, window: WindowId) -> bool { self.drawn.contains(&window) }

    fn position(&self, key: GroupKey) -> Option<OverlayPoint> {
        self.plan.positions.get(&key).copied()
    }

    /// The delta as edits, in this order, `look` carrying each node placed where it ends. The
    /// containers a merge opens at where they install, a newcomer that opened one riding it at its
    /// frame in it; members moving container placed in their new one where they start; every
    /// retargeted container, carried on from where the score has its movement; every opened one,
    /// carried off from where it installs; tiles joining, each riding its container, a rigid one at
    /// its frame in it and a loose one carried off from where it starts; loose tiles carried on from
    /// where the score draws them, one `place_moved` just placed from there; and what the flight
    /// stops drawing dropped.
    fn bend(&mut self, look: &mut impl Bending) {
        self.open_groups(look);
        let placed = self.place_moved();
        for &(key, to) in &self.delta.retargeted_groups {
            let Some(Value::Point(from)) = self.making.score.along(Node::Container(key), self.now)
            else {
                continue;
            };
            let to = to.rounded();
            self.making.push(Edit::Place(Node::Container(key), Value::Point(to)));
            look.retarget(&mut self.making, ContainerWay { key, from, to });
        }
        for new in &self.delta.new_groups {
            let key = new.key;
            let to = self.plan.positions.get(&key).copied().unwrap_or(new.install).rounded();
            self.making.push(Edit::Place(Node::Container(key), Value::Point(to)));
            let way = ContainerWay { key, from: new.install, to };
            look.open(&mut self.making, way, new.leaving);
        }
        self.join_tiles(look);
        self.bend_tiles(look, &placed);
        self.drop_stale(look);
    }

    fn open_groups(&mut self, look: &mut impl Bending) {
        for new in &self.delta.new_groups {
            self.making.push(Edit::Place(
                Node::Container(new.key),
                Value::Point(new.install.rounded()),
            ));
            look.forget(new.key);
            let Some(group) = self.plan.groups.iter().find(|g| g.key == new.key) else {
                continue;
            };
            for member in &group.members {
                let moved_in = self
                    .delta
                    .reparented
                    .iter()
                    .any(|(w, _, to)| *w == member.window && *to == new.key);
                if moved_in || !self.drawn.contains(&member.window) {
                    continue;
                }
                self.making.push(Edit::Ride(member.window, new.key));
                self.making.push(Edit::Place(
                    Node::Tile(member.window),
                    Value::Rect(member.rel.rounded()),
                ));
            }
        }
    }

    /// A member moving container starts at its plan frame, which its tile sits a rounding from: it
    /// keeps the tile's, or it jumps by up to a point. Gives the windows it placed.
    fn place_moved(&mut self) -> Vec<WindowId> {
        let mut placed: Vec<WindowId> = Vec::new();
        for &(window, _, to_key) in &self.delta.reparented {
            let Some(frame) = self.plan.member(window).map(|member| member.start_frame()) else {
                continue;
            };
            let (Some(Member::Rigid { rel, .. }), Some(Value::Rect(drawn))) = (
                self.before.member(window),
                self.making.score.model(Node::Tile(window)),
            ) else {
                continue;
            };
            let frame = GroupRect::new(
                Point::new(
                    frame.origin.x + drawn.origin.x - rel.origin.x,
                    frame.origin.y + drawn.origin.y - rel.origin.y,
                ),
                drawn.size,
            );
            let at = self.plan.positions.get(&to_key).copied().unwrap_or(OverlayPoint::origin());
            self.making.open(to_key, at);
            self.making.push(Edit::Ride(window, to_key));
            self.making.push(Edit::Place(Node::Tile(window), Value::Rect(frame.rounded())));
            placed.push(window);
        }
        placed
    }

    fn join_tiles(&mut self, look: &mut impl Bending) {
        for &(window, key) in &self.delta.joined_tiles {
            if !self.drawn(window) {
                continue;
            }
            let Some(member) = self.plan.member(window) else {
                continue;
            };
            let at = self.position(key).unwrap_or(OverlayPoint::origin());
            self.making.open(key, at);
            self.making.push(Edit::Ride(window, key));
            match member {
                Member::Rigid { rel, .. } => {
                    self.making.push(Edit::Place(Node::Tile(window), Value::Rect(rel.rounded())))
                }
                Member::Changing { from, to }
                | Member::Entrance { from, to }
                | Member::Floating { from, to } => {
                    let way = TileWay {
                        window,
                        from: from.rounded(),
                        to: to.rounded(),
                    };
                    place_tile(&mut self.making, look, way);
                }
            }
        }
    }

    fn bend_tiles(&mut self, look: &mut impl Bending, placed: &[WindowId]) {
        for &(window, to) in &self.delta.retargeted_tiles {
            if !self.drawn(window) {
                continue;
            }
            let node = Node::Tile(window);
            let from = if placed.contains(&window) {
                self.making.score.model(node)
            } else {
                self.making.score.sample(node, self.now)
            };
            let Some(Value::Rect(from)) = from else { continue };
            place_tile(&mut self.making, look, TileWay {
                window,
                from,
                to: to.rounded(),
            });
        }
    }

    fn drop_stale(&mut self, look: &mut impl Bending) {
        let tiles: Vec<(WindowId, GroupKey)> = self.making.score.tiles().collect();
        let containers: Vec<GroupKey> = self.making.score.containers().collect();
        let (stale, empty) = stale_overlay_layers(&tiles, self.drawn, &containers);
        for window in stale {
            self.making.push(Edit::Drop(Node::Tile(window)));
        }
        for key in empty {
            self.making.push(Edit::Drop(Node::Container(key)));
            look.forget(key);
        }
    }

    fn retarget(self) -> Retarget {
        Retarget {
            plan: self.plan,
            delta: self.delta,
            edits: self.making.edits,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rini_geometry::space::{Rect, Respace, ScreenRect, ScreenVector};
    use rini_geometry::{from_overlay_space, is_off_screen, to_overlay_space};

    use super::*;
    use crate::animation::domain::motion::accents::Swap;
    use crate::animation::domain::motion::bounce::Bounce;
    use crate::animation::domain::motion::easing::{EASE_IN_OUT, MOTION_CURVE};
    use crate::animation::domain::motion::plan::{
        EDGE_BOUNCE_OVERSHOOT, GROUP_TOLERANCE, GroupKey, GroupMember, Member, Resolved,
        RigidGroup, carries_out_and_back, installed, on_overlay, overlay_of, reflow_plan,
    };
    use crate::animation::domain::motion::score::{Channel, Curve, SAMPLE_STEP, Track, Zoom};
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

    /// One flight of a look, every edit it answers folded into its score, and the strips it
    /// draws beside the one it moves, each window where it stands.
    struct Flight {
        choreography: Box<dyn Choreography>,
        plan: FlightPlan,
        drawn: Vec<WindowId>,
        score: Score,
        rows: Vec<Vec<(WindowId, OverlayRect)>>,
    }

    impl Flight {
        /// `requests` posed at frame zero by `choreography`, the windows `drawn` keeps drawn.
        fn posed(
            choreography: Box<dyn Choreography>,
            requests: &[Resolved],
            drawn: impl Fn(WindowId) -> bool,
        ) -> Flight {
            Flight::posed_beside(choreography, requests, Vec::new(), drawn)
        }

        /// `posed`, with `rows` drawn beside the strip, each a container of its own standing
        /// still, as `surface_plan` makes one.
        fn posed_beside(
            choreography: Box<dyn Choreography>,
            requests: &[Resolved],
            rows: Vec<Vec<(WindowId, OverlayRect)>>,
            drawn: impl Fn(WindowId) -> bool,
        ) -> Flight {
            let mut pass = reflow_plan(requests, DISPLAY);
            stand_beside(&mut pass, &rows);
            let plan = choreography.plan(pass, &Cue::Plain);
            let drawn: Vec<WindowId> = plan.windows().into_iter().filter(|w| drawn(*w)).collect();
            let score = choreography.pose(&stage(&plan, &drawn, &[]));
            Flight {
                choreography,
                plan,
                drawn,
                score,
                rows,
            }
        }

        /// Whether `window` is of a strip beside the one the flight moves.
        fn beside(&self, window: WindowId) -> bool {
            self.rows.iter().flatten().any(|(w, _)| *w == window)
        }

        fn fold(&mut self, edits: &[Edit]) {
            for edit in edits {
                self.score.apply(edit);
            }
        }

        /// The motion from frame zero at `BEGIN`, with `accents` waiting for it.
        fn start(&mut self, accents: &[Accent]) { self.start_as(accents, Cue::Plain); }

        /// The motion from frame zero at `BEGIN`, with `accents` waiting for it and frame zero
        /// showing `cue`.
        fn start_as(&mut self, accents: &[Accent], cue: Cue) {
            let stage = Stage {
                cue,
                ..stage(&self.plan, &self.drawn, accents)
            };
            let edits = self.choreography.compose(&self.score, &stage, BEGIN);
            self.fold(&edits);
        }

        /// `incoming` merged at `now`, with `accent` riding it, and what the look answered.
        fn merge(
            &mut self,
            incoming: &ReflowPlan,
            pan: Option<OverlayVector>,
            accent: Option<Accent>,
            now: f64,
        ) -> Retarget {
            let pass = Pass {
                plan: incoming,
                pan,
                focus: None,
                viewport: VIEWPORT,
                drawn: &self.drawn,
                seconds: PLAIN,
                cue: Cue::Plain,
                accent,
            };
            let retarget = self.choreography.retarget(&self.score, &self.plan, &pass, now);
            self.fold(&retarget.edits);
            self.plan = retarget.plan.clone();
            retarget
        }
    }

    /// `rows` put into `pass`, each a still container of its own, as `surface_plan` puts the
    /// strips beside the one a movement moves.
    fn stand_beside(pass: &mut ReflowPlan, rows: &[Vec<(WindowId, OverlayRect)>]) {
        for (index, row) in rows.iter().enumerate().filter(|(_, row)| !row.is_empty()) {
            let key = GroupKey::Rigid(pass.groups.len() as u16);
            pass.groups.push(RigidGroup {
                key,
                travel: OverlayVector::zero(),
                members: row
                    .iter()
                    .map(|&(window, frame)| GroupMember { window, rel: installed(frame) })
                    .collect(),
                beside: Some(index),
            });
        }
    }

    /// One or two strips beside the one a flight moves, above it or below, each of one or two
    /// windows a usable height and `zoom::STACK_GAP` away, numbered from `first`.
    fn random_rows(rng: &mut Gen, first: u32) -> Vec<Vec<(WindowId, OverlayRect)>> {
        let pitch = DISPLAY.size.height - 32.0 + zoom::STACK_GAP;
        let mut next = first;
        (0..1 + rng.below(2))
            .map(|_| {
                let dy = if rng.coin() { -pitch } else { pitch };
                (0..1 + rng.below(2))
                    .map(|_| {
                        let frame: OverlayRect = rng.on_screen();
                        next += 1;
                        (wid(next), frame.translate(OverlayVector::new(0.0, dy)))
                    })
                    .collect()
            })
            .collect()
    }

    /// A switch onto a strip beside `flight`'s as the reactor sends one under a look drawing them:
    /// a pan up or down by a pitch onto the only strip on that side, naming its windows the
    /// strip's and the strip's windows the strip it leaves in the stack, every window travelling
    /// with the view from where the flight sends it; and `flight.rows` as it leaves them. None
    /// when neither side holds exactly one strip a pitch away.
    fn a_switch(rng: &mut Gen, flight: &mut Flight) -> Option<(ReflowPlan, OverlayVector)> {
        let pitch = DISPLAY.size.height - 32.0 + zoom::STACK_GAP;
        let side = |row: &Vec<(WindowId, OverlayRect)>| {
            let (_, frame) = row.first()?;
            Some(((frame.center().y - DISPLAY.size.height / 2.0) / pitch).round() as i64)
        };
        let sides: Vec<Option<i64>> = flight.rows.iter().map(side).collect();
        let onto: Vec<usize> = (0..sides.len())
            .filter(|&i| {
                sides[i].is_some_and(|s| {
                    s.abs() == 1 && sides.iter().filter(|o| **o == Some(s)).count() == 1
                })
            })
            .collect();
        if onto.is_empty() {
            return None;
        }
        let row = onto[rng.below(onto.len() as u64) as usize];
        let d = OverlayVector::new(0.0, -(sides[row]? as f64) * pitch);
        let strip: Vec<(WindowId, OverlayRect)> = flight
            .plan
            .windows()
            .into_iter()
            .filter(|w| !flight.beside(*w))
            .filter_map(|w| Some((w, flight.plan.destination(w)?)))
            .collect();
        let arriving = std::mem::replace(&mut flight.rows[row], strip);
        let members = |windows: &[(WindowId, OverlayRect)]| -> Vec<GroupMember> {
            windows
                .iter()
                .map(|&(window, frame)| GroupMember { window, rel: installed(frame) })
                .collect()
        };
        let mut pass = ReflowPlan::empty();
        pass.groups.push(RigidGroup {
            key: GroupKey::Rigid(pass.groups.len() as u16),
            travel: d,
            members: members(&arriving),
            beside: None,
        });
        for (index, windows) in flight.rows.iter().enumerate().filter(|(_, w)| !w.is_empty()) {
            pass.groups.push(RigidGroup {
                key: GroupKey::Rigid(pass.groups.len() as u16),
                travel: d,
                members: members(windows),
                beside: Some(index),
            });
        }
        for (_, frame) in flight.rows.iter_mut().flatten() {
            *frame = frame.translate(d);
        }
        Some((pass, d))
    }

    fn stage<'a>(plan: &'a FlightPlan, drawn: &'a [WindowId], accents: &'a [Accent]) -> Stage<'a> {
        Stage {
            plan,
            drawn,
            seconds: PLAIN,
            cue: Cue::Plain,
            accents,
            viewport: VIEWPORT,
        }
    }

    /// Zoom alone draws the strips above and below the one a flight moves, 24pt beyond a usable
    /// height apart.
    #[test]
    fn only_zoom_draws_the_strips_beside() {
        for look in Look::ALL {
            let want = (look == Look::Zoom).then_some(24.0);
            assert_eq!(look.stack_gap(), want, "{look:?}");
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
    /// waited for it and whatever press frame zero shows, and once every track has ended nothing
    /// plays: each edge within a point, a half for its tile and a half for its container, each
    /// placed on whole points.
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
            let mut presses = Gen(32);
            for run in 0..RUNS {
                let mut flight =
                    Flight::posed(look.choreography(), &random_requests(&mut rng), |_| true);
                let mut accents = waiting[rng.below(waiting.len() as u64) as usize].clone();
                if presses.coin() {
                    accents.push(an_arc(&mut presses, &flight.drawn));
                }
                let cue = a_cue(&mut presses, &flight.drawn);
                flight.start_as(&accents, cue);
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

    /// One pass of the bend contracts' flights, merged at `now`: the flight playing as it lands and
    /// the plan it had, and the flight with it in.
    struct Bend<'a> {
        tag: String,
        now: f64,
        /// Whether the pass is a switch onto a strip beside (`a_switch`) or the pass after one.
        switched: bool,
        incoming: &'a ReflowPlan,
        newcomer: WindowId,
        playing: &'a Score,
        sent: &'a FlightPlan,
        delta: &'a PlanDelta,
        bent: &'a Flight,
    }

    /// Every pass of the bend contracts' flights of the look `make` gives, as `check` sees it: passes
    /// built as the reactor builds them, from where the last pass sends every window, merged one to
    /// four times 10-400ms apart into a flight playing from its score, 200 flights from each of
    /// `BEND_SEEDS`. Some flights start on an arc and some passes bring one (`an_arc`), and some
    /// draw strips beside the one they move (`random_rows`), which every pan names standing still,
    /// and are then switched onto one of them up to twice (`a_switch`), each switch followed by a
    /// pass.
    fn bent_flights(
        make: &dyn Fn() -> Box<dyn Choreography>,
        check: &mut dyn FnMut(&Bend) -> Result<(), String>,
    ) -> Result<(), String> {
        for seed in BEND_SEEDS {
            let mut rng = Gen(seed);
            let mut arcs = Gen(seed + 1000);
            let mut beside = Gen(seed + 2000);
            let mut switches = Gen(seed + 3000);
            for run in 0..RUNS {
                let requests = random_requests(&mut rng);
                let rows = match beside.coin() {
                    true => random_rows(&mut beside, 600),
                    false => Vec::new(),
                };
                let mut flight = Flight::posed_beside(make(), &requests, rows, |_| true);
                let first = arcs.coin().then(|| an_arc(&mut arcs, &flight.drawn));
                flight.start(&Vec::from_iter(first));
                let mut now = BEGIN;
                for step in 0..1 + rng.below(4) {
                    now += rng.pt(10.0, 400.0) / 1000.0;
                    let newcomer = wid(500 + step as u32);
                    let (incoming, pan) = random_pass(&mut rng, &mut flight, newcomer);
                    let (playing, sent) = (flight.score.clone(), flight.plan.clone());
                    let accent = (arcs.below(3) == 0).then(|| an_arc(&mut arcs, &flight.drawn));
                    let delta = flight.merge(&incoming, pan, accent, now).delta;
                    check(&Bend {
                        tag: format!("seed {seed} run {run} step {step}"),
                        now,
                        switched: false,
                        incoming: &incoming,
                        newcomer,
                        playing: &playing,
                        sent: &sent,
                        delta: &delta,
                        bent: &flight,
                    })?;
                }
                // After the seeded passes, so their streams stay as they were: a switch changes
                // which windows the strip has, and so what a pass draws next.
                for round in 0..switches.below(3) as u32 {
                    let Some((incoming, d)) = a_switch(&mut switches, &mut flight) else {
                        break;
                    };
                    let newcomer = wid(520 + round);
                    for (incoming, pan, switch) in
                        [(Some(incoming), Some(d), true), (None, None, false)]
                    {
                        let (incoming, pan) = match incoming {
                            Some(incoming) => (incoming, pan),
                            None => random_pass(&mut switches, &mut flight, newcomer),
                        };
                        now += switches.pt(10.0, 400.0) / 1000.0;
                        let (playing, sent) = (flight.score.clone(), flight.plan.clone());
                        let delta = flight.merge(&incoming, pan, None, now).delta;
                        let after = if switch { "" } else { " after" };
                        check(&Bend {
                            tag: format!("seed {seed} run {run} switch {round}{after}"),
                            now,
                            switched: true,
                            incoming: &incoming,
                            newcomer,
                            playing: &playing,
                            sent: &sent,
                            delta: &delta,
                            bent: &flight,
                        })?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The seeds of the bend contracts' flights: 163, and 2, which moves members of two containers
    /// into one a pass opens, a member from the other drawn 0.82pt from where it was under Slide,
    /// which starts the container's movement at the whole point nearest where it installs.
    const BEND_SEEDS: [u64; 2] = [163, 2];

    /// How far a window a pass moves into a container it opens may be drawn from where it was: Slide
    /// starts the container's movement on a whole point where its first member's container is drawn,
    /// and a member from another container sat at another fraction, a half point each. See "Whole
    /// points" in `src/animation/docs/animation-smoothness.md`.
    const OPENED_SLACK: f64 = 1.0;

    /// Whether the merge that left `delta` moved `window` into a container it opened.
    fn moved_into_an_opened(delta: &PlanDelta, window: WindowId) -> bool {
        delta.reparented.iter().any(|&(moved, _, to)| {
            moved == window && delta.new_groups.iter().any(|group| group.key == to)
        })
    }

    /// The bend contract, over `bent_flights`: whatever a pass brings and whenever it lands, every
    /// window the flight drew before it is drawn where it was, and a window it brings rides beside
    /// the windows it sits beside in the layout: drawn where the pass has it, moved by as far as
    /// the container it rides is drawn from where the last pass sent it, through the strip's zoom.
    /// Each edge within the half point placing on whole points allows, and within a point for a
    /// window the pass moves into a container it opens (`OPENED_SLACK`). No rigid member's tile has
    /// a track of its own: `Merged::bend` moves one to another container by riding and placing it
    /// alone. A strip drawn beside the one the flight moves is sent nowhere, by a pan or any pass;
    /// a switch onto one of them and the pass after it send every window each sends onto the
    /// viewport where it sends it, within `GROUP_TOLERANCE`, and no strip beside sideways.
    fn bends_without_a_jump(make: &dyn Fn() -> Box<dyn Choreography>) -> Result<(), String> {
        let (mut brought, mut besides, mut switched) = (0, 0, 0);
        bent_flights(make, &mut |bend| {
            if bend.switched {
                for window in bend.incoming.windows() {
                    let want =
                        sent_by(bend.incoming, window).filter(|w| !is_off_screen(VIEWPORT, *w));
                    let Some(want) = want else { continue };
                    let got = bend.bent.plan.destination(window);
                    if !got.is_some_and(|got| edges_apart(got, want) <= GROUP_TOLERANCE + 0.01) {
                        return Err(format!(
                            "{}: {window:?} sent to {got:?}, the pass sends it to {want:?}",
                            bend.tag
                        ));
                    }
                }
                switched += 1;
            }
            let (tag, now, newcomer, flight) = (&bend.tag, bend.now, bend.newcomer, bend.bent);
            if let Some(Member::Rigid { key, .. }) = flight.plan.member(newcomer)
                && let Some(start) = bend.sent.positions.get(&key)
            {
                let from =
                    bend.incoming.groups.iter().find_map(|g| {
                        g.members.iter().find(|m| m.window == newcomer).map(|m| m.rel)
                    });
                let from = from.expect("the pass brings it");
                let Some(Value::Point(container)) = flight.score.sample(Node::Container(key), now)
                else {
                    return Err(format!("{tag}: {key:?} is not drawn"));
                };
                let want = OverlayRect::new(
                    OverlayPoint::new(
                        from.origin.x + container.x - start.x,
                        from.origin.y + container.y - start.y,
                    ),
                    from.size.respace(),
                );
                let want = match flight.score.sample(Node::Strip, now) {
                    Some(Value::Zoom(zoom)) => {
                        OverlayRect::new(zoom.draw(want.origin), want.size * zoom.scale)
                    }
                    _ => want,
                };
                let is = flight.score.drawn(newcomer, now).expect("drawn");
                let apart = edges_apart(is, want);
                if apart > 0.5 + 1e-6 {
                    return Err(format!(
                        "{tag}: {newcomer:?} starts {apart} from beside the strip: {is:?}, wants {want:?}"
                    ));
                }
                brought += 1;
            }
            for window in bend.sent.windows() {
                let Some(was) = bend.playing.drawn(window, now) else {
                    continue;
                };
                let Some(is) = flight.score.drawn(window, now) else {
                    return Err(format!("{tag}: {window:?} gone"));
                };
                let apart = edges_apart(is, was);
                let slack = if moved_into_an_opened(bend.delta, window) {
                    OPENED_SLACK
                } else {
                    0.5
                };
                if apart > slack + 1e-6 {
                    return Err(format!("{tag}: {window:?} jumped {apart}: {was:?} then {is:?}"));
                }
            }
            for window in flight.plan.windows() {
                if matches!(flight.plan.member(window), Some(Member::Rigid { .. }))
                    && flight.score.track(Node::Tile(window), Channel::Base).is_some()
                {
                    return Err(format!("{tag}: rigid {window:?} has a track of its own"));
                }
            }
            for &(window, stands) in flight.rows.iter().flatten() {
                let sent = flight.plan.destination(window);
                if !sent.is_some_and(|sent| edges_apart(sent, stands) <= 0.5) {
                    return Err(format!(
                        "{tag}: {window:?} of a strip beside sent to {sent:?}, stands at {stands:?}"
                    ));
                }
                besides += 1;
            }
            Ok(())
        })?;
        if brought == 0 {
            return Err("no window brought beside the strip".to_string());
        }
        if besides == 0 {
            return Err("no flight drew a strip beside the one it moves".to_string());
        }
        if switched == 0 {
            return Err("no flight switched onto a strip beside".to_string());
        }
        Ok(())
    }

    /// How a speed changes across a bend at `now`, read over `SAMPLE_STEP`s: `was` as it was drawn
    /// and `is` as it is, each from 2 steps before to 2 after. The speed over the step before the
    /// bend and over the one after, and how far they may differ: as far as the motions themselves
    /// change it there, the motion before over the step before that and the step after, as it would
    /// have gone on, the one after over its second step, and `slack` over a step.
    struct Crossed {
        before: f64,
        after: f64,
        allowed: f64,
    }

    impl Crossed {
        fn read(
            was: impl Fn(f64) -> Option<f64>,
            is: impl Fn(f64) -> Option<f64>,
            slack: f64,
        ) -> Option<Crossed> {
            const STEP: f64 = SAMPLE_STEP;
            let (p2, p1, p0, o1) = (was(-2.0)?, was(-1.0)?, was(0.0)?, was(1.0)?);
            let (q0, q1, q2) = (is(0.0)?, is(1.0)?, is(2.0)?);
            let (earlier, before, unbent) = ((p1 - p2) / STEP, (p0 - p1) / STEP, (o1 - p0) / STEP);
            let (after, later) = ((q1 - q0) / STEP, (q2 - q1) / STEP);
            let allowed = (before - earlier).abs()
                + (unbent - before).abs()
                + (later - after).abs()
                + slack / STEP;
            Some(Crossed { before, after, allowed })
        }

        fn jumped(&self) -> bool { (self.after - self.before).abs() > self.allowed }
    }

    /// Where `score` lays `window` out at `at`: its frame in its container, moved by where the
    /// container is drawn, before the strip's zoom.
    fn laid(score: &Score, window: WindowId, at: f64) -> Option<OverlayRect> {
        let key = score.parent(window)?;
        match (
            score.sample(Node::Tile(window), at)?,
            score.sample(Node::Container(key), at)?,
        ) {
            (Value::Rect(frame), Value::Point(container)) => Some(overlay_of(frame, container)),
            _ => None,
        }
    }

    /// The speed contract, over `bent_flights`: every window the flight drew before a pass that the
    /// pass leaves riding a container goes on at the speed it is drawn moving. A window is drawn
    /// through the strip's zoom, so each is read apart: each edge's speed as the window is laid out
    /// on the strip, and the zoom's scale and shift, each `Crossed` the bend within what the
    /// motions change it, a laid-out edge with the half point a bend may move it as slack, and a
    /// strip the score holds no zoom for read at rest. Read drawn, a window's speed and the zoom's
    /// turning opposite ways cancel the allowance though neither jumps. A loose tile is outside it:
    /// Slide starts each one a pass bends from where it is drawn on `MOTION_CURVE` again, so its
    /// speed jumps, against "Arriving mid-flight" in `specs/animation.md`.
    fn bends_at_the_speed_it_moves(make: &dyn Fn() -> Box<dyn Choreography>) -> Result<(), String> {
        let edges =
            |frame: OverlayRect| [frame.min().x, frame.min().y, frame.max().x, frame.max().y];
        let zoom = |score: &Score, at: f64| match score.sample(Node::Strip, at) {
            Some(Value::Zoom(zoom)) => Value::Zoom(zoom).parts(),
            _ => Value::Zoom(Zoom::REST).parts(),
        };
        let mut moving = 0;
        bent_flights(make, &mut |bend| {
            let now = bend.now;
            let at = |k: f64| now + k * SAMPLE_STEP;
            for part in 0..3 {
                let was = |k: f64| Some(zoom(bend.playing, at(k))[part]);
                let is = |k: f64| Some(zoom(&bend.bent.score, at(k))[part]);
                if let Some(crossed) = Crossed::read(was, is, 1e-6).filter(Crossed::jumped) {
                    let Crossed { before, after, allowed } = crossed;
                    return Err(format!(
                        "{}: the zoom's speed jumped from {before:.3} to {after:.3}/s, allowed \
                         {allowed:.3}",
                        bend.tag
                    ));
                }
            }
            for window in bend.sent.windows() {
                if !matches!(bend.bent.plan.member(window), Some(Member::Rigid { .. })) {
                    continue;
                }
                for edge in 0..4 {
                    let was = |k: f64| laid(bend.playing, window, at(k)).map(|f| edges(f)[edge]);
                    let is = |k: f64| laid(&bend.bent.score, window, at(k)).map(|f| edges(f)[edge]);
                    let Some(crossed) = Crossed::read(was, is, 0.5) else {
                        continue;
                    };
                    if crossed.jumped() {
                        let Crossed { before, after, allowed } = crossed;
                        return Err(format!(
                            "{}: {window:?}'s speed jumped from {before:.0} to {after:.0}pt/s, \
                             allowed {allowed:.0}",
                            bend.tag
                        ));
                    }
                    moving += usize::from(crossed.before.abs() > 100.0);
                }
            }
            Ok(())
        })?;
        if moving == 0 {
            return Err("no window was moving as a pass landed".to_string());
        }
        Ok(())
    }

    /// Every look keeps the bend contract.
    #[test]
    fn every_look_bends_a_flight_without_a_jump() {
        for look in Look::ALL {
            if let Err(broken) = bends_without_a_jump(&|| look.choreography()) {
                panic!("{look:?}: {broken}");
            }
        }
    }

    /// The bend contract rejects a look that cuts every node the pass moves to where it ends, and
    /// one that gives a rigid member's tile a track of its own.
    #[test]
    fn the_bend_contract_rejects_a_look_that_cuts_to_the_pass_or_tracks_a_rigid_tile() {
        let cut = bends_without_a_jump(&|| Flawed::boxed(Flaw::CutsTheBend))
            .expect_err("a bend that cuts to the pass");
        assert!(cut.contains("jumped"), "{cut}");
        let tracked = bends_without_a_jump(&|| Flawed::boxed(Flaw::TracksRigidTiles))
            .expect_err("a rigid tile with a track of its own");
        assert!(tracked.contains("track of its own"), "{tracked}");
    }

    /// Every look keeps the speed contract.
    #[test]
    fn every_look_bends_a_flight_at_the_speed_it_moves() {
        for look in Look::ALL {
            if let Err(broken) = bends_at_the_speed_it_moves(&|| look.choreography()) {
                panic!("{look:?}: {broken}");
            }
        }
    }

    /// The speed contract rejects a look that starts every container a pass bends on a fresh curve
    /// from where it is drawn, one that sends the strip's zoom home from rest where it is drawn, and
    /// one that starts an unzoomed strip zooming at speed, each keeping the bend contract.
    #[test]
    fn the_speed_contract_rejects_a_look_that_restarts_the_motion_or_the_zoom_at_a_pass() {
        let restarted = bends_at_the_speed_it_moves(&|| Flawed::boxed(Flaw::RestartsTheBend))
            .expect_err("a bend that restarts the motion");
        assert!(restarted.contains("speed jumped"), "{restarted}");
        bends_without_a_jump(&|| Flawed::boxed(Flaw::RestartsTheBend)).expect("drawn where it was");
        let zoomed = bends_at_the_speed_it_moves(&|| Flawed::boxed(Flaw::RestartsTheZoom))
            .expect_err("a bend that restarts the zoom");
        assert!(zoomed.contains("the zoom's speed jumped"), "{zoomed}");
        bends_without_a_jump(&|| Flawed::boxed(Flaw::RestartsTheZoom)).expect("drawn where it was");
        let started = bends_at_the_speed_it_moves(&|| Flawed::boxed(Flaw::StartsTheZoomAtSpeed))
            .expect_err("a bend that starts a zoom at speed");
        assert!(started.contains("the zoom's speed jumped"), "{started}");
        bends_without_a_jump(&|| Flawed::boxed(Flaw::StartsTheZoomAtSpeed))
            .expect("drawn where it was");
    }

    /// A pass as the reactor builds one for `flight`, from where its last pass sends every window: a
    /// pan of every rigid window, naming the strips beside it where they stand, or a layout pass
    /// over some of the strip's windows, moving or resizing each; either at times bringing
    /// `newcomer`, which the flight then draws. The pan, for a pan.
    fn random_pass(
        rng: &mut Gen,
        flight: &mut Flight,
        newcomer: WindowId,
    ) -> (ReflowPlan, Option<OverlayVector>) {
        let windows: Vec<WindowId> =
            flight.plan.windows().into_iter().filter(|w| !flight.beside(*w)).collect();
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
                stand_beside(&mut p, &flight.rows);
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
    /// names every window it named and none twice; starts every window it leaves riding alike where
    /// the plan before started it; and sends every window the pass sends onto the viewport to the
    /// frame the pass sends it to, within `GROUP_TOLERANCE` (one sent off it may ride its container
    /// out). A window it moves to another container or kind of path may start anywhere, and the
    /// pass's own start frames bind no start. Frame zero draws every window the flight draws where
    /// the folded plan starts it, within the half point placing on whole points allows, which holds
    /// the pose to the fold; and the motion lands every window where the folded plan sends it. Some
    /// flights draw strips beside the one they move (`random_rows`), which every fold sends
    /// nowhere, and are switched onto one of them after a pass (`a_switch`).
    fn folds_each_pass_before_the_motion(
        make: &dyn Fn() -> Box<dyn Choreography>,
    ) -> Result<(), String> {
        let mut rng = Gen(167);
        let mut beside = Gen(1167);
        let mut switches = Gen(2167);
        let mut switched = 0;
        for run in 0..RUNS {
            let requests = random_requests(&mut rng);
            let rows = match beside.coin() {
                true => random_rows(&mut beside, 600),
                false => Vec::new(),
            };
            let mut flight = Flight::posed_beside(make(), &requests, rows, |_| true);
            for step in 0..1 + rng.below(3) {
                let tag = format!("seed 167 run {run} step {step}");
                let (incoming, pan) = random_pass(&mut rng, &mut flight, wid(500 + step as u32));
                folds_one(&mut flight, &incoming, pan, &tag)?;
            }
            // After the seeded passes, so their streams stay as they were.
            for round in 0..switches.below(3) as u32 {
                let Some((incoming, d)) = a_switch(&mut switches, &mut flight) else {
                    break;
                };
                switched += 1;
                let tag = format!("seed 167 run {run} switch {round}");
                folds_one(&mut flight, &incoming, Some(d), &tag)?;
                let (incoming, pan) = random_pass(&mut switches, &mut flight, wid(520 + round));
                folds_one(&mut flight, &incoming, pan, &format!("{tag} after"))?;
            }
            flight.start(&[]);
            lands_on_its_plan(&flight, BEGIN, &format!("seed 167 run {run}"))?;
        }
        if switched == 0 {
            return Err("no flight switched onto a strip beside".to_string());
        }
        Ok(())
    }

    /// One pass folded into `flight` before the motion, as `folds_each_pass_before_the_motion`
    /// holds it, frame zero posed again after it.
    fn folds_one(
        flight: &mut Flight,
        incoming: &ReflowPlan,
        pan: Option<OverlayVector>,
        tag: &str,
    ) -> Result<(), String> {
        let named = flight.plan.windows();
        let started: Vec<(WindowId, Member)> =
            named.iter().filter_map(|w| Some((*w, flight.plan.member(*w)?))).collect();
        let pass = Pass {
            plan: incoming,
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
        for &(window, was) in &started {
            let Some(is) = folded.member(window).filter(|is| rides_alike(was, *is)) else {
                continue;
            };
            let (was, is) = (on_overlay(was.start_frame()), on_overlay(is.start_frame()));
            if edges_apart(was, is) > 0.01 {
                return Err(format!(
                    "{tag}: {window:?} keeps where it rides and starts at {is:?}, it started at {was:?}"
                ));
            }
        }
        for window in incoming.windows() {
            let Some(want) = sent_by(incoming, window) else {
                continue;
            };
            if is_off_screen(VIEWPORT, want) {
                continue;
            }
            let got = folded.destination(window);
            let honoured = got.is_some_and(|got| edges_apart(got, want) <= GROUP_TOLERANCE + 0.01);
            if !honoured {
                return Err(format!(
                    "{tag}: {window:?} sent to {got:?}, the pass sends it to {want:?}"
                ));
            }
        }
        for &(window, stands) in flight.rows.iter().flatten() {
            let sent = folded.destination(window);
            if !sent.is_some_and(|sent| edges_apart(sent, stands) <= 0.01) {
                return Err(format!(
                    "{tag}: {window:?} of a strip beside sent to {sent:?}, stands at {stands:?}"
                ));
            }
        }
        flight.plan = folded;
        flight.score = flight.choreography.pose(&stage(&flight.plan, &flight.drawn, &[]));
        for &window in &flight.drawn {
            let starts = flight.plan.member(window).map(|m| on_overlay(m.start_frame()));
            let drawn = flight.score.drawn(window, BEGIN);
            let posed = matches!((drawn, starts), (Some(d), Some(s)) if edges_apart(d, s) <= 0.5);
            if !posed {
                return Err(format!(
                    "{tag}: frame zero draws {window:?} at {drawn:?}, it starts at {starts:?}"
                ));
            }
        }
        Ok(())
    }

    /// Whether two members ride alike: one container, or one kind of path of their own.
    fn rides_alike(a: Member, b: Member) -> bool {
        match (a, b) {
            (Member::Rigid { key: a, .. }, Member::Rigid { key: b, .. }) => a == b,
            (a, b) => std::mem::discriminant(&a) == std::mem::discriminant(&b),
        }
    }

    /// The push contract, over flights of the look `make` gives: one to three pushes against an end
    /// reaching a flight that moves, each 10-300ms after the one before, each with a press of its
    /// own (`a_cue`), the flight at times started on an arc or by a push of its own. No window the
    /// flight draws jumps as a push reaches it, within the half point placing on whole points
    /// allows; against each of the four walls, some push draws a window more than a point further
    /// toward it than the flight without it draws it; and every window is home where its plan sends
    /// it once every track has ended, which is no later than one bounce after the last push or the
    /// end of the motion.
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
        let mut presses = Gen(174);
        let mut toward = [0usize; 4];
        for run in 0..RUNS {
            let mut flight = Flight::posed(make(), &random_requests(&mut rng), |_| true);
            let first = match presses.below(3) {
                0 => vec![an_arc(&mut presses, &flight.drawn)],
                1 => vec![Accent::Push {
                    wall: walls[presses.below(2) as usize],
                    seconds: PLAIN,
                }],
                _ => Vec::new(),
            };
            let cue = a_cue(&mut presses, &flight.drawn);
            flight.start_as(&first, cue);
            let motion_end = BEGIN + flight.score.remaining(BEGIN);
            let mut now = BEGIN;
            for step in 0..1 + rng.below(3) {
                let tag = format!("seed 173 run {run} step {step}");
                now += rng.pt(10.0, 300.0) / 1000.0;
                let side = rng.below(4) as usize;
                let wall = walls[side];
                let without = flight.score.clone();
                let cue = a_cue(&mut presses, &flight.drawn);
                let edits = flight.choreography.push(&flight.score, &cue, wall, PLAIN, now);
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
                toward[side] += usize::from(pushed);
            }
            lands_on_its_plan(&flight, now, &format!("seed 173 run {run}"))?;
            let (lands, due) = (now + flight.score.remaining(now), motion_end.max(now + PLAIN));
            if lands > due + 1e-6 {
                return Err(format!(
                    "seed 173 run {run}: home at {lands:.3}s, due by {due:.3}s, one bounce after the last push or the motion's end"
                ));
            }
        }
        if let Some(side) = toward.iter().position(|pushed| *pushed == 0) {
            let wall = walls[side];
            return Err(format!(
                "no push against {wall:?} in {RUNS} runs drew a window toward its wall"
            ));
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

    /// The fold contract rejects a look whose fold keeps the plan it is given, one that moves each
    /// window where the pass sends it but keeps every resize at the size it starts at, and one that
    /// starts every resize and entrance where it ends.
    #[test]
    fn the_fold_contract_rejects_a_look_that_ignores_the_pass_drops_resizes_or_moves_a_start() {
        let broken = folds_each_pass_before_the_motion(&|| Flawed::boxed(Flaw::IgnoresThePass))
            .expect_err("a fold that ignores the pass");
        assert!(broken.contains("the pass sends it to"), "{broken}");
        let unresized = folds_each_pass_before_the_motion(&|| Flawed::boxed(Flaw::DropsResizes))
            .expect_err("a fold that drops every resize");
        assert!(unresized.contains("the pass sends it to"), "{unresized}");
        let finished = folds_each_pass_before_the_motion(&|| Flawed::boxed(Flaw::StartsAtTheEnd))
            .expect_err("a fold that starts every resize and entrance where it ends");
        assert!(finished.contains("it started at"), "{finished}");
    }

    /// The push contract rejects a look that plays no push, one that pushes only against the left
    /// and right walls, one that starts every bounce from rest wherever the one before is drawn, and
    /// one whose bounce plays ten times as long as it is given.
    #[test]
    fn the_push_contract_rejects_a_look_that_skips_a_wall_pushes_from_rest_or_lands_late() {
        let still = pushes_out_and_home_without_a_jump(&|| Flawed::boxed(Flaw::PlaysNoPush))
            .expect_err("a push that plays nothing");
        assert!(still.contains("toward its wall"), "{still}");
        let level = pushes_out_and_home_without_a_jump(&|| Flawed::boxed(Flaw::PushesSideways))
            .expect_err("a push only against the left and right walls");
        assert!(
            level.contains("(0.0, -72.0)") || level.contains("(0.0, 72.0)"),
            "{level}"
        );
        let from_rest = pushes_out_and_home_without_a_jump(&|| Flawed::boxed(Flaw::PushesFromRest))
            .expect_err("a push from rest over a bounce");
        assert!(from_rest.contains("jumped"), "{from_rest}");
        let slow = pushes_out_and_home_without_a_jump(&|| Flawed::boxed(Flaw::PushesSlowly))
            .expect_err("a push that bounces ten times as long");
        assert!(slow.contains("after the last push"), "{slow}");
    }

    /// The digest of what Slide draws over seed 163's flights, posed, composed with the accents
    /// seed 181 has wait for the motion and bent by one to four passes as `random_pass` builds them,
    /// half of them bringing a step: the score after the motion starts and after each pass, and each
    /// pass's edits. A change to how Slide moves changes it; one that only moves code between Slide
    /// and what every look shares keeps it.
    const SLIDE_SCORES: u64 = 0xf63d_137b_00ac_6966;

    /// Slide draws over seed 163's flights what `SLIDE_SCORES` holds: every node's place, ride and
    /// tracks, and every edit a pass answers; some flights start stepping or pushed, and some passes
    /// take a step while none plays. Each starts playing nothing or for at least its plain length,
    /// so the clock fitted to it (`Event::Fit`) is a slide's as it was.
    #[test]
    fn slide_draws_every_seeded_flight_as_it_did() {
        let mut rng = Gen(163);
        let mut accents = Gen(181);
        let mut digest = Digest::default();
        let (mut started, mut stepped) = (0, 0);
        for _ in 0..RUNS {
            let mut flight =
                Flight::posed(Look::Slide.choreography(), &random_requests(&mut rng), |_| true);
            let waiting = waiting_accents(&mut accents);
            flight.start(&waiting);
            started += usize::from(flight.score.plays(Channel::Nudge, BEGIN));
            let plays = flight.score.remaining(BEGIN);
            assert!(
                plays == 0.0 || plays >= PLAIN - 1e-9,
                "the clock keeps a slide's length: {plays}"
            );
            digest.score(&flight.score);
            let mut now = BEGIN;
            for step in 0..1 + rng.below(4) {
                now += rng.pt(10.0, 400.0) / 1000.0;
                let (incoming, pan) = random_pass(&mut rng, &mut flight, wid(500 + step as u32));
                let accent = accents.coin().then(|| a_step(&mut accents));
                let edits = flight.merge(&incoming, pan, accent, now).edits;
                stepped += usize::from(edits.iter().any(
                    |edit| matches!(edit, Edit::Add(track) if track.channel() == Channel::Nudge),
                ));
                digest.edits(&edits);
                digest.score(&flight.score);
            }
        }
        assert!(
            started > 0 && stepped > 0,
            "{started} started, {stepped} stepped"
        );
        assert_eq!(digest.0, SLIDE_SCORES, "{:#x}", digest.0);
    }

    /// An arc a zoom look stages, at the plain length: 0.7 to 0.88 deep, a quarter to three quarters
    /// of the travel carried at its middle, and half the time two of `windows` swapped, each
    /// crossing up to 900pt.
    fn an_arc(rng: &mut Gen, windows: &[WindowId]) -> Accent {
        let depth = 0.7 + rng.below(19) as f64 / 100.0;
        let share = 0.25 + rng.below(51) as f64 / 100.0;
        let swap = (windows.len() >= 2 && rng.coin()).then(|| {
            let moved = rng.below(windows.len() as u64) as usize;
            let passed = (moved + 1 + rng.below(windows.len() as u64 - 1) as usize) % windows.len();
            let by = rng.pt(100.0, 900.0);
            Swap {
                moved: windows[moved],
                passed: windows[passed],
                moved_by: OverlayVector::new(by, 0.0),
                passed_by: OverlayVector::new(-by, 0.0),
            }
        });
        Accent::Arc {
            depth,
            share,
            swap,
            seconds: PLAIN,
        }
    }

    /// The press a flight or a push shows: plain, focus along the strip between two of `windows` or
    /// staying on one at an end, or a move of one of them.
    fn a_cue(rng: &mut Gen, windows: &[WindowId]) -> Cue {
        let kind = rng.below(3);
        let mut any = || windows.get(rng.below(windows.len().max(1) as u64) as usize).copied();
        match (kind, any(), any()) {
            (1, Some(from), Some(to)) => Cue::Focus { from, to },
            (2, Some(window), _) => Cue::Move {
                window,
                direction: Direction::Left,
            },
            _ => Cue::Plain,
        }
    }

    /// A move's step left or right, of a third of the viewport, at the plain length.
    fn a_step(rng: &mut Gen) -> Accent {
        let offset = if rng.coin() { 576.0 } else { -576.0 };
        Accent::Step {
            offset: OverlayVector::new(offset, 0.0),
            seconds: PLAIN,
        }
    }

    /// What waits for the motion: nothing, a step, a push against a side wall, or both.
    fn waiting_accents(rng: &mut Gen) -> Vec<Accent> {
        let wall = if rng.coin() {
            EDGE_BOUNCE_OVERSHOOT
        } else {
            -EDGE_BOUNCE_OVERSHOOT
        };
        let push = Accent::Push {
            wall: OverlayVector::new(wall, 0.0),
            seconds: PLAIN,
        };
        match rng.below(4) {
            0 => Vec::new(),
            1 => vec![a_step(rng)],
            2 => vec![push],
            _ => vec![a_step(rng), push],
        }
    }

    /// The digest of what Zoom draws over seed 163's flights, each started on an arc seed 191 stages
    /// with a press of its own, some with a push waiting, and bent by one to four passes as
    /// `random_pass` builds them, some bringing an arc and some reached by a push first: the score
    /// after the motion starts and after each pass or push, and each one's edits. A change to how
    /// Zoom moves changes it.
    const ZOOM_SCORES: u64 = 0x7e3f_2c8e_1bc2_f480;

    /// Zoom draws over seed 163's flights what `ZOOM_SCORES` holds: the strip's zoom and every
    /// node's place, ride and tracks, and every edit a pass or a push answers; some passes fold into
    /// the arc playing and some start a shallow one.
    #[test]
    fn zoom_draws_every_seeded_flight_as_it_did() {
        let mut rng = Gen(163);
        let mut presses = Gen(191);
        let mut digest = Digest::default();
        let (mut during, mut started) = (0, 0);
        for _ in 0..RUNS {
            let mut flight =
                Flight::posed(Look::Zoom.choreography(), &random_requests(&mut rng), |_| true);
            let mut waiting = vec![an_arc(&mut presses, &flight.drawn)];
            if presses.below(3) == 0 {
                waiting.push(Accent::Push {
                    wall: OverlayVector::new(-EDGE_BOUNCE_OVERSHOOT, 0.0),
                    seconds: PLAIN,
                });
            }
            let cue = a_cue(&mut presses, &flight.drawn);
            flight.start_as(&waiting, cue);
            digest.score(&flight.score);
            let mut now = BEGIN;
            for step in 0..1 + rng.below(4) {
                now += rng.pt(10.0, 400.0) / 1000.0;
                let (incoming, pan) = random_pass(&mut rng, &mut flight, wid(500 + step as u32));
                if presses.below(4) == 0 {
                    let wall = OverlayVector::new(EDGE_BOUNCE_OVERSHOOT, 0.0);
                    let edits = flight.choreography.push(&flight.score, &cue, wall, PLAIN, now);
                    flight.fold(&edits);
                    digest.edits(&edits);
                }
                let arcing = flight.score.track(Node::Strip, Channel::Base).cloned();
                during += usize::from(arcing.as_ref().is_some_and(|t| t.at(now).is_some()));
                let accent = presses.coin().then(|| an_arc(&mut presses, &flight.drawn));
                let edits = flight.merge(&incoming, pan, accent, now).edits;
                let fresh = flight.score.track(Node::Strip, Channel::Base);
                started += usize::from(fresh.is_some_and(|t| t.begin() == now));
                digest.edits(&edits);
                digest.score(&flight.score);
            }
        }
        assert!(
            during > 0 && started > 0,
            "{during} during an arc, {started} started one"
        );
        assert_eq!(digest.0, ZOOM_SCORES, "{:#x}", digest.0);
    }

    /// FNV-1a over every score and edit list folded in, each read in one order: a score's maps and
    /// the merge's keep none. The strip is read only once it is placed.
    struct Digest(u64);

    impl Default for Digest {
        fn default() -> Self { Digest(0xcbf2_9ce4_8422_2325) }
    }

    impl Digest {
        fn score(&mut self, score: &Score) {
            let node = |node: Node| {
                let tracks: Vec<_> = score.tracks(node).collect();
                format!("{node:?} {:?} {tracks:?}", score.model(node))
            };
            let mut lines: Vec<String> =
                score.containers().map(|key| node(Node::Container(key))).collect();
            lines.extend(
                score
                    .tiles()
                    .map(|(window, key)| format!("{key:?} {}", node(Node::Tile(window)))),
            );
            if score.model(Node::Strip).is_some() {
                lines.push(node(Node::Strip));
            }
            lines.sort();
            self.bytes(lines.concat().bytes());
        }

        fn edits(&mut self, edits: &[Edit]) {
            let mut lines: Vec<String> = edits.iter().map(|edit| format!("{edit:?}")).collect();
            lines.sort();
            self.bytes(lines.concat().bytes());
        }

        fn bytes(&mut self, bytes: impl IntoIterator<Item = u8>) {
            for byte in bytes {
                self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }

    /// The one contract a `Flawed` look breaks.
    #[derive(Clone, Copy, Debug)]
    enum Flaw {
        /// Its fold gives back the plan it is given.
        IgnoresThePass,
        /// Its fold keeps every resize at the size it starts at.
        DropsResizes,
        /// Its fold starts every resize and entrance where it ends.
        StartsAtTheEnd,
        /// Its push plays nothing.
        PlaysNoPush,
        /// Its push plays nothing against the top or bottom wall.
        PushesSideways,
        /// Its push starts every carrier's bounce from rest, wherever the one before is drawn.
        PushesFromRest,
        /// Its push plays a bounce ten times as long as it is given.
        PushesSlowly,
        /// Its bend moves nothing: every node it would carry is cut to where it ends.
        CutsTheBend,
        /// Its bend starts every container it carries on `MOTION_CURVE` from where it is, at the
        /// curve's speed, whatever speed it had.
        RestartsTheBend,
        /// Its motion holds every rigid member's tile at its frame on a track of its own.
        TracksRigidTiles,
        /// Its motion zooms the strip out and back, and its bend sends the zoom home from where it
        /// is drawn on `EASE_IN_OUT`, from rest.
        RestartsTheZoom,
        /// Its motion leaves the strip unzoomed, and its bend zooms it out and back from rest at
        /// speed, on a straight line each way.
        StartsTheZoomAtSpeed,
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
                Flaw::DropsResizes => {
                    let mut folded = self.slide.fold(plan, pass);
                    for path in &mut folded.changing {
                        path.to.size = path.from.size;
                    }
                    folded
                }
                Flaw::StartsAtTheEnd => {
                    let mut folded = self.slide.fold(plan, pass);
                    for path in folded.changing.iter_mut().chain(&mut folded.entrances) {
                        path.from = path.to;
                    }
                    folded
                }
                Flaw::PlaysNoPush
                | Flaw::PushesSideways
                | Flaw::PushesFromRest
                | Flaw::PushesSlowly
                | Flaw::CutsTheBend
                | Flaw::RestartsTheBend
                | Flaw::TracksRigidTiles
                | Flaw::RestartsTheZoom
                | Flaw::StartsTheZoomAtSpeed => self.slide.fold(plan, pass),
            }
        }

        fn pose(&self, stage: &Stage) -> Score { self.slide.pose(stage) }

        fn compose(&mut self, playing: &Score, stage: &Stage, begin: f64) -> Vec<Edit> {
            let mut edits = self.slide.compose(playing, stage, begin);
            if let Flaw::RestartsTheZoom = self.flaw {
                let mut making = Making::from(playing);
                for edit in edits {
                    making.push(edit);
                }
                let centre = stage.viewport.center();
                zoom::strip_arc(&mut making, zoom::STRIP_ZOOM, centre, begin, stage.seconds);
                return making.edits;
            }
            if let Flaw::TracksRigidTiles = self.flaw {
                let members = stage.plan.groups.iter().flat_map(|group| &group.members);
                for member in members.filter(|m| stage.drawn.contains(&m.window)) {
                    let frame = Value::Rect(member.rel.rounded());
                    edits.push(Edit::Add(Track::between(
                        Node::Tile(member.window),
                        Channel::Base,
                        frame,
                        frame,
                        begin,
                        stage.seconds,
                        Curve::Bezier(MOTION_CURVE),
                    )));
                }
            }
            edits
        }

        fn retarget(
            &mut self,
            playing: &Score,
            plan: &FlightPlan,
            pass: &Pass,
            now: f64,
        ) -> Retarget {
            let mut bent = self.slide.retarget(playing, plan, pass, now);
            if let (Flaw::RestartsTheZoom, Some(drawn)) =
                (self.flaw, playing.sample(Node::Strip, now))
            {
                bent.edits.push(Edit::Add(Track::between(
                    Node::Strip,
                    Channel::Base,
                    drawn,
                    Value::Zoom(Zoom::REST),
                    now,
                    pass.seconds,
                    Curve::Bezier(EASE_IN_OUT),
                )));
            }
            if let (Flaw::StartsTheZoomAtSpeed, None) =
                (self.flaw, playing.sample(Node::Strip, now))
            {
                let out = Zoom {
                    scale: zoom::STRIP_ZOOM,
                    shift: OverlayVector::zero(),
                };
                bent.edits.push(Edit::Place(Node::Strip, Value::Zoom(Zoom::REST)));
                bent.edits.push(Edit::Add(Track::samples(
                    Node::Strip,
                    Channel::Base,
                    [Zoom::REST, out, Zoom::REST].map(Value::Zoom),
                    now,
                    pass.seconds,
                )));
            }
            for edit in &mut bent.edits {
                let Edit::Add(track) = edit else { continue };
                if track.channel() != Channel::Base {
                    continue;
                }
                match (self.flaw, track.node(), track.keys()) {
                    (Flaw::CutsTheBend, node, _) => *edit = Edit::Cut(node, Channel::Base),
                    (
                        Flaw::RestartsTheBend,
                        node @ Node::Container(_),
                        [(_, from), .., (_, to)],
                    ) => {
                        *edit = Edit::Add(Track::between(
                            node,
                            Channel::Base,
                            *from,
                            *to,
                            now,
                            pass.seconds,
                            Curve::Bezier(MOTION_CURVE),
                        ))
                    }
                    _ => {}
                }
            }
            bent
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
                Flaw::PlaysNoPush => Vec::new(),
                Flaw::PushesSideways if wall.y != 0.0 => Vec::new(),
                Flaw::IgnoresThePass
                | Flaw::DropsResizes
                | Flaw::StartsAtTheEnd
                | Flaw::PushesSideways
                | Flaw::CutsTheBend
                | Flaw::RestartsTheBend
                | Flaw::TracksRigidTiles
                | Flaw::RestartsTheZoom
                | Flaw::StartsTheZoomAtSpeed => self.slide.push(playing, cue, wall, seconds, now),
                Flaw::PushesSlowly => self.slide.push(playing, cue, wall, seconds * 10.0, now),
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
            viewport: VIEWPORT,
        };
        for look in Look::ALL {
            let mut rng = Gen(37);
            let mut focus = Gen(38);
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
                    0 if focus.coin() => Cue::Focus {
                        from: wid(1 + focus.below(count as u64) as u32),
                        to: wid(1 + focus.below(count as u64) as u32),
                    },
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
