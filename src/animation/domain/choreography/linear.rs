//! Linear, a look only the tests build: every container and loose tile goes straight to where its
//! pass sends it at an even speed over the flight's length, and a pass arriving mid-flight carries
//! each on from where the score has it at the speed it is moving there, that speed changing evenly
//! until it arrives over the pass's length; no accent but the push, and a move holds the column it
//! passes. It plans, folds and poses as Slide does and bends through `Merged::bend`, so it keeps
//! every look's contract on motion of its own.

use rini_geometry::Round;
use rini_geometry::space::OverlayVector;

use super::slide::Slide;
use super::{
    Bending, Choreography, ContainerWay, Cue, Making, Merged, Pass, Retarget, Stage, Staging,
    Strip, TileWay,
};
use crate::animation::domain::motion::accents::push_tracks;
use crate::animation::domain::motion::plan::{
    AnimationTarget, FlightPlan, GroupKey, ReflowPlan, animation_targets, group_relative,
    on_overlay,
};
use crate::animation::domain::motion::score::{
    Channel, Curve, Edit, Node, SAMPLE_STEP, Score, Track, Value,
};

#[derive(Debug)]
pub struct Linear;

/// What Linear does with a press on the strip: for a move, the column passed is held at its start
/// on the strip and the moved one crosses from where it starts, with no accent; a plain press stages
/// nothing.
pub fn stage(cue: &Cue, strip: &Strip) -> Staging {
    let mut staging = Staging::default();
    if cue.moved().is_none() {
        return staging;
    }
    for pair in strip.pair {
        let start = (pair.window, pair.start);
        if pair.passed {
            staging.held.push(start);
        } else {
            staging.from.push(start);
        }
    }
    staging
}

/// The clock every movement of one composition or bend runs on, and for a bend the score playing
/// as it lands, whose speeds it carries on.
struct Even<'a> {
    begin: f64,
    seconds: f64,
    bent: Option<&'a Score>,
}

impl Even<'_> {
    /// `node` carried from `from` to where it is placed, starting at `speed`: from rest on a linear
    /// track, or with its speed changing evenly on one sampled every `SAMPLE_STEP`; nothing for no
    /// length or no movement.
    fn carry(&self, making: &mut Making, node: Node, from: Value, to: Value, speed: [f64; 4]) {
        let still = from == to && speed == [0.0; 4];
        if self.seconds <= 0.0 || still {
            return;
        }
        let track = match self.bent {
            None => Track::between(
                node,
                Channel::Base,
                from,
                to,
                self.begin,
                self.seconds,
                Curve::Linear,
            ),
            Some(_) => {
                let (a, b, length) = (from.parts(), to.parts(), self.seconds);
                let steps = (length / SAMPLE_STEP).ceil().max(1.0) as usize;
                let values = (0..=steps).map(|k| {
                    let t = length * k as f64 / steps as f64;
                    let at = |i: usize| {
                        let pull = b[i] - a[i] - speed[i] * length;
                        a[i] + speed[i] * t + pull * (t / length).powi(2)
                    };
                    from.with_parts([at(0), at(1), at(2), at(3)])
                });
                Track::samples(node, Channel::Base, values, self.begin, length)
            }
        };
        making.push(Edit::Add(track));
    }

    /// How fast `node`'s movement is going as the bend lands (`Score::rate`): still in a
    /// composition.
    fn speed(&self, node: Node) -> [f64; 4] {
        self.bent.map_or([0.0; 4], |score| score.rate(node, self.begin))
    }
}

impl Bending for Even<'_> {
    fn retarget(&mut self, making: &mut Making, way: ContainerWay) {
        let node = Node::Container(way.key);
        let speed = self.speed(node);
        self.carry(making, node, Value::Point(way.from), Value::Point(way.to), speed);
    }

    fn open(&mut self, making: &mut Making, way: ContainerWay, leaving: Option<GroupKey>) {
        let node = Node::Container(way.key);
        let speed = leaving.map_or([0.0; 4], |left| self.speed(Node::Container(left)));
        self.carry(making, node, Value::Point(way.from), Value::Point(way.to), speed);
    }

    fn tile(&mut self, making: &mut Making, way: TileWay) {
        let node = Node::Tile(way.window);
        let speed = self.speed(node);
        self.carry(making, node, Value::Rect(way.from), Value::Rect(way.to), speed);
    }
}

impl Choreography for Linear {
    fn plan(&self, pass: ReflowPlan, cue: &Cue) -> FlightPlan { Slide::default().plan(pass, cue) }

    fn fold(&self, plan: &FlightPlan, pass: &Pass) -> FlightPlan {
        Slide::default().fold(plan, pass)
    }

    fn pose(&self, stage: &Stage) -> Score { Slide::default().pose(stage) }

    /// Exactly what `animation_targets` names, each node placed where it ends and carried there.
    fn compose(&mut self, playing: &Score, stage: &Stage, begin: f64) -> Vec<Edit> {
        let even = Even {
            begin,
            seconds: stage.seconds,
            bent: None,
        };
        let mut making = Making::from(playing);
        for target in animation_targets(stage.plan) {
            match target {
                AnimationTarget::Container { key, from, to } => {
                    if making.container(key).is_none() {
                        continue;
                    }
                    let (from, to) = (Value::Point(from.rounded()), Value::Point(to.rounded()));
                    making.push(Edit::Place(Node::Container(key), to));
                    even.carry(&mut making, Node::Container(key), from, to, [0.0; 4]);
                }
                AnimationTarget::Tile { window, from, to } => {
                    let Some(offset) =
                        making.score.parent(window).and_then(|key| making.container(key))
                    else {
                        continue;
                    };
                    let at =
                        |frame| Value::Rect(group_relative(on_overlay(frame), offset).rounded());
                    making.push(Edit::Place(Node::Tile(window), at(to)));
                    even.carry(&mut making, Node::Tile(window), at(from), at(to), [0.0; 4]);
                }
            }
        }
        making.edits
    }

    fn retarget(&mut self, playing: &Score, plan: &FlightPlan, pass: &Pass, now: f64) -> Retarget {
        let mut merged = Merged::new(playing, plan, pass, now);
        merged.bend(&mut Even {
            begin: now,
            seconds: pass.seconds,
            bent: Some(playing),
        });
        merged.retarget()
    }

    fn push(
        &mut self,
        playing: &Score,
        _cue: &Cue,
        wall: OverlayVector,
        seconds: f64,
        now: f64,
    ) -> Vec<Edit> {
        push_tracks(playing, wall, seconds, now).into_iter().map(Edit::Add).collect()
    }
}
