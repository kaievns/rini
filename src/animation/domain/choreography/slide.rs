//! Slide, the default look: every rigid piece slides to where its pass sends it in one movement, on
//! the motion curve, or on the ease-in-out while the strip steps; a pass arriving mid-flight bends
//! each container from where the score draws its movement, as fast as its leg is going. Everything
//! is placed on whole points, as the window server places the real windows. See "The overlay engine"
//! and "One flight however many presses" in `src/animation/docs/animation-smoothness.md`.

use std::collections::HashMap;

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::WindowId;
use rini_geometry::{Round, SameAs};

use super::{Pass, Retarget, Stage};
use crate::animation::domain::motion::accents::{Accent, push_tracks, step_tracks};
use crate::animation::domain::motion::easing::MOTION_CURVE;
use crate::animation::domain::motion::glide::{Leg, spring_omega};
use crate::animation::domain::motion::nudge::{NUDGE_STRETCH, flight_curve};
use crate::animation::domain::motion::plan::{
    AnimationTarget, FlightPlan, GroupKey, Member, Presented, animation_targets, group_relative,
    merge_plans, stale_overlay_layers,
};
use crate::animation::domain::motion::score::{
    Channel, Curve, Edit, Node, SAMPLE_STEP, Score, Track, Value,
};

/// The default look, and the legs its containers are on.
#[derive(Debug, Default)]
pub struct Slide {
    /// Each moving container's current leg, whose velocity a new destination carries on at.
    legs: HashMap<GroupKey, Leg>,
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
    fn container(&self, key: GroupKey) -> Option<CGPoint> {
        match self.score.model(Node::Container(key)) {
            Some(Value::Point(position)) => Some(position),
            _ => None,
        }
    }

    /// The container `key`, opened at `at` unless the score has it.
    fn open(&mut self, key: GroupKey, at: CGPoint) {
        if self.container(key).is_none() {
            self.push(Edit::Place(Node::Container(key), Value::Point(at)));
        }
    }

    /// `window`'s tile placed at `to` in its container and carried there from `from` on
    /// `MOTION_CURVE`, from `begin` for `seconds`; one of no length is placed only.
    fn move_tile(&mut self, window: WindowId, from: CGRect, to: CGRect, begin: f64, seconds: f64) {
        let node = Node::Tile(window);
        self.push(Edit::Place(node, Value::Rect(to)));
        if seconds > 0.0 {
            self.push(Edit::Add(Track::between(
                node,
                Channel::Base,
                Value::Rect(from),
                Value::Rect(to),
                begin,
                seconds,
                Curve::Bezier(MOTION_CURVE),
            )));
        }
    }
}

/// The step an accent asks for, if any.
fn step_of<'a>(accents: impl IntoIterator<Item = &'a Accent>) -> Option<CGPoint> {
    accents.into_iter().find_map(|accent| match accent {
        Accent::Step { offset } => Some(*offset),
        Accent::Push { .. } => None,
    })
}

impl Slide {
    /// A pass folded into a flight not yet moving: by `merge_plans`, every container drawn where
    /// frame zero installs it and its movement taken to be where the last pass sends it. Nothing has
    /// moved, and a pass's frames are places on the strip there.
    pub fn fold(&self, plan: &FlightPlan, pass: &Pass) -> FlightPlan {
        let presented = Presented {
            drawn: plan.positions.keys().map(|key| (*key, CGPoint::new(0.0, 0.0))).collect(),
            along: plan.positions.clone(),
        };
        merge_plans(plan, pass.plan, pass.pan, &presented, pass.focus, pass.viewport).0
    }

    /// Frame zero, nothing playing: every container with a drawn member at the origin, each rigid
    /// member at its frame in it, and every loose and floating tile where it starts.
    pub fn pose(&self, stage: &Stage) -> Score {
        let plan = stage.plan;
        let mut score = Score::default();
        let mut put = |key: GroupKey, members: Vec<(WindowId, CGRect)>| {
            let members: Vec<(WindowId, CGRect)> =
                members.into_iter().filter(|(window, _)| stage.drawn.contains(window)).collect();
            if members.is_empty() {
                return;
            }
            score.apply(&Edit::Place(
                Node::Container(key),
                Value::Point(CGPoint::new(0.0, 0.0)),
            ));
            for (window, frame) in members {
                score.apply(&Edit::Ride(window, key));
                score.apply(&Edit::Place(Node::Tile(window), Value::Rect(frame.round())));
            }
        };
        for group in &plan.groups {
            put(
                group.key,
                group.members.iter().map(|m| (m.window, m.rel)).collect(),
            );
        }
        let starts = |list: &[(WindowId, CGRect, CGRect)]| -> Vec<(WindowId, CGRect)> {
            list.iter().map(|&(window, from, _)| (window, from)).collect()
        };
        let mut loose = starts(&plan.changing);
        loose.extend(starts(&plan.entrances));
        put(GroupKey::Loose, loose);
        put(GroupKey::Floating, starts(&plan.floating));
        score
    }

    /// The motion from frame zero, from `begin`: exactly what `animation_targets` names, every
    /// container on one curve over one length, every loose tile on `MOTION_CURVE` in its container,
    /// then the accents that waited. A step taken stretches the flight by `NUDGE_STRETCH` and flies
    /// it on the ease-in-out; a flight of no length is placed only.
    pub fn compose(&mut self, playing: &Score, stage: &Stage, begin: f64) -> Vec<Edit> {
        let step = step_of(stage.accents).filter(|_| !playing.plays(Channel::Nudge, begin));
        let seconds = if step.is_some() {
            stage.seconds * NUDGE_STRETCH
        } else {
            stage.seconds
        };
        let curve = flight_curve(step.is_some());
        let mut making = Making::from(playing);
        for target in animation_targets(stage.plan) {
            match target {
                AnimationTarget::Container { key, from, to } => {
                    if making.container(key).is_none() {
                        continue;
                    }
                    let (from, to) = (from.round(), to.round());
                    let leg = Leg::Curve {
                        from,
                        to,
                        begin,
                        seconds,
                        curve,
                    };
                    making.push(Edit::Place(Node::Container(key), Value::Point(to)));
                    if seconds > 0.0 {
                        making.push(Edit::Add(leg.track(key, SAMPLE_STEP)));
                    }
                    self.record(key, leg);
                }
                AnimationTarget::Tile { window, from, to } => {
                    let Some(offset) =
                        making.score.parent(window).and_then(|key| making.container(key))
                    else {
                        continue;
                    };
                    let from = group_relative(from, offset).round();
                    let to = group_relative(to, offset).round();
                    making.move_tile(window, from, to, begin, seconds);
                }
            }
        }
        if let Some(offset) = step {
            for track in step_tracks(&making.score, offset, seconds, begin) {
                making.push(Edit::Add(track));
            }
        }
        for accent in stage.accents {
            if let Accent::Push { wall, seconds } = *accent {
                for track in push_tracks(&making.score, wall, seconds, begin) {
                    making.push(Edit::Add(track));
                }
            }
        }
        making.edits
    }

    /// A later pass folded into a flight that moves, at `now`: `merge_plans` over where the score
    /// draws every container, then its delta as edits, in this order. The containers a merge opens
    /// at where they install, a newcomer that opened one in it; members moving container placed in
    /// their new one where they start; every retargeted container bent from where the score has its
    /// movement on a spring as fast as its leg is going; a container opened for a member leaving
    /// another carrying on that one's leg, and one opened for a newcomer on `MOTION_CURVE`; tiles
    /// joining; loose tiles bent from where the score draws them; what the flight no longer draws
    /// dropped; and a step the pass brings, taken only while none plays, which stretches every leg
    /// of the merge by `NUDGE_STRETCH`.
    pub fn retarget(
        &mut self,
        playing: &Score,
        plan: &FlightPlan,
        pass: &Pass,
        now: f64,
    ) -> Retarget {
        let (merged, delta) = merge_plans(
            plan,
            pass.plan,
            pass.pan,
            &playing.presented(now),
            pass.focus,
            pass.viewport,
        );
        let step = step_of(pass.accent.as_ref()).filter(|_| !playing.plays(Channel::Nudge, now));
        let seconds = if step.is_some() {
            pass.seconds * NUDGE_STRETCH
        } else {
            pass.seconds
        };
        let omega = spring_omega(seconds);
        let drawn = |window: WindowId| pass.drawn.contains(&window);
        let position = |key: GroupKey| merged.positions.get(&key).copied();
        let origin = CGPoint::new(0.0, 0.0);
        let mut making = Making::from(playing);

        // Read before any leg is bent: a container opened for a member leaving another goes on as
        // fast as that one was going.
        let carried: HashMap<GroupKey, Leg> = delta
            .new_groups
            .iter()
            .filter_map(|group| Some((group.key, *self.legs.get(&group.leaving?)?)))
            .collect();
        for new in &delta.new_groups {
            making.push(Edit::Place(
                Node::Container(new.key),
                Value::Point(new.install.round()),
            ));
            self.legs.remove(&new.key);
            let Some(group) = merged.groups.iter().find(|g| g.key == new.key) else {
                continue;
            };
            for member in &group.members {
                let moved_in =
                    delta.reparented.iter().any(|(w, _, to)| *w == member.window && *to == new.key);
                if moved_in || !drawn(member.window) {
                    continue;
                }
                making.push(Edit::Ride(member.window, new.key));
                making.push(Edit::Place(
                    Node::Tile(member.window),
                    Value::Rect(member.rel.round()),
                ));
            }
        }

        // A member moving container starts at its plan frame, which its tile sits a rounding from:
        // it keeps the tile's, or it jumps by up to a point.
        let mut placed: Vec<WindowId> = Vec::new();
        for &(window, _, to_key) in &delta.reparented {
            let Some(frame) = merged.member(window).map(|member| member.start_frame()) else {
                continue;
            };
            let (Some(Member::Rigid { rel, .. }), Some(Value::Rect(drawn))) =
                (plan.member(window), making.score.model(Node::Tile(window)))
            else {
                continue;
            };
            let frame = CGRect::new(
                CGPoint::new(
                    frame.origin.x + drawn.origin.x - rel.origin.x,
                    frame.origin.y + drawn.origin.y - rel.origin.y,
                ),
                drawn.size,
            );
            making.open(to_key, position(to_key).unwrap_or(origin));
            making.push(Edit::Ride(window, to_key));
            making.push(Edit::Place(Node::Tile(window), Value::Rect(frame.round())));
            if making.score.track(Node::Tile(window), Channel::Base).is_some() {
                making.push(Edit::Cut(Node::Tile(window), Channel::Base));
            }
            placed.push(window);
        }

        for &(key, to) in &delta.retargeted_groups {
            let Some(Value::Point(along)) = making.score.along(Node::Container(key), now) else {
                continue;
            };
            let to = to.round();
            let leg = Leg::toward(self.legs.get(&key), along, to, now, omega);
            making.push(Edit::Place(Node::Container(key), Value::Point(to)));
            if seconds <= 0.0 {
                self.legs.remove(&key);
                continue;
            }
            making.push(Edit::Add(leg.track(key, SAMPLE_STEP)));
            self.legs.insert(key, leg);
        }
        for new in &delta.new_groups {
            let key = new.key;
            let to = position(key).unwrap_or(new.install).round();
            let install = new.install.round();
            making.push(Edit::Place(Node::Container(key), Value::Point(to)));
            if seconds <= 0.0 || install.same_as(to) {
                self.legs.remove(&key);
                continue;
            }
            let leg = match new.leaving {
                Some(_) => Leg::toward(carried.get(&key), install, to, now, omega),
                None => Leg::Curve {
                    from: install,
                    to,
                    begin: now,
                    seconds,
                    curve: MOTION_CURVE,
                },
            };
            making.push(Edit::Add(leg.track(key, SAMPLE_STEP)));
            self.record(key, leg);
        }

        for &(window, key) in &delta.joined_tiles {
            if !drawn(window) {
                continue;
            }
            let Some(member) = merged.member(window) else { continue };
            making.open(key, position(key).unwrap_or(origin));
            making.push(Edit::Ride(window, key));
            match member {
                Member::Rigid { rel, .. } => {
                    making.push(Edit::Place(Node::Tile(window), Value::Rect(rel.round())))
                }
                Member::Changing { from, to }
                | Member::Entrance { from, to }
                | Member::Floating { from, to } => {
                    making.move_tile(window, from.round(), to.round(), now, seconds)
                }
            }
        }

        for &(window, to) in &delta.retargeted_tiles {
            if !drawn(window) {
                continue;
            }
            let node = Node::Tile(window);
            let from = if placed.contains(&window) {
                making.score.model(node)
            } else {
                making.score.sample(node, now)
            };
            let Some(Value::Rect(from)) = from else { continue };
            making.move_tile(window, from, to.round(), now, seconds);
        }

        let tiles: Vec<(WindowId, Option<GroupKey>)> =
            making.score.tiles().map(|(window, key)| (window, Some(key))).collect();
        let containers: Vec<GroupKey> = making.score.containers().collect();
        let (stale, empty) = stale_overlay_layers(&tiles, pass.drawn, &containers);
        for window in stale {
            making.push(Edit::Drop(Node::Tile(window)));
        }
        for key in empty {
            making.push(Edit::Drop(Node::Container(key)));
            self.legs.remove(&key);
        }

        if let Some(offset) = step {
            for track in step_tracks(&making.score, offset, seconds, now) {
                making.push(Edit::Add(track));
            }
        }
        Retarget {
            plan: merged,
            delta,
            edits: making.edits,
        }
    }

    /// A push against `wall` at `now`, on every container that carries it, each from where its own
    /// bounce is drawn.
    pub fn push(&self, playing: &Score, wall: CGPoint, seconds: f64, now: f64) -> Vec<Edit> {
        push_tracks(playing, wall, seconds, now).into_iter().map(Edit::Add).collect()
    }

    /// Keeps the leg a container was just given, or forgets it when it was placed outright.
    fn record(&mut self, key: GroupKey, leg: Leg) {
        let placed = match leg {
            Leg::Curve { from, to, seconds, .. } => seconds == 0.0 || from.same_as(to),
            Leg::Spring { .. } => false,
        };
        if placed {
            self.legs.remove(&key);
        } else {
            self.legs.insert(key, leg);
        }
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;
    use crate::animation::domain::motion::easing::EASE_IN_OUT;
    use crate::animation::domain::motion::nudge::nudge_track;
    use crate::animation::domain::motion::plan::{
        Banding, ReflowPlan, entrance_from, overlay_of, reflow_plan,
    };
    use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, random_requests, rect};

    const PLAIN: f64 = 0.35;
    const BEGIN: f64 = 10.0;

    fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

    fn column(i: f64, width: f64) -> CGRect { rect(4.0 + i * 863.0, 32.0, width, 1081.0) }

    fn shifted(frame: CGRect, dx: f64) -> CGRect {
        rect(
            frame.origin.x + dx,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        )
    }

    /// A strip of `count` columns stepping `dx`, from `first`.
    fn strip(first: u32, count: u32, dx: f64) -> Vec<(WindowId, CGRect, CGRect, bool)> {
        (0..count)
            .map(|i| {
                let from = column(i as f64, 859.0);
                (wid(first + i), from, shifted(from, dx), false)
            })
            .collect()
    }

    struct Flight {
        slide: Slide,
        plan: FlightPlan,
        drawn: Vec<WindowId>,
        score: Score,
    }

    impl Flight {
        /// `requests` posed at frame zero, every window drawn.
        fn posed(requests: &[(WindowId, CGRect, CGRect, bool)]) -> Flight {
            let plan = FlightPlan::from(reflow_plan(requests, DISPLAY));
            let drawn = plan.windows();
            let slide = Slide::default();
            let score = slide.pose(&Stage {
                plan: &plan,
                drawn: &drawn,
                seconds: PLAIN,
                accents: &[],
            });
            Flight { slide, plan, drawn, score }
        }

        fn fold(&mut self, edits: &[Edit]) {
            for edit in edits {
                self.score.apply(edit);
            }
        }

        /// The motion from frame zero at `BEGIN`, with `accents` waiting for it.
        fn start(&mut self, seconds: f64, accents: &[Accent]) -> Vec<Edit> {
            let stage = Stage {
                plan: &self.plan,
                drawn: &self.drawn,
                seconds,
                accents,
            };
            let edits = self.slide.compose(&self.score, &stage, BEGIN);
            self.fold(&edits);
            edits
        }

        /// `incoming` merged at `now`.
        fn merge(
            &mut self,
            incoming: &ReflowPlan,
            pan: Option<CGPoint>,
            accent: Option<Accent>,
            now: f64,
        ) -> Retarget {
            let pass = Pass {
                plan: incoming,
                pan,
                focus: None,
                viewport: DISPLAY,
                drawn: &self.drawn,
                seconds: PLAIN,
                accent,
            };
            let retarget = self.slide.retarget(&self.score, &self.plan, &pass, now);
            self.fold(&retarget.edits);
            self.plan = retarget.plan.clone();
            retarget
        }
    }

    fn adds(edits: &[Edit]) -> Vec<&Track> {
        edits
            .iter()
            .filter_map(|edit| match edit {
                Edit::Add(track) => Some(track),
                _ => None,
            })
            .collect()
    }

    fn places(edits: &[Edit]) -> Vec<(Node, Value)> {
        edits
            .iter()
            .filter_map(|edit| match edit {
                Edit::Place(node, value) => Some((*node, *value)),
                _ => None,
            })
            .collect()
    }

    fn track_on(edits: &[Edit], node: Node, channel: Channel) -> Option<Track> {
        adds(edits)
            .into_iter()
            .find(|t| t.node() == node && t.channel() == channel)
            .cloned()
    }

    fn point(value: Option<Value>) -> CGPoint {
        match value {
            Some(Value::Point(point)) => point,
            other => panic!("a container holds a point: {other:?}"),
        }
    }

    fn key_of(plan: &FlightPlan, window: WindowId) -> GroupKey {
        match plan.member(window).expect("in the plan") {
            Member::Rigid { key, .. } => key,
            Member::Changing { .. } | Member::Entrance { .. } => GroupKey::Loose,
            Member::Floating { .. } => GroupKey::Floating,
        }
    }

    /// A strip moving, a resize, a floating window moving and one standing, and a still window.
    fn mixed() -> Vec<(WindowId, CGRect, CGRect, bool)> {
        let mut requests = strip(1, 2, -863.0);
        requests.push((wid(3), column(2.0, 859.0), column(2.0, 859.0), false));
        requests.push((wid(4), column(3.0, 400.0), column(3.0, 600.0), false));
        let floating = rect(224.0, 95.0, 600.0, 400.0);
        requests.push((wid(5), floating, shifted(floating, 40.0), true));
        requests.push((
            wid(6),
            rect(300.0, 500.0, 300.0, 200.0),
            rect(300.0, 500.0, 300.0, 200.0),
            true,
        ));
        requests
    }

    /// Per target `animation_targets` names, one `Base` track and nothing else: a container's the
    /// curve leg's track between whole points, a loose or floating tile's `Track::between` in its
    /// container on whole points, every model at its target's end. A still window and a floating
    /// window standing play nothing.
    #[test]
    fn a_flight_composes_exactly_what_moves() {
        let mut flight = Flight::posed(&mixed());
        let edits = flight.start(PLAIN, &[]);
        let targets = animation_targets(&flight.plan);
        assert_eq!(adds(&edits).len(), targets.len(), "{edits:?}");
        assert_eq!(places(&edits).len(), targets.len());
        for target in targets {
            match target {
                AnimationTarget::Container { key, from, to } => {
                    let leg = Leg::Curve {
                        from: from.round(),
                        to: to.round(),
                        begin: BEGIN,
                        seconds: PLAIN,
                        curve: MOTION_CURVE,
                    };
                    let node = Node::Container(key);
                    assert_eq!(
                        track_on(&edits, node, Channel::Base),
                        Some(leg.track(key, SAMPLE_STEP))
                    );
                    assert_eq!(flight.score.model(node), Some(Value::Point(to.round())));
                }
                AnimationTarget::Tile { window, from, to } => {
                    let key = flight.score.parent(window).expect("posed");
                    let at = point(flight.score.model(Node::Container(key)));
                    let (from, to) =
                        (group_relative(from, at).round(), group_relative(to, at).round());
                    let node = Node::Tile(window);
                    let between = Track::between(
                        node,
                        Channel::Base,
                        Value::Rect(from),
                        Value::Rect(to),
                        BEGIN,
                        PLAIN,
                        Curve::Bezier(MOTION_CURVE),
                    );
                    assert_eq!(track_on(&edits, node, Channel::Base), Some(between));
                    assert_eq!(flight.score.model(node), Some(Value::Rect(to)));
                }
            }
        }
        for still in [wid(3), wid(6)] {
            assert!(
                flight.score.track(Node::Tile(still), Channel::Base).is_none(),
                "{still:?}"
            );
        }
    }

    /// A flight of no length is placed where every target ends and plays nothing.
    #[test]
    fn a_flight_of_no_length_places_everything_and_plays_nothing() {
        let mut flight = Flight::posed(&mixed());
        let edits = flight.start(0.0, &[]);
        assert!(adds(&edits).is_empty(), "{edits:?}");
        assert_eq!(places(&edits).len(), animation_targets(&flight.plan).len());
        assert!(flight.score.settled(BEGIN));
        let key = key_of(&flight.plan, wid(1));
        assert_eq!(
            flight.score.model(Node::Container(key)),
            Some(Value::Point(CGPoint::new(-863.0, 0.0)))
        );
        assert!(flight.slide.legs.is_empty(), "nothing is on a leg");
    }

    /// A step that waited for the motion is taken as it starts: the flight runs `NUDGE_STRETCH` times
    /// its plain length on the ease-in-out, and the step plays over the same on every strip
    /// container, the moved column's and the strip's, and on no floating one.
    #[test]
    fn a_stepping_flight_runs_longer_on_the_ease_in_out_and_steps_every_strip_container() {
        let mut requests = strip(1, 2, -863.0);
        let moved = column(2.0, 859.0);
        requests.push((wid(3), moved, shifted(moved, 863.0), false));
        let floating = rect(224.0, 95.0, 600.0, 400.0);
        requests.push((wid(5), floating, floating, true));
        let mut flight = Flight::posed(&requests);
        let offset = CGPoint::new(-576.0, 0.0);
        let edits = flight.start(PLAIN, &[Accent::Step { offset }]);
        let stretched = PLAIN * NUDGE_STRETCH;
        let mut strips = 0;
        for target in animation_targets(&flight.plan) {
            let AnimationTarget::Container { key, from, to } = target else {
                continue;
            };
            let leg = Leg::Curve {
                from: from.round(),
                to: to.round(),
                begin: BEGIN,
                seconds: stretched,
                curve: EASE_IN_OUT,
            };
            assert_eq!(
                track_on(&edits, Node::Container(key), Channel::Base),
                Some(leg.track(key, SAMPLE_STEP))
            );
            assert_eq!(
                track_on(&edits, Node::Container(key), Channel::Nudge),
                Some(nudge_track(key, offset, BEGIN, stretched, SAMPLE_STEP)),
                "{key:?} steps"
            );
            strips += 1;
        }
        assert_eq!(strips, 2, "the strip and the moved column");
        assert!(flight.score.containers().any(|key| key == GroupKey::Floating));
        assert_eq!(
            track_on(&edits, Node::Container(GroupKey::Floating), Channel::Nudge),
            None
        );
    }

    /// A step a press brings while one plays is not taken: no second step, and the merge's legs keep
    /// their plain length; the step playing goes on as it was.
    #[test]
    fn a_step_while_one_plays_is_not_taken() {
        let mut flight = Flight::posed(&strip(1, 3, -863.0));
        let offset = CGPoint::new(-576.0, 0.0);
        flight.start(PLAIN, &[Accent::Step { offset }]);
        let key = key_of(&flight.plan, wid(1));
        let node = Node::Container(key);
        let playing = flight.score.track(node, Channel::Nudge).cloned();
        let leg = flight.slide.legs[&key];
        let now = BEGIN + 0.1;
        let along = point(flight.score.along(node, now));
        let pan = CGPoint::new(-863.0, 0.0);
        let incoming = reflow_plan(
            &strip(1, 3, -1726.0)
                .into_iter()
                .map(|(w, _, to, f)| (w, shifted(to, 863.0), to, f))
                .collect::<Vec<_>>(),
            DISPLAY,
        );
        let bent = flight.merge(&incoming, Some(pan), Some(Accent::Step { offset }), now);
        assert_eq!(
            track_on(&bent.edits, node, Channel::Nudge),
            None,
            "no second step"
        );
        assert_eq!(flight.score.track(node, Channel::Nudge).cloned(), playing);
        let to = point(flight.score.model(node));
        let plain = Leg::toward(Some(&leg), along, to, now, spring_omega(PLAIN));
        assert_eq!(
            track_on(&bent.edits, node, Channel::Base),
            Some(plain.track(key, SAMPLE_STEP))
        );
    }

    /// A merge bends a container from where the score draws its movement, as fast as its leg is
    /// going: on the first leg, a curve, that is the leg's own position to 1e-9, so a first merge
    /// bends as it always did; on a spring it is the polyline Core Animation draws, never more than
    /// its chord, 10.8pt, from the spring's own position.
    #[test]
    fn a_bent_container_leaves_from_where_the_score_draws_it_as_fast_as_its_leg() {
        let mut flight = Flight::posed(&strip(1, 3, -861.0));
        flight.start(PLAIN, &[]);
        let key = key_of(&flight.plan, wid(1));
        let node = Node::Container(key);
        let omega = spring_omega(PLAIN);
        let mut destination = -861.0;
        for (press, now) in
            [BEGIN + 0.15, BEGIN + 0.15 + 1.0 / 240.0, BEGIN + 0.2].into_iter().enumerate()
        {
            let leg = flight.slide.legs[&key];
            let along = point(flight.score.along(node, now));
            let gap = (along.x - leg.position_at(now).x).abs();
            match leg {
                Leg::Curve { .. } => assert!(gap < 1e-9, "press {press}: {gap}"),
                Leg::Spring { .. } => assert!(gap <= 10.8, "press {press}: {gap}"),
            }
            destination -= 861.0;
            let requests: Vec<_> = strip(1, 3, 0.0)
                .into_iter()
                .map(|(w, from, _, f)| {
                    (
                        w,
                        shifted(from, destination + 861.0),
                        shifted(from, destination),
                        f,
                    )
                })
                .collect();
            let bent = flight.merge(
                &reflow_plan(&requests, DISPLAY),
                Some(CGPoint::new(-861.0, 0.0)),
                None,
                now,
            );
            let to = CGPoint::new(destination, 0.0);
            let spring = Leg::toward(Some(&leg), along, to, now, omega);
            assert_eq!(
                track_on(&bent.edits, node, Channel::Base),
                Some(spring.track(key, SAMPLE_STEP))
            );
            assert!(
                (spring.position_at(now).x - along.x).abs() < 1e-9,
                "from where it is drawn"
            );
            assert!(
                (spring.velocity_at(now).x - leg.velocity_at(now).x).abs() < 1e-6,
                "as fast"
            );
        }
    }

    /// A member a merge sends elsewhere leaves in a container of its own from where it is drawn, and
    /// that container carries on as fast as the one it left was going.
    #[test]
    fn a_container_opened_for_a_leaving_member_keeps_its_speed() {
        let (first, second) = (column(1.0, 859.0), column(2.0, 859.0));
        let mut flight = Flight::posed(&[
            (wid(1), first, shifted(first, -863.0), false),
            (wid(2), second, shifted(second, -863.0), false),
        ]);
        flight.start(PLAIN, &[]);
        let strip_key = key_of(&flight.plan, wid(1));
        let leg = flight.slide.legs[&strip_key];
        let now = BEGIN + 0.08;
        let before = flight.score.drawn(wid(2), now).expect("drawn");
        let incoming = reflow_plan(
            &[
                (wid(1), first, shifted(first, -863.0), false),
                (wid(2), second, shifted(second, -563.0), false),
            ],
            DISPLAY,
        );
        let bent = flight.merge(&incoming, None, None, now);
        let key = key_of(&flight.plan, wid(2));
        let opened = bent
            .delta
            .new_groups
            .iter()
            .find(|g| g.key == key)
            .expect("a container of its own");
        assert_eq!(opened.leaving, Some(strip_key));
        let to = point(flight.score.model(Node::Container(key)));
        let carried = Leg::toward(Some(&leg), opened.install.round(), to, now, spring_omega(PLAIN));
        assert_eq!(
            track_on(&bent.edits, Node::Container(key), Channel::Base),
            Some(carried.track(key, SAMPLE_STEP))
        );
        let speed = leg.velocity_at(now).x;
        assert!(speed.abs() > 500.0, "the strip was moving: {speed}");
        assert!((carried.velocity_at(now).x - speed).abs() < 1e-6, "no kick");
        let after = flight.score.drawn(wid(2), now).expect("drawn");
        assert!(
            (after.origin.x - before.origin.x).abs() <= 0.5,
            "no jump: {before:?} then {after:?}"
        );
    }

    /// Whatever a pass brings and whenever it lands, every window the flight drew before it is drawn
    /// where it was, within the half point placing on whole points allows: the plan tests' passes,
    /// merged into a flight playing from its score, 200 cases from a fixed seed.
    #[test]
    fn a_bent_flight_is_drawn_without_a_jump() {
        let mut rng = Gen(163);
        for run in 0..RUNS {
            let mut flight = Flight::posed(&random_requests(&mut rng));
            flight.start(PLAIN, &[]);
            let mut now = BEGIN;
            for step in 0..1 + rng.below(4) {
                let tag = format!("seed 163 run {run} step {step}");
                now += rng.pt(10.0, 400.0) / 1000.0;
                let windows = flight.plan.windows();
                let position = |plan: &FlightPlan, key: GroupKey| plan.position_of(key);
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
                        let pieces = reflow_plan(&requests, DISPLAY);
                        p.groups = pieces.groups;
                        p
                    }
                    _ => {
                        let v1 = CGPoint::new(rng.pt(-9.0, 9.0) * 50.0, 0.0);
                        let v2 = CGPoint::new(rng.pt(-9.0, 9.0) * 50.0, rng.pt(-2.0, 2.0) * 50.0);
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
                            let newcomer = wid(500 + step as u32);
                            let from = rng.on_screen();
                            requests.push((newcomer, from, shifted(from, v1.x), false));
                            flight.drawn.push(newcomer);
                        }
                        reflow_plan(&requests, DISPLAY)
                    }
                };
                let before: Vec<(WindowId, CGRect)> = windows
                    .iter()
                    .filter_map(|w| Some((*w, flight.score.drawn(*w, now)?)))
                    .collect();
                flight.merge(&incoming, pan, None, now);
                for (window, was) in before {
                    let is = flight
                        .score
                        .drawn(window, now)
                        .unwrap_or_else(|| panic!("{tag}: {window:?} gone"));
                    let gap = [
                        is.origin.x - was.origin.x,
                        is.origin.y - was.origin.y,
                        is.size.width - was.size.width,
                        is.size.height - was.size.height,
                    ]
                    .into_iter()
                    .fold(0.0f64, |worst, d| worst.max(d.abs()));
                    assert!(
                        gap <= 0.5 + 1e-6,
                        "{tag}: {window:?} jumped {gap}: {was:?} then {is:?}"
                    );
                }
            }
        }
    }

    /// A loose tile a pass bends starts its new track where the score draws it, mid-resize, so it
    /// goes on from there.
    #[test]
    fn a_retargeted_tile_starts_where_the_score_draws_it() {
        let from = column(0.0, 400.0);
        let mut flight = Flight::posed(&[(wid(1), from, column(0.0, 800.0), false)]);
        flight.start(PLAIN, &[]);
        let now = BEGIN + 0.05;
        let node = Node::Tile(wid(1));
        let drawn = flight.score.sample(node, now).expect("drawn");
        let incoming = reflow_plan(&[(wid(1), from, column(0.0, 1200.0), false)], DISPLAY);
        let bent = flight.merge(&incoming, None, None, now);
        let track = track_on(&bent.edits, node, Channel::Base).expect("bent");
        assert_eq!(track.keys()[0].1, drawn);
        assert_eq!(track.begin(), now);
        assert_eq!(track.keys()[1].1, Value::Rect(column(0.0, 1200.0)));
        assert_eq!(flight.score.sample(node, now), Some(drawn));
    }

    /// A window entering once the motion runs joins the loose container on a track of its own,
    /// growing from no width over what is left of the flight, and nothing else moves.
    #[test]
    fn a_late_entrance_joins_on_a_track_of_its_own() {
        let mut flight = Flight::posed(&strip(1, 2, -863.0));
        flight.start(PLAIN, &[]);
        let slot = column(2.0, 859.0);
        let mut entrance = ReflowPlan::empty();
        entrance.entrances.push((wid(9), entrance_from(slot), slot));
        flight.drawn.push(wid(9));
        let now = BEGIN + 0.2;
        let pass = Pass {
            plan: &entrance,
            pan: None,
            focus: None,
            viewport: DISPLAY,
            drawn: &flight.drawn,
            seconds: 0.15,
            accent: None,
        };
        let bent = flight.slide.retarget(&flight.score, &flight.plan, &pass, now);
        let node = Node::Tile(wid(9));
        assert_eq!(bent.edits, [
            Edit::Place(
                Node::Container(GroupKey::Loose),
                Value::Point(CGPoint::new(0.0, 0.0))
            ),
            Edit::Ride(wid(9), GroupKey::Loose),
            Edit::Place(node, Value::Rect(slot)),
            Edit::Add(Track::between(
                node,
                Channel::Base,
                Value::Rect(entrance_from(slot)),
                Value::Rect(slot),
                now,
                0.15,
                Curve::Bezier(MOTION_CURVE),
            )),
        ]);
        assert_eq!(bent.delta.joined_tiles, [(wid(9), GroupKey::Loose)]);
    }

    /// A window the flight no longer draws leaves the score with its tracks, and so does a container
    /// it leaves empty, with its leg.
    #[test]
    fn a_dropped_window_and_the_container_it_emptied_leave_the_score() {
        let mut requests = strip(1, 2, -863.0);
        let alone = column(2.0, 859.0);
        requests.push((wid(3), alone, shifted(alone, 300.0), false));
        let mut flight = Flight::posed(&requests);
        flight.start(PLAIN, &[]);
        let key = key_of(&flight.plan, wid(3));
        assert!(flight.slide.legs.contains_key(&key));
        flight.drawn.retain(|w| *w != wid(3));
        let bent = flight.merge(&ReflowPlan::empty(), None, None, BEGIN + 0.1);
        assert!(bent.edits.contains(&Edit::Drop(Node::Tile(wid(3)))));
        assert!(bent.edits.contains(&Edit::Drop(Node::Container(key))));
        assert_eq!(flight.score.parent(wid(3)), None);
        assert_eq!(flight.score.model(Node::Container(key)), None);
        assert_eq!(flight.score.track(Node::Container(key), Channel::Base), None);
        assert!(!flight.slide.legs.contains_key(&key));
        assert!(flight.score.parent(wid(1)).is_some(), "the rest stays");
    }

    /// The accents that waited begin with the motion, after every movement in the order they are
    /// added: a push's bounce on every container that carries it, a step's on every strip container.
    #[test]
    fn an_accent_waiting_for_the_motion_begins_with_it() {
        let mut flight = Flight::posed(&strip(1, 2, -863.0));
        let (wall, offset) = (CGPoint::new(0.0, 72.0), CGPoint::new(-576.0, 0.0));
        let edits = flight.start(PLAIN, &[Accent::Push { wall, seconds: 0.35 }, Accent::Step {
            offset,
        }]);
        let key = key_of(&flight.plan, wid(1));
        let tracks = adds(&edits);
        let channels: Vec<Channel> = tracks.iter().map(|t| t.channel()).collect();
        assert_eq!(channels, [Channel::Base, Channel::Nudge, Channel::Bounce]);
        for track in &tracks {
            assert_eq!(track.begin(), BEGIN, "{:?}", track.channel());
            assert_eq!(track.node(), Node::Container(key));
        }
        let bounce = crate::animation::domain::motion::bounce::Bounce::from_rest(
            wall,
            std::time::Duration::from_secs_f64(0.35),
        );
        assert_eq!(*tracks[2], bounce.track(key, BEGIN));
    }

    /// Layout offsets are fractional and the window server places windows on whole points, so a
    /// tile drawn at the fraction is resampled and pops by it at the lift: every tile and container
    /// is placed and keyed on whole points, and a strip's columns keep their pitch.
    #[test]
    fn tiles_sit_on_whole_points_like_the_real_windows() {
        let a = rect(4.0 - 4592.6724 + 4592.0, 32.0, 859.0, 1081.0);
        let b = rect(a.origin.x + 861.0, 32.0, 861.0, 1081.0);
        let resizing = rect(1730.4, 32.3, 400.0, 1081.0);
        let requests = [
            (wid(1), a, shifted(a, -861.3276), false),
            (wid(2), b, shifted(b, -861.3276), false),
            (wid(3), resizing, rect(1730.4, 32.3, 600.5, 1081.0), false),
        ];
        let mut flight = Flight::posed(&requests);
        let whole = |v: f64| v == v.round();
        let on_whole = |value: Value| match value {
            Value::Point(p) => whole(p.x) && whole(p.y),
            Value::Rect(r) => {
                whole(r.origin.x)
                    && whole(r.origin.y)
                    && whole(r.size.width)
                    && whole(r.size.height)
            }
        };
        let edits = flight.start(PLAIN, &[]);
        for (node, value) in places(&edits) {
            assert!(on_whole(value), "{node:?} at {value:?}");
        }
        for track in adds(&edits) {
            for (_, value) in track.keys() {
                assert!(on_whole(*value), "{:?} keyed at {value:?}", track.node());
            }
        }
        let frame = |w| match flight.score.model(Node::Tile(w)) {
            Some(Value::Rect(r)) => r,
            other => panic!("{other:?}"),
        };
        assert_eq!(frame(wid(1)).origin.x, 3.0);
        assert_eq!(
            frame(wid(2)).origin.x - frame(wid(1)).origin.x,
            861.0,
            "one rounding for the strip"
        );
        assert_eq!(frame(wid(1)).size, CGSize::new(859.0, 1081.0));
        let _ = Banding::default();
    }
}
