//! Everything that moves in one transition, as data: tracks of keyframes on containers and tiles,
//! where each node ends, which container each tile rides and how deep everything is drawn. A score is
//! the fold of its edits (`Edit`), and evaluates at any media time to what Core Animation draws, so a
//! retarget starts from the score instead of the presentation layer. What Core Animation was
//! measured to play is "The score" in `src/animation/docs/animation-smoothness.md`.
use std::collections::HashMap;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::WindowId;

use super::easing::CubicBezier;
use super::plan::{Banding, GroupKey, Presented};

/// How far apart a spring leg's and a nudge's samples are: one 120Hz frame.
pub const SAMPLE_STEP: f64 = 1.0 / 120.0;

/// What a track moves: a container's position, or a tile's frame inside its container.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Node {
    Container(GroupKey),
    Tile(WindowId),
}

impl Node {
    fn holds(self, value: Value) -> bool {
        matches!(
            (self, value),
            (Node::Container(_), Value::Point(_)) | (Node::Tile(_), Value::Rect(_))
        )
    }
}

/// A container holds a `Point` and a tile a `Rect`, on every channel.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Value {
    Point(CGPoint),
    Rect(CGRect),
}

impl Value {
    fn zip(self, other: Value, f: impl Fn(f64, f64) -> f64) -> Value {
        match (self, other) {
            (Value::Point(a), Value::Point(b)) => {
                Value::Point(CGPoint::new(f(a.x, b.x), f(a.y, b.y)))
            }
            (Value::Rect(a), Value::Rect(b)) => Value::Rect(CGRect::new(
                CGPoint::new(f(a.origin.x, b.origin.x), f(a.origin.y, b.origin.y)),
                CGSize::new(f(a.size.width, b.size.width), f(a.size.height, b.size.height)),
            )),
            _ => unreachable!("a node holds one kind of value"),
        }
    }

    fn lerp(self, to: Value, progress: f64) -> Value {
        self.zip(to, |a, b| a + (b - a) * progress)
    }

    fn plus(self, offset: Value) -> Value {
        self.zip(offset, |a, b| a + b)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Curve {
    Linear,
    Bezier(CubicBezier),
}

impl Curve {
    /// Progress at local time `t`, clamped to `[0, 1]`.
    pub fn progress(self, t: f64) -> f64 {
        match self {
            Curve::Linear => t.clamp(0.0, 1.0),
            Curve::Bezier(curve) => curve.ease(t),
        }
    }
}

/// A track's slot on its node, as a Core Animation key is: a track added to an occupied slot
/// replaces the one there. `Base` is the movement; the others are added on top of it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Channel {
    Base,
    Bounce,
    Nudge,
}

impl Channel {
    const ALL: [Channel; 3] = [Channel::Base, Channel::Bounce, Channel::Nudge];

    pub fn is_additive(self) -> bool {
        self != Channel::Base
    }
}

/// Which constructor made a track, and so which Core Animation class plays it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
    /// `from` to `to` on one curve: a `CABasicAnimation`.
    Between,
    /// Evenly spaced and linear between: a `CAKeyframeAnimation` with values only.
    Samples,
    /// Key times and a curve per segment: a `CAKeyframeAnimation` with `keyTimes` and
    /// `timingFunctions`.
    Keyframes,
}

/// Keyframes on one node and channel, from `begin` on the media clock for `seconds`, at unit key
/// times with one curve per segment.
#[derive(Clone, PartialEq, Debug)]
pub struct Track {
    node: Node,
    channel: Channel,
    form: Form,
    begin: f64,
    seconds: f64,
    keys: Vec<(f64, Value)>,
    curves: Vec<Curve>,
}

impl Track {
    /// `from` to `to` on one curve: a `CABasicAnimation`.
    pub fn between(
        node: Node,
        channel: Channel,
        from: Value,
        to: Value,
        begin: f64,
        seconds: f64,
        curve: Curve,
    ) -> Track {
        let keys = vec![(0.0, from), (1.0, to)];
        Track::made(Form::Between, node, channel, keys, vec![curve], begin, seconds)
    }

    /// Evenly spaced and linear between: a `CAKeyframeAnimation` with neither `keyTimes` nor
    /// `timingFunctions`, which is how a spring leg and the nudge are played.
    pub fn samples(
        node: Node,
        channel: Channel,
        values: impl IntoIterator<Item = Value>,
        begin: f64,
        seconds: f64,
    ) -> Track {
        let values: Vec<Value> = values.into_iter().collect();
        let last = values.len().saturating_sub(1).max(1) as f64;
        let curves = vec![Curve::Linear; values.len().saturating_sub(1)];
        let keys = values.into_iter().enumerate().map(|(k, v)| (k as f64 / last, v)).collect();
        Track::made(Form::Samples, node, channel, keys, curves, begin, seconds)
    }

    /// Panics unless there are two keys or more, in order from 0 to 1, one curve per segment, every
    /// value of the kind the node holds, and `seconds` is not negative.
    pub fn keyframes(
        node: Node,
        channel: Channel,
        keys: Vec<(f64, Value)>,
        curves: Vec<Curve>,
        begin: f64,
        seconds: f64,
    ) -> Track {
        Track::made(Form::Keyframes, node, channel, keys, curves, begin, seconds)
    }

    fn made(
        form: Form,
        node: Node,
        channel: Channel,
        keys: Vec<(f64, Value)>,
        curves: Vec<Curve>,
        begin: f64,
        seconds: f64,
    ) -> Track {
        assert!(keys.len() >= 2, "a track needs two keys: {keys:?}");
        assert_eq!(curves.len(), keys.len() - 1, "one curve per segment");
        assert!(
            keys[0].0 == 0.0
                && keys[keys.len() - 1].0 == 1.0
                && keys.windows(2).all(|pair| pair[0].0 <= pair[1].0),
            "key times run from 0 to 1 in order: {keys:?}"
        );
        assert!(
            keys.iter().all(|(_, value)| node.holds(*value)),
            "{node:?} holds one kind of value: {keys:?}"
        );
        assert!(seconds >= 0.0, "{seconds}s");
        Track {
            node,
            channel,
            form,
            begin,
            seconds,
            keys,
            curves,
        }
    }

    pub fn node(&self) -> Node {
        self.node
    }

    pub fn channel(&self) -> Channel {
        self.channel
    }

    pub fn form(&self) -> Form {
        self.form
    }

    /// The same track, its timing, curves and every other key kept, ending at `to`. Panics if `to`
    /// is not the kind its node holds.
    pub fn ending_at(&self, to: Value) -> Track {
        assert!(self.node.holds(to), "{:?} cannot hold {to:?}", self.node);
        let mut track = self.clone();
        if let Some((_, last)) = track.keys.last_mut() {
            *last = to;
        }
        track
    }

    pub fn begin(&self) -> f64 {
        self.begin
    }

    pub fn seconds(&self) -> f64 {
        self.seconds
    }

    pub fn keys(&self) -> &[(f64, Value)] {
        &self.keys
    }

    pub fn curves(&self) -> &[Curve] {
        &self.curves
    }

    pub fn ends(&self) -> f64 {
        self.begin + self.seconds
    }

    /// The value at media time `at` while the track plays, from `begin` up to but not including
    /// its end, and none outside it: Core Animation's default fill mode draws nothing there. A
    /// track of no length never plays, but Core Animation plays a zero `duration` as 0.25s, so a
    /// renderer installs nothing for one.
    pub fn at(&self, at: f64) -> Option<Value> {
        if !(self.begin <= at && at < self.ends()) {
            return None;
        }
        let unit = (at - self.begin) / self.seconds;
        let segment = (self.keys.partition_point(|(t, _)| *t <= unit) - 1).min(self.keys.len() - 2);
        let ((t0, from), (t1, to)) = (self.keys[segment], self.keys[segment + 1]);
        let local = if t1 > t0 {
            (unit - t0) / (t1 - t0)
        } else {
            1.0
        };
        Some(from.lerp(to, self.curves[segment].progress(local)))
    }
}

/// One change to a score, and what a renderer carries out to draw it.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// The node's model, where it is drawn once its tracks end. A container's first opens it.
    Place(Node, Value),
    /// A track, replacing the one on its node and channel.
    Add(Track),
    /// Takes the track off a node's channel.
    Cut(Node, Channel),
    /// A tile into a container: its first edit, a tile joining a flight's included; a later one
    /// moves it to another container.
    Ride(WindowId, GroupKey),
    /// A node out, with every track on it; a tile with its ride.
    Drop(Node),
    /// How deep every container and tile is drawn, a cut: depth does not interpolate.
    Bank(Banding),
}

/// Where every node ends, as a layer's model value, the tracks that carry them there, which container
/// each tile rides, and how deep each is drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Score {
    models: HashMap<Node, Value>,
    tracks: HashMap<(Node, Channel), Track>,
    parents: HashMap<WindowId, GroupKey>,
    banding: Banding,
}

impl Score {
    /// Folds `edit` in: the one way a score changes. Panics on a `Place` of a value its node does
    /// not hold.
    pub fn apply(&mut self, edit: &Edit) {
        match edit {
            Edit::Place(node, value) => self.place(*node, *value),
            Edit::Add(track) => self.add(track.clone()),
            Edit::Cut(node, channel) => {
                self.tracks.remove(&(*node, *channel));
            }
            Edit::Ride(window, key) => {
                self.parents.insert(*window, *key);
            }
            Edit::Drop(node) => self.remove(*node),
            Edit::Bank(banding) => self.banding = banding.clone(),
        }
    }

    fn place(&mut self, node: Node, value: Value) {
        assert!(node.holds(value), "{node:?} cannot hold {value:?}");
        self.models.insert(node, value);
    }

    fn add(&mut self, track: Track) { self.tracks.insert((track.node, track.channel), track); }

    fn remove(&mut self, node: Node) {
        self.models.remove(&node);
        self.tracks.retain(|(on, _), _| *on != node);
        if let Node::Tile(window) = node {
            self.parents.remove(&window);
        }
    }

    /// Where `node` is drawn once its tracks have ended.
    pub fn model(&self, node: Node) -> Option<Value> {
        self.models.get(&node).copied()
    }

    pub fn track(&self, node: Node, channel: Channel) -> Option<&Track> {
        self.tracks.get(&(node, channel))
    }

    /// The container `window`'s tile rides.
    pub fn parent(&self, window: WindowId) -> Option<GroupKey> {
        self.parents.get(&window).copied()
    }

    /// Every container placed.
    pub fn containers(&self) -> impl Iterator<Item = GroupKey> + '_ {
        self.models.keys().filter_map(|node| match node {
            Node::Container(key) => Some(*key),
            Node::Tile(_) => None,
        })
    }

    /// Every tile, with the container it rides.
    pub fn tiles(&self) -> impl Iterator<Item = (WindowId, GroupKey)> + '_ {
        self.parents.iter().map(|(window, key)| (*window, *key))
    }

    pub fn banding(&self) -> &Banding { &self.banding }

    /// Whether a track on `channel` plays at `at` on any node.
    pub fn plays(&self, channel: Channel, at: f64) -> bool {
        self.tracks
            .values()
            .any(|track| track.channel == channel && track.at(at).is_some())
    }

    /// `node`'s tracks in the order Core Animation must draw them. It composes a layer's
    /// animations in order of their begin times, ties in the order added, a non-additive one
    /// replacing whatever came before it: a renderer that replaces a `Base` track begins the node's
    /// other tracks again with it, as far into their own time as they have got.
    pub fn tracks(&self, node: Node) -> impl Iterator<Item = &Track> {
        Channel::ALL.into_iter().filter_map(move |channel| self.track(node, channel))
    }

    /// What is drawn at `at`: where the movement has `node`, plus every additive track playing.
    /// None for a node never placed.
    pub fn sample(&self, node: Node, at: f64) -> Option<Value> {
        let along = self.along(node, at)?;
        Some(
            self.tracks(node)
                .filter(|track| track.channel.is_additive())
                .filter_map(|track| track.at(at))
                .fold(along, Value::plus),
        )
    }

    /// The `Base` track's value at `at` while it plays, the model otherwise. A new `Base` track
    /// starts here: begun from `sample`, it carries every accent riding the node twice.
    pub fn along(&self, node: Node, at: f64) -> Option<Value> {
        let model = self.model(node)?;
        let base = self.track(node, Channel::Base).and_then(|track| track.at(at));
        Some(base.unwrap_or(model))
    }

    /// Where `window`'s tile is drawn at `at` in the overlay's space: its frame in its container,
    /// moved by where the container is drawn.
    pub fn drawn(&self, window: WindowId, at: f64) -> Option<CGRect> {
        let key = self.parent(window)?;
        match (
            self.sample(Node::Tile(window), at)?,
            self.sample(Node::Container(key), at)?,
        ) {
            (Value::Rect(frame), Value::Point(at)) => Some(CGRect::new(
                CGPoint::new(frame.origin.x + at.x, frame.origin.y + at.y),
                frame.size,
            )),
            _ => unreachable!("a tile holds a frame and a container a point"),
        }
    }

    /// Every container at `at`: where it is drawn, and where its movement has it.
    pub fn presented(&self, at: f64) -> Presented {
        let mut presented = Presented::default();
        for key in self.containers() {
            let node = Node::Container(key);
            if let (Some(Value::Point(drawn)), Some(Value::Point(along))) =
                (self.sample(node, at), self.along(node, at))
            {
                presented.drawn.insert(key, drawn);
                presented.along.insert(key, along);
            }
        }
        presented
    }

    /// When the last of `node`'s tracks ends; none with no tracks.
    pub fn ends(&self, node: Node) -> Option<f64> {
        self.tracks(node).map(Track::ends).reduce(f64::max)
    }

    /// Every track has ended, so every node is drawn at its model.
    pub fn settled(&self, at: f64) -> bool {
        self.tracks.values().all(|track| track.ends() <= at)
    }

    /// Seconds from `at` until `settled`.
    pub fn remaining(&self, at: f64) -> f64 {
        self.tracks.values().map(|track| track.ends() - at).fold(0.0, f64::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::domain::motion::easing::{EASE_IN_OUT, MOTION_CURVE};

    const CURVES: [Curve; 3] = [
        Curve::Linear,
        Curve::Bezier(MOTION_CURVE),
        Curve::Bezier(EASE_IN_OUT),
    ];

    fn strip() -> Node {
        Node::Container(GroupKey::Rigid(1))
    }

    fn tile() -> Node {
        Node::Tile(WindowId::new(1, 1))
    }

    fn point(x: f64, y: f64) -> Value {
        Value::Point(CGPoint::new(x, y))
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Value {
        Value::Rect(CGRect::new(CGPoint::new(x, y), CGSize::new(w, h)))
    }

    fn x(value: Option<Value>) -> f64 {
        match value {
            Some(Value::Point(p)) => p.x,
            Some(Value::Rect(r)) => r.origin.x,
            None => panic!("no value"),
        }
    }

    fn gap(a: Value, b: Value) -> f64 {
        let components = |v: Value| match v {
            Value::Point(p) => [p.x, p.y, 0.0, 0.0],
            Value::Rect(r) => [r.origin.x, r.origin.y, r.size.width, r.size.height],
        };
        let (a, b) = (components(a), components(b));
        (0..4).map(|i| (a[i] - b[i]).abs()).fold(0.0, f64::max)
    }

    fn base(from: f64, to: f64, begin: f64, seconds: f64, curve: Curve) -> Track {
        Track::between(
            strip(),
            Channel::Base,
            point(from, 0.0),
            point(to, 0.0),
            begin,
            seconds,
            curve,
        )
    }

    fn moving(from: f64, to: f64, begin: f64, seconds: f64, curve: Curve) -> Score {
        let mut score = Score::default();
        score.place(strip(), point(to, 0.0));
        score.add(base(from, to, begin, seconds, curve));
        score
    }

    /// Every curve leaves at 0, arrives at 1 and never steps back: a curve that did would show a
    /// window sliding backwards mid-flight.
    #[test]
    fn every_curve_runs_from_rest_to_arrival_and_never_back() {
        for curve in CURVES {
            assert_eq!(curve.progress(0.0), 0.0, "{curve:?}");
            assert_eq!(curve.progress(1.0), 1.0, "{curve:?}");
            assert_eq!(curve.progress(-0.5), 0.0, "{curve:?}");
            assert_eq!(curve.progress(1.5), 1.0, "{curve:?}");
            let mut previous = 0.0;
            for step in 0..=1000 {
                let p = curve.progress(step as f64 / 1000.0);
                assert!(p >= previous, "{curve:?} went back at {step}");
                previous = p;
            }
        }
    }

    /// A movement on any curve is drawn at its start when it begins, nears its end as it ends, and
    /// goes one way only between.
    #[test]
    fn a_movement_on_any_curve_runs_from_its_start_to_its_end() {
        for curve in CURVES {
            let score = moving(0.0, 1722.0, 10.0, 0.35, curve);
            assert_eq!(x(score.sample(strip(), 10.0)), 0.0, "{curve:?}");
            assert!(
                (x(score.sample(strip(), 10.35 - 1e-9)) - 1722.0).abs() < 1e-3,
                "{curve:?}"
            );
            assert_eq!(x(score.sample(strip(), 10.35)), 1722.0, "{curve:?}");
            let mut previous = 0.0;
            for step in 0..=350 {
                let drawn = x(score.sample(strip(), 10.0 + step as f64 / 1000.0));
                assert!(drawn >= previous, "{curve:?} went back at {step}ms");
                previous = drawn;
            }
        }
    }

    /// A Bezier segment is the timing function the overlay hands Core Animation: `ease` of local
    /// time, the curve `a_leg_the_score_and_the_render_server_run_one_curve` pins against its control
    /// points.
    #[test]
    fn a_bezier_segment_runs_on_its_timing_function() {
        for curve in [MOTION_CURVE, EASE_IN_OUT] {
            let score = moving(-861.0, 861.0, 3.0, 0.5, Curve::Bezier(curve));
            for step in 0..500 {
                let t = step as f64 / 500.0;
                let drawn = x(score.sample(strip(), 3.0 + 0.5 * t));
                let expected = -861.0 + 1722.0 * curve.ease(t);
                assert!((drawn - expected).abs() < 1e-6, "{curve:?} at {t}");
            }
        }
        let linear = moving(0.0, 100.0, 0.0, 2.0, Curve::Linear);
        assert!((x(linear.sample(strip(), 0.5)) - 25.0).abs() < 1e-12);
    }

    /// A tile's frame moves and resizes on one curve: origin and size are at the same progress at
    /// every instant.
    #[test]
    fn a_tile_moves_and_resizes_on_one_curve() {
        let from = rect(0.0, 32.0, 859.0, 1081.0);
        let to = rect(861.0, 32.0, 1720.0, 540.0);
        let mut score = Score::default();
        score.place(tile(), to);
        score.add(Track::between(
            tile(),
            Channel::Base,
            from,
            to,
            1.0,
            0.35,
            Curve::Bezier(MOTION_CURVE),
        ));
        for step in 0..35 {
            let t = step as f64 / 35.0;
            let Some(Value::Rect(r)) = score.sample(tile(), 1.0 + 0.35 * t) else {
                panic!("a tile holds a rect");
            };
            let p = MOTION_CURVE.ease(t);
            assert!((r.origin.x - 861.0 * p).abs() < 1e-6);
            assert!((r.size.width - (859.0 + 861.0 * p)).abs() < 1e-6);
            assert!((r.size.height - (1081.0 - 541.0 * p)).abs() < 1e-6);
            assert_eq!(r.origin.y, 32.0);
        }
    }

    /// Keys at uneven times map each instant to its own segment, on that segment's curve.
    #[test]
    fn key_times_pick_the_segment() {
        let mut score = Score::default();
        score.place(strip(), point(0.0, 0.0));
        score.add(Track::keyframes(
            strip(),
            Channel::Base,
            vec![
                (0.0, point(0.0, 0.0)),
                (0.25, point(100.0, 0.0)),
                (1.0, point(0.0, 0.0)),
            ],
            vec![Curve::Linear, Curve::Bezier(EASE_IN_OUT)],
            0.0,
            1.0,
        ));
        assert!((x(score.sample(strip(), 0.125)) - 50.0).abs() < 1e-12);
        assert!((x(score.sample(strip(), 0.25)) - 100.0).abs() < 1e-12);
        let home = 100.0 * (1.0 - EASE_IN_OUT.ease(0.5 / 0.75));
        assert!((x(score.sample(strip(), 0.75)) - home).abs() < 1e-9);
        assert!(
            (x(score.sample(strip(), 0.625)) - 50.0).abs() < 1e-9,
            "half way home"
        );
    }

    /// Evenly spaced samples are drawn exactly at their key times, as Core Animation spaces a
    /// keyframe animation with no `keyTimes`, and on the straight line between them.
    #[test]
    fn evenly_spaced_samples_are_drawn_at_their_key_times() {
        let values = [0.0, 100.0, 50.0, 300.0];
        let mut score = Score::default();
        score.place(strip(), point(300.0, 0.0));
        score.add(Track::samples(
            strip(),
            Channel::Base,
            values.map(|v| point(v, 0.0)),
            10.0,
            1.0,
        ));
        for (k, value) in values.iter().enumerate() {
            let at = 10.0 + k as f64 / 3.0;
            assert!((x(score.sample(strip(), at)) - value).abs() < 1e-9, "key {k}");
        }
        // What the probe read off Core Animation for the same keyframes.
        for (at, drawn) in [(1.0 / 12.0, 25.0), (5.0 / 12.0, 87.5), (0.75, 112.5)] {
            assert!(
                (x(score.sample(strip(), 10.0 + at)) - drawn).abs() < 1e-9,
                "{at}"
            );
        }
        let track = score.track(strip(), Channel::Base).unwrap();
        assert_eq!(track.curves(), [Curve::Linear; 3]);
        assert_eq!(
            track.keys().iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]
        );
    }

    /// Before its begin and from its end on, a movement draws the model, as Core Animation's
    /// default fill mode and removal on completion do. A movement that does not end on the model
    /// jumps to it at its end: the renderer places a node where its movement ends.
    #[test]
    fn outside_its_window_a_movement_draws_the_model() {
        let score = moving(0.0, 500.0, 2.0, 0.5, Curve::Bezier(MOTION_CURVE));
        assert_eq!(x(score.sample(strip(), 1.999)), 500.0, "not begun: the model");
        assert_eq!(x(score.sample(strip(), 2.0)), 0.0, "begun: its start");
        assert_eq!(x(score.sample(strip(), 2.5)), 500.0);
        assert_eq!(x(score.sample(strip(), 99.0)), 500.0);

        let mut short = moving(0.0, 500.0, 2.0, 0.5, Curve::Linear);
        short.place(strip(), point(800.0, 0.0));
        assert!((x(short.sample(strip(), 2.499)) - 499.0).abs() < 1e-6);
        assert_eq!(x(short.sample(strip(), 2.5)), 800.0, "the model, at once");
    }

    /// An accent adds nothing outside its window, and the movement under it is unchanged there.
    #[test]
    fn outside_its_window_an_accent_adds_nothing() {
        let mut score = Score::default();
        score.place(strip(), point(40.0, 0.0));
        score.add(Track::samples(
            strip(),
            Channel::Nudge,
            [point(0.0, 0.0), point(-300.0, 0.0), point(0.0, 0.0)],
            5.0,
            0.4,
        ));
        assert_eq!(x(score.sample(strip(), 4.9)), 40.0);
        assert!((x(score.sample(strip(), 5.2)) + 260.0).abs() < 1e-9);
        assert_eq!(x(score.sample(strip(), 5.4)), 40.0);
    }

    /// The movement, the bounce and the nudge on one container add up: each accent rides the
    /// movement additively, as their Core Animation keys do.
    #[test]
    fn a_bounce_and_a_nudge_add_to_the_movement() {
        let mut score = moving(0.0, -861.0, 0.0, 0.35, Curve::Bezier(MOTION_CURVE));
        let bounce = Track::keyframes(
            strip(),
            Channel::Bounce,
            vec![
                (0.0, point(0.0, 0.0)),
                (0.35, point(0.0, -72.0)),
                (1.0, point(0.0, 0.0)),
            ],
            vec![Curve::Bezier(MOTION_CURVE), Curve::Bezier(EASE_IN_OUT)],
            0.1,
            0.35,
        );
        let nudge = Track::samples(
            strip(),
            Channel::Nudge,
            [point(0.0, 0.0), point(287.0, 0.0), point(0.0, 0.0)],
            0.05,
            0.455,
        );
        score.add(bounce.clone());
        score.add(nudge.clone());
        for step in 0..60 {
            let at = step as f64 / 100.0;
            let mut expected = score.along(strip(), at).unwrap();
            for accent in [&bounce, &nudge] {
                if let Some(offset) = accent.at(at) {
                    expected = expected.plus(offset);
                }
            }
            assert!(gap(score.sample(strip(), at).unwrap(), expected) < 1e-12, "{at}");
        }
        let at = 0.2;
        let Some(Value::Point(drawn)) = score.sample(strip(), at) else {
            panic!()
        };
        let Some(Value::Point(movement)) = score.along(strip(), at) else {
            panic!()
        };
        let (Some(Value::Point(b)), Some(Value::Point(n))) = (bounce.at(at), nudge.at(at)) else {
            panic!("both accents play at {at}")
        };
        assert!(b.y < -1.0 && n.x > 1.0, "both ride: {b:?} {n:?}");
        assert!((drawn.x - (movement.x + n.x)).abs() < 1e-12);
        assert!((drawn.y - (movement.y + b.y)).abs() < 1e-12);
    }

    /// A track added on an occupied node and channel replaces the one there, as a Core Animation
    /// key does; another channel or another node is left alone.
    #[test]
    fn a_track_replaces_the_one_on_its_node_and_channel() {
        let mut score = moving(0.0, 100.0, 0.0, 1.0, Curve::Linear);
        let second = base(50.0, 200.0, 0.5, 1.0, Curve::Linear);
        score.apply(&Edit::Add(second.clone()));
        assert_eq!(score.track(strip(), Channel::Base), Some(&second));

        let bounce = Track::between(
            strip(),
            Channel::Bounce,
            point(0.0, 0.0),
            point(0.0, 0.0),
            0.0,
            1.0,
            Curve::Linear,
        );
        score.apply(&Edit::Add(bounce.clone()));
        let other = Node::Container(GroupKey::Floating);
        score.apply(&Edit::Place(other, point(0.0, 0.0)));
        let elsewhere = Track::between(
            other,
            Channel::Base,
            point(9.0, 0.0),
            point(0.0, 0.0),
            0.0,
            1.0,
            Curve::Linear,
        );
        score.apply(&Edit::Add(elsewhere.clone()));
        assert_eq!(score.track(strip(), Channel::Base), Some(&second));
        assert_eq!(score.track(strip(), Channel::Bounce), Some(&bounce));
        assert_eq!(score.track(other, Channel::Base), Some(&elsewhere));
        assert_eq!(score.tracks(strip()).count(), 2);
    }

    /// A node's tracks come out movement first, whatever order they went in: Core Animation lets
    /// a movement added after an accent hide it.
    #[test]
    fn a_nodes_tracks_come_out_movement_first() {
        let mut score = Score::default();
        score.place(strip(), point(0.0, 0.0));
        let accent = |channel| {
            Track::samples(strip(), channel, [point(0.0, 0.0), point(5.0, 0.0)], 0.0, 1.0)
        };
        score.add(accent(Channel::Nudge));
        score.add(accent(Channel::Bounce));
        score.add(base(0.0, 0.0, 0.0, 1.0, Curve::Linear));
        let order: Vec<Channel> = score.tracks(strip()).map(Track::channel).collect();
        assert_eq!(order, Channel::ALL);
        assert!(!Channel::Base.is_additive());
        assert!(Channel::Bounce.is_additive() && Channel::Nudge.is_additive());
    }

    /// Settled once the last track has ended, wherever the tracks are; `remaining` counts down to
    /// it and `ends` says when each node's last track ends.
    #[test]
    fn settled_once_every_track_has_ended() {
        let mut score = moving(0.0, 100.0, 1.0, 0.35, Curve::Bezier(MOTION_CURVE));
        assert!(!score.settled(0.0));
        assert!((score.remaining(0.0) - 1.35).abs() < 1e-12);
        score.add(Track::samples(
            strip(),
            Channel::Nudge,
            [point(0.0, 0.0), point(9.0, 0.0), point(0.0, 0.0)],
            1.2,
            0.455,
        ));
        score.place(tile(), rect(0.0, 0.0, 10.0, 10.0));
        score.add(Track::between(
            tile(),
            Channel::Base,
            rect(5.0, 0.0, 10.0, 10.0),
            rect(0.0, 0.0, 10.0, 10.0),
            1.0,
            0.2,
            Curve::Linear,
        ));
        let last = 1.2 + 0.455;
        assert_eq!(
            score.ends(strip()),
            Some(last),
            "the nudge outlasts the movement"
        );
        assert_eq!(score.ends(tile()), Some(1.0 + 0.2));
        assert_eq!(score.ends(Node::Container(GroupKey::Floating)), None);
        for at in [1.0, 1.3, 1.5, last - 1e-9] {
            assert!(!score.settled(at), "{at}");
            assert!((score.remaining(at) - (last - at)).abs() < 1e-12, "{at}");
        }
        assert!(score.settled(last));
        assert_eq!(score.remaining(last), 0.0);
        assert_eq!(score.remaining(9.0), 0.0);
        assert!(Score::default().settled(0.0), "nothing playing");
    }

    /// A track of no length is a cut: the node is drawn at its model at once, nothing is left
    /// playing, and an accent of no length adds nothing.
    #[test]
    fn a_track_of_no_length_is_a_cut() {
        let mut score = moving(0.0, 100.0, 1.0, 0.0, Curve::Bezier(MOTION_CURVE));
        score.add(Track::samples(
            strip(),
            Channel::Bounce,
            [point(0.0, 0.0), point(0.0, 72.0), point(0.0, 0.0)],
            1.0,
            0.0,
        ));
        for at in [0.5, 1.0, 1.0 + 1e-9, 2.0] {
            assert_eq!(score.sample(strip(), at), Some(point(100.0, 0.0)), "{at}");
        }
        assert!(score.settled(1.0));
        assert_eq!(score.remaining(1.0), 0.0);
    }

    /// `presented` is every container's `sample` and `along`: drawn with the accents riding it,
    /// and where its movement has it without them.
    #[test]
    fn presented_draws_accents_and_along_does_not() {
        let mut score = moving(0.0, -861.0, 0.0, 0.35, Curve::Bezier(MOTION_CURVE));
        score.add(Track::samples(
            strip(),
            Channel::Nudge,
            [point(0.0, 0.0), point(287.0, 0.0), point(0.0, 0.0)],
            0.0,
            0.455,
        ));
        let still = GroupKey::Floating;
        score.place(Node::Container(still), point(0.0, 40.0));
        score.place(tile(), rect(0.0, 0.0, 10.0, 10.0));
        let at = 0.2;
        let presented = score.presented(at);
        let key = GroupKey::Rigid(1);
        let (Some(Value::Point(drawn)), Some(Value::Point(along))) =
            (score.sample(strip(), at), score.along(strip(), at))
        else {
            panic!("the strip is placed")
        };
        assert_eq!(presented.drawn[&key], drawn);
        assert_eq!(presented.along[&key], along);
        assert!(drawn.x - along.x > 200.0, "the nudge rides what is drawn only");
        assert_eq!(presented.drawn[&still], CGPoint::new(0.0, 40.0));
        assert_eq!(presented.along[&still], CGPoint::new(0.0, 40.0));
        assert_eq!(presented.drawn.len(), 2, "containers only");
    }

    /// `ending_at` moves the last key and nothing else: the begin, the length, the curves and
    /// every key before it stay, so a snap lands where the movement was going to on its own clock.
    #[test]
    fn ending_at_keeps_a_tracks_timing_and_moves_its_end() {
        let from = rect(1726.0, 32.0, 861.0, 1081.0);
        let to = rect(865.0, 32.0, 861.0, 1081.0);
        let real = rect(866.0, 32.0, 860.0, 1081.0);
        let track = Track::between(
            tile(),
            Channel::Base,
            from,
            to,
            100.0,
            0.35,
            Curve::Bezier(MOTION_CURVE),
        );
        let snapped = track.ending_at(real);
        assert_eq!(snapped.keys(), [(0.0, from), (1.0, real)]);
        assert_eq!(
            (snapped.node(), snapped.channel(), snapped.form()),
            (track.node(), track.channel(), track.form())
        );
        assert_eq!((snapped.begin(), snapped.seconds()), (100.0, 0.35));
        assert_eq!(snapped.curves(), track.curves());

        let samples = Track::samples(
            strip(),
            Channel::Base,
            [point(0.0, 0.0), point(5.0, 0.0), point(9.0, 0.0)],
            1.0,
            0.5,
        );
        let moved = samples.ending_at(point(10.0, 0.0));
        assert_eq!(&moved.keys()[..2], &samples.keys()[..2]);
        assert_eq!(moved.keys()[2], (1.0, point(10.0, 0.0)));
        assert_eq!(moved.form(), Form::Samples);
    }

    /// The form is the constructor's, never read off the keys: evenly spaced keys made as
    /// keyframes stay keyframes, and two samples stay samples.
    #[test]
    fn every_track_keeps_the_form_it_was_made_with() {
        let between = base(0.0, 1.0, 0.0, 1.0, Curve::Linear);
        assert_eq!(between.form(), Form::Between);
        let samples = Track::samples(
            strip(),
            Channel::Base,
            [point(0.0, 0.0), point(1.0, 0.0)],
            0.0,
            1.0,
        );
        assert_eq!(samples.form(), Form::Samples);
        assert_eq!(samples.keys(), between.keys());
        let keyframes = Track::keyframes(
            strip(),
            Channel::Base,
            vec![(0.0, point(0.0, 0.0)), (1.0, point(1.0, 0.0))],
            vec![Curve::Linear],
            0.0,
            1.0,
        );
        assert_eq!(keyframes.form(), Form::Keyframes);
        assert_ne!(keyframes, between, "same keys, another class");
        assert_eq!(keyframes.ending_at(point(2.0, 0.0)).form(), Form::Keyframes);
    }

    /// A cut takes one channel off a node and leaves the rest; a drop takes the node, its model
    /// and every track, and `containers` names what is left.
    #[test]
    fn a_cut_takes_one_channel_and_a_removal_the_whole_node() {
        let mut score = moving(0.0, 100.0, 0.0, 1.0, Curve::Linear);
        let bounce = Track::samples(
            strip(),
            Channel::Bounce,
            [point(0.0, 0.0), point(0.0, 9.0), point(0.0, 0.0)],
            0.0,
            1.0,
        );
        let floating = Node::Container(GroupKey::Floating);
        for edit in [
            Edit::Add(bounce),
            Edit::Place(floating, point(0.0, 0.0)),
            Edit::Place(tile(), rect(0.0, 0.0, 10.0, 10.0)),
            Edit::Cut(strip(), Channel::Bounce),
        ] {
            score.apply(&edit);
        }
        assert_eq!(score.track(strip(), Channel::Bounce), None);
        let cut = score.clone();
        score.apply(&Edit::Cut(strip(), Channel::Bounce));
        assert_eq!(score, cut, "nothing left to cut");
        assert!(
            score.track(strip(), Channel::Base).is_some(),
            "the movement stays"
        );

        let mut keys: Vec<GroupKey> = score.containers().collect();
        keys.sort_by_key(|key| format!("{key:?}"));
        assert_eq!(keys, [GroupKey::Floating, GroupKey::Rigid(1)]);
        score.apply(&Edit::Drop(strip()));
        assert_eq!(score.model(strip()), None);
        assert_eq!(score.tracks(strip()).count(), 0);
        assert_eq!(score.containers().collect::<Vec<_>>(), [GroupKey::Floating]);
        assert!(score.settled(0.5), "nothing is left playing");
        assert!(score.model(tile()).is_some(), "the tile stays");
    }

    fn window(idx: u32) -> WindowId { WindowId::new(1, idx) }

    /// A score is the fold of its edits: the same edits in the same order make the same score
    /// however they are split, which is how a renderer's replica and the score it plays agree, and
    /// each kind is what the score says after it: the last `Place` the model, the last `Ride` the
    /// container, the last `Bank` the depths.
    #[test]
    fn a_score_is_the_fold_of_its_edits() {
        let (a, b) = (window(1), window(2));
        let (strip, floating) = (GroupKey::Rigid(1), GroupKey::Floating);
        let moved = Track::between(
            Node::Tile(a),
            Channel::Base,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 0.0, 10.0, 10.0),
            1.0,
            0.5,
            Curve::Linear,
        );
        let deep = Banding {
            group_order: vec![strip],
            ..Banding::default()
        };
        let edits = [
            Edit::Place(Node::Container(strip), point(0.0, 0.0)),
            Edit::Place(Node::Container(floating), point(0.0, 0.0)),
            Edit::Ride(a, strip),
            Edit::Place(Node::Tile(a), rect(0.0, 0.0, 10.0, 10.0)),
            Edit::Ride(b, strip),
            Edit::Place(Node::Tile(b), rect(20.0, 0.0, 10.0, 10.0)),
            Edit::Bank(Banding::default()),
            Edit::Place(Node::Container(strip), point(-861.0, 0.0)),
            Edit::Place(Node::Tile(a), rect(50.0, 0.0, 10.0, 10.0)),
            Edit::Add(moved.clone()),
            Edit::Ride(b, floating),
            Edit::Bank(deep.clone()),
        ];
        let mut whole = Score::default();
        for edit in &edits {
            whole.apply(edit);
        }
        for split in 0..=edits.len() {
            let mut first = Score::default();
            for edit in &edits[..split] {
                first.apply(edit);
            }
            let mut rest = first.clone();
            for edit in &edits[split..] {
                rest.apply(edit);
            }
            assert_eq!(rest, whole, "split at {split}");
        }
        assert_eq!(whole.model(Node::Container(strip)), Some(point(-861.0, 0.0)));
        assert_eq!(whole.model(Node::Tile(a)), Some(rect(50.0, 0.0, 10.0, 10.0)));
        assert_eq!(whole.track(Node::Tile(a), Channel::Base), Some(&moved));
        assert_eq!((whole.parent(a), whole.parent(b)), (Some(strip), Some(floating)));
        assert_eq!(whole.banding(), &deep);
    }

    /// A tile rides one container at a time and is drawn where it sits in it, moved by where the
    /// container is drawn; a drop takes the tile out with its ride and its tracks, and leaves the
    /// container.
    #[test]
    fn a_tile_rides_one_container_and_a_drop_takes_its_tracks() {
        let (a, b) = (window(1), window(2));
        let (rigid, loose) = (GroupKey::Rigid(1), GroupKey::Loose);
        let mut score = moving(0.0, -861.0, 0.0, 1.0, Curve::Linear);
        for edit in [
            Edit::Place(Node::Container(loose), point(0.0, 0.0)),
            Edit::Ride(a, rigid),
            Edit::Place(Node::Tile(a), rect(865.0, 32.0, 859.0, 1081.0)),
            Edit::Ride(b, rigid),
            Edit::Place(Node::Tile(b), rect(4.0, 32.0, 859.0, 1081.0)),
            Edit::Add(Track::between(
                Node::Tile(b),
                Channel::Base,
                rect(4.0, 32.0, 859.0, 1081.0),
                rect(4.0, 32.0, 859.0, 1081.0),
                0.0,
                1.0,
                Curve::Linear,
            )),
        ] {
            score.apply(&edit);
        }
        let along = x(score.along(strip(), 0.25));
        assert_eq!(
            score.drawn(a, 0.25),
            Some(CGRect::new(
                CGPoint::new(865.0 + along, 32.0),
                CGSize::new(859.0, 1081.0)
            ))
        );
        score.apply(&Edit::Ride(a, loose));
        assert_eq!(score.parent(a), Some(loose));
        assert_eq!(
            score.drawn(a, 0.25).map(|r| r.origin.x),
            Some(865.0),
            "in its new container"
        );
        let mut tiles: Vec<(WindowId, GroupKey)> = score.tiles().collect();
        tiles.sort_by_key(|(window, _)| window.idx);
        assert_eq!(tiles, [(a, loose), (b, rigid)], "one container each");

        score.apply(&Edit::Drop(Node::Tile(b)));
        assert_eq!(score.parent(b), None);
        assert_eq!(score.model(Node::Tile(b)), None);
        assert_eq!(score.track(Node::Tile(b), Channel::Base), None);
        assert_eq!(score.drawn(b, 0.25), None);
        assert_eq!(score.tiles().count(), 1);
        assert!(score.model(strip()).is_some(), "the container stays");
    }

    /// `plays` says whether any node has a track of the channel playing: from its begin up to its
    /// end, never outside it, and never for a track of no length.
    #[test]
    fn plays_says_whether_a_channel_is_playing() {
        let mut score = moving(0.0, -861.0, 1.0, 0.35, Curve::Linear);
        let floating = Node::Container(GroupKey::Floating);
        score.apply(&Edit::Place(floating, point(0.0, 0.0)));
        score.apply(&Edit::Add(Track::samples(
            floating,
            Channel::Nudge,
            [point(0.0, 0.0), point(-300.0, 0.0), point(0.0, 0.0)],
            2.0,
            0.4,
        )));
        assert!(!score.plays(Channel::Base, 0.99));
        assert!(score.plays(Channel::Base, 1.0));
        assert!(!score.plays(Channel::Base, 1.35));
        assert!(!score.plays(Channel::Nudge, 1.5), "not begun");
        assert!(score.plays(Channel::Nudge, 2.2), "on another node");
        assert!(!score.plays(Channel::Nudge, 2.4), "home");
        assert!(!score.plays(Channel::Bounce, 2.2));
        score.apply(&Edit::Add(Track::samples(
            strip(),
            Channel::Bounce,
            [point(0.0, 0.0), point(0.0, 72.0), point(0.0, 0.0)],
            3.0,
            0.0,
        )));
        assert!(!score.plays(Channel::Bounce, 3.0), "a cut never plays");
    }

    #[test]
    fn a_node_never_placed_draws_nothing() {
        let mut score = Score::default();
        score.add(base(0.0, 1.0, 0.0, 1.0, Curve::Linear));
        assert_eq!(score.sample(strip(), 0.5), None);
        assert_eq!(score.along(strip(), 0.5), None);
    }

    #[test]
    #[should_panic(expected = "holds one kind of value")]
    fn a_container_cannot_be_given_a_frame() {
        Track::between(
            strip(),
            Channel::Base,
            rect(0.0, 0.0, 1.0, 1.0),
            rect(1.0, 0.0, 1.0, 1.0),
            0.0,
            1.0,
            Curve::Linear,
        );
    }

    #[test]
    #[should_panic(expected = "one curve per segment")]
    fn every_segment_has_a_curve() {
        Track::keyframes(
            strip(),
            Channel::Base,
            vec![
                (0.0, point(0.0, 0.0)),
                (0.5, point(1.0, 0.0)),
                (1.0, point(0.0, 0.0)),
            ],
            vec![Curve::Linear],
            0.0,
            1.0,
        );
    }

    #[test]
    #[should_panic(expected = "key times run from 0 to 1 in order")]
    fn key_times_run_from_zero_to_one() {
        Track::keyframes(
            strip(),
            Channel::Base,
            vec![(0.0, point(0.0, 0.0)), (0.9, point(1.0, 0.0))],
            vec![Curve::Linear],
            0.0,
            1.0,
        );
    }

    /// A deterministic stream in `[0, 1)`.
    struct Stream(u64);

    impl Stream {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }

        fn within(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * self.next()
        }

        fn curve(&mut self) -> Curve {
            CURVES[(self.next() * 3.0) as usize]
        }

        fn value(&mut self, node: Node) -> Value {
            match node {
                Node::Container(_) => {
                    point(self.within(-3000.0, 3000.0), self.within(-900.0, 900.0))
                }
                Node::Tile(_) => rect(
                    self.within(-3000.0, 3000.0),
                    self.within(0.0, 900.0),
                    self.within(200.0, 1720.0),
                    self.within(200.0, 1081.0),
                ),
            }
        }

        /// A movement to `to`: one curve, or 2 to 40 samples ending there.
        fn movement(&mut self, node: Node, to: Value, begin: f64) -> Track {
            let seconds = self.within(0.05, 0.6);
            if self.next() < 0.5 {
                let from = self.value(node);
                let curve = self.curve();
                Track::between(node, Channel::Base, from, to, begin, seconds, curve)
            } else {
                let count = 2 + (self.next() * 39.0) as usize;
                let mut values: Vec<Value> = (1..count).map(|_| self.value(node)).collect();
                values.push(to);
                Track::samples(node, Channel::Base, values, begin, seconds)
            }
        }

        /// An accent out from rest and home again.
        fn accent(&mut self, node: Node, channel: Channel, begin: f64) -> Track {
            let rest = match node {
                Node::Container(_) => point(0.0, 0.0),
                Node::Tile(_) => rect(0.0, 0.0, 0.0, 0.0),
            };
            let out = match node {
                Node::Container(_) => point(self.within(-600.0, 600.0), self.within(-72.0, 72.0)),
                Node::Tile(_) => rect(self.within(-72.0, 72.0), 0.0, 0.0, 0.0),
            };
            let turn = self.within(0.1, 0.9);
            let curves = vec![self.curve(), self.curve()];
            let keys = vec![(0.0, rest), (turn, out), (1.0, rest)];
            Track::keyframes(node, channel, keys, curves, begin, self.within(0.1, 0.5))
        }
    }

    /// A movement retargeted at `now` from where the score has the node continues from exactly
    /// where it was drawn, whatever was playing, accents included: a merge can bend a flight from
    /// the score and never read Core Animation back. 2000 cases from a fixed seed.
    #[test]
    fn a_movement_retargeted_from_the_score_is_drawn_without_a_jump() {
        const NEAR: f64 = 1e-12;
        let mut stream = Stream(0x5c0e);
        for case in 0..2000 {
            let node = if stream.next() < 0.7 { strip() } else { tile() };
            let to = stream.value(node);
            let mut score = Score::default();
            score.place(node, to);
            let begin = stream.within(0.0, 1.0);
            score.add(stream.movement(node, to, begin));
            for channel in [Channel::Bounce, Channel::Nudge] {
                if stream.next() < 0.5 {
                    let begin = stream.within(0.0, 1.0);
                    score.add(stream.accent(node, channel, begin));
                }
            }
            let now = if stream.next() < 0.8 {
                begin + stream.within(0.0, 0.6)
            } else {
                stream.within(-0.2, 1.8)
            };
            let drawn = score.sample(node, now).unwrap();
            let before = score.sample(node, now - NEAR).unwrap();

            let from = score.along(node, now).unwrap();
            let target = stream.value(node);
            let (seconds, curve) = (stream.within(0.05, 0.6), stream.curve());
            let mut bent = score.clone();
            bent.place(node, target);
            bent.add(Track::between(
                node,
                Channel::Base,
                from,
                target,
                now,
                seconds,
                curve,
            ));
            let at_now = bent.sample(node, now).unwrap();
            let after = bent.sample(node, now + NEAR).unwrap();
            assert!(gap(at_now, drawn) < 1e-9, "case {case}: {at_now:?} vs {drawn:?}");
            assert!(gap(after, before) < 1e-3, "case {case}: {before:?} -> {after:?}");
        }
    }

    /// Begun from what is drawn rather than where the movement has the node, the next movement
    /// carries each accent riding it a second time: the drawn position jumps by exactly the accent.
    #[test]
    fn a_movement_begun_where_an_accent_draws_the_node_carries_it_twice() {
        let mut score = moving(0.0, -861.0, 0.0, 0.35, Curve::Bezier(MOTION_CURVE));
        score.add(Track::samples(
            strip(),
            Channel::Nudge,
            [point(0.0, 0.0), point(287.0, 0.0), point(0.0, 0.0)],
            0.0,
            0.455,
        ));
        let now = 0.2;
        let drawn = x(score.sample(strip(), now));
        let mut bent = score.clone();
        bent.add(base(drawn, -1722.0, now, 0.35, Curve::Linear));
        let riding = x(score.track(strip(), Channel::Nudge).unwrap().at(now));
        assert!(riding > 200.0, "{riding}");
        assert!((x(bent.sample(strip(), now)) - (drawn + riding)).abs() < 1e-9);
    }
}
