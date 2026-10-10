//! Slide, the default look: every rigid piece slides to where its pass sends it in one movement, on
//! the motion curve, or on the ease-in-out while the strip steps; a pass arriving mid-flight bends
//! each container from where the score draws its movement, as fast as its leg is going. Everything
//! is placed on whole points, as the window server places the real windows. A move whose column
//! fills the viewport holds the column it passes and steps the whole strip; any other crosses both.
//! See "The overlay engine" and "One flight however many presses" in
//! `src/animation/docs/animation-smoothness.md`, and "The move flight, staged" in
//! `src/transitions/docs/coordination.md`.

use std::collections::HashMap;

use rini_core::ids::WindowId;
use rini_geometry::space::{GroupRect, OverlayPoint, OverlayVector, Point};
use rini_geometry::{Round, SameAs};
use rini_ipc::protocol::Direction;

use super::{Choreography, Cue, Pass, Retarget, Stage, Staging, Strip};
use crate::animation::domain::motion::accents::{Accent, push_tracks, step_tracks, steps};
use crate::animation::domain::motion::easing::MOTION_CURVE;
use crate::animation::domain::motion::glide::{Leg, spring_omega};
use crate::animation::domain::motion::nudge::{NUDGE_SHARE, NUDGE_STRETCH, flight_curve};
use crate::animation::domain::motion::plan::{
    AnimationTarget, FlightPlan, GroupKey, Member, Presented, ReflowPlan, animation_targets,
    group_relative, merge_plans, on_overlay, stale_overlay_layers,
};
use crate::animation::domain::motion::score::{Channel, Edit, Node, SAMPLE_STEP, Score, Value};
use crate::animation::domain::motion::tile;

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

    /// `window`'s tile placed at `to` in its container and carried there from `from` on
    /// `MOTION_CURVE`, from `begin` for `seconds`, in one movement or, growing it on one axis as it
    /// shrinks it on the other, in two steps (`tile::leg`); one of no length is placed only. Every
    /// tile's leg is made here, a flight's first and a merge's alike.
    fn move_tile(
        &mut self,
        window: WindowId,
        from: GroupRect,
        to: GroupRect,
        begin: f64,
        seconds: f64,
    ) {
        self.push(Edit::Place(Node::Tile(window), Value::Rect(to)));
        if seconds > 0.0 {
            self.push(Edit::Add(tile::leg(window, from, to, begin, seconds, MOTION_CURVE)));
        }
    }
}

/// A column this close to the tiling area's width fills the viewport.
const FILLS_TOLERANCE: f64 = 1.0;

/// What Slide does with a press on the strip. A move whose column fills the viewport steps the whole
/// strip away from where the window went, and the column it passed is held at its start on the
/// strip, riding the strip beneath the moved window; the moved column, and both columns of any other
/// move, cross from where they start.
pub fn stage(cue: &Cue, strip: &Strip) -> Staging {
    let Some((moved, direction)) = cue.moved() else {
        return Staging::default();
    };
    let Some(&(_, at)) = strip.windows.iter().find(|(window, _)| *window == moved) else {
        return Staging::default();
    };
    let area = strip.area;
    let steps = !strip.pair.is_empty()
        && fills_viewport(
            at.size.width,
            area.frame.size.width,
            area.outer_left,
            area.outer_right,
        );
    let mut staging = Staging::default();
    for pair in strip.pair {
        let start = (pair.window, pair.start);
        if steps && pair.passed {
            staging.held.push(start);
        } else {
            staging.from.push(start);
        }
    }
    staging.accent = steps
        .then(|| nudge(area.frame.size.width, direction))
        .flatten()
        .map(|offset| Accent::Step {
            offset,
            seconds: strip.seconds,
        });
    staging
}

/// Whether a column `width` wide fills the viewport: it is the tiling area's width, the display's
/// usable width less both outer gaps, within a point. The camera follows such a column, so nothing
/// on screen says it moved; a narrower one is seen moving.
fn fills_viewport(width: f64, screen_width: f64, outer_left: f64, outer_right: f64) -> bool {
    (width - (screen_width - outer_left - outer_right)).abs() <= FILLS_TOLERANCE
}

/// The step of a move: a share of the viewport away from where the moved window went, so the side
/// it opens is the side the column it passed comes from.
fn nudge(viewport_width: f64, direction: Direction) -> Option<OverlayVector> {
    let step = viewport_width * NUDGE_SHARE;
    match direction {
        Direction::Right => Some(OverlayVector::new(-step, 0.0)),
        Direction::Left => Some(OverlayVector::new(step, 0.0)),
        Direction::Up | Direction::Down => None,
    }
}

/// The step an accent asks for, if any, and its press's plain length.
fn step_of<'a>(accents: impl IntoIterator<Item = &'a Accent>) -> Option<(OverlayVector, f64)> {
    accents.into_iter().find_map(|accent| match *accent {
        Accent::Step { offset, seconds } => Some((offset, seconds)),
        Accent::Push { .. } => None,
    })
}

/// How long legs of `plain` length run with `step` taken: as long as the step, its press's length
/// stretched by `NUDGE_STRETCH`, when that is longer.
fn stepped(plain: f64, step: Option<(OverlayVector, f64)>) -> f64 {
    step.map_or(plain, |(_, seconds)| plain.max(seconds * NUDGE_STRETCH))
}

impl Choreography for Slide {
    /// A fresh flight: every container installs at the origin.
    fn plan(&self, pass: ReflowPlan, _cue: &Cue) -> FlightPlan { FlightPlan::from(pass) }

    /// A pass folded into a flight not yet moving: by `merge_plans`, every container drawn where
    /// frame zero installs it and its movement taken to be where the last pass sends it. Nothing has
    /// moved, and a pass's frames are places on the strip there.
    fn fold(&self, plan: &FlightPlan, pass: &Pass) -> FlightPlan {
        let presented = Presented {
            drawn: plan.positions.keys().map(|key| (*key, OverlayPoint::origin())).collect(),
            along: plan.positions.clone(),
        };
        merge_plans(plan, pass.plan, pass.pan, &presented, pass.focus, pass.viewport).0
    }

    /// Frame zero, nothing playing: every container with a drawn member at the origin, each rigid
    /// member at its frame in it, and every loose and floating tile where it starts.
    fn pose(&self, stage: &Stage) -> Score {
        let plan = stage.plan;
        let mut score = Score::default();
        let mut put = |key: GroupKey, members: Vec<(WindowId, GroupRect)>| {
            let members: Vec<(WindowId, GroupRect)> =
                members.into_iter().filter(|(window, _)| stage.drawn.contains(window)).collect();
            if members.is_empty() {
                return;
            }
            score.apply(&Edit::Place(
                Node::Container(key),
                Value::Point(OverlayPoint::origin()),
            ));
            for (window, frame) in members {
                score.apply(&Edit::Ride(window, key));
                score.apply(&Edit::Place(Node::Tile(window), Value::Rect(frame.rounded())));
            }
        };
        for group in &plan.groups {
            put(
                group.key,
                group.members.iter().map(|m| (m.window, m.rel)).collect(),
            );
        }
        let starts = |list: &[(WindowId, GroupRect, GroupRect)]| -> Vec<(WindowId, GroupRect)> {
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
    /// then the accents that waited. A step taken runs over its own press's length stretched by
    /// `NUDGE_STRETCH`, the flight at least as long, on the ease-in-out; a flight of no length is
    /// placed only.
    fn compose(&mut self, playing: &Score, stage: &Stage, begin: f64) -> Vec<Edit> {
        let step = step_of(stage.accents).filter(|_| steps(playing, begin));
        let seconds = stepped(stage.seconds, step);
        let curve = flight_curve(step.is_some());
        let mut making = Making::from(playing);
        for target in animation_targets(stage.plan) {
            match target {
                AnimationTarget::Container { key, from, to } => {
                    if making.container(key).is_none() {
                        continue;
                    }
                    let (from, to) = (from.rounded(), to.rounded());
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
                    let from = group_relative(on_overlay(from), offset).rounded();
                    let to = group_relative(on_overlay(to), offset).rounded();
                    making.move_tile(window, from, to, begin, seconds);
                }
            }
        }
        if let Some((offset, plain)) = step {
            for track in step_tracks(&making.score, offset, plain * NUDGE_STRETCH, begin) {
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
    /// dropped; and a step the pass brings, taken only while none plays, over its press's length
    /// stretched by `NUDGE_STRETCH`, every leg of the merge at least as long.
    fn retarget(&mut self, playing: &Score, plan: &FlightPlan, pass: &Pass, now: f64) -> Retarget {
        let (merged, delta) = merge_plans(
            plan,
            pass.plan,
            pass.pan,
            &playing.presented(now),
            pass.focus,
            pass.viewport,
        );
        let step = step_of(pass.accent.as_ref()).filter(|_| steps(playing, now));
        let seconds = stepped(pass.seconds, step);
        let omega = spring_omega(seconds);
        let drawn = |window: WindowId| pass.drawn.contains(&window);
        let position = |key: GroupKey| merged.positions.get(&key).copied();
        let origin = OverlayPoint::origin();
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
                Value::Point(new.install.rounded()),
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
                    Value::Rect(member.rel.rounded()),
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
            let frame = GroupRect::new(
                Point::new(
                    frame.origin.x + drawn.origin.x - rel.origin.x,
                    frame.origin.y + drawn.origin.y - rel.origin.y,
                ),
                drawn.size,
            );
            making.open(to_key, position(to_key).unwrap_or(origin));
            making.push(Edit::Ride(window, to_key));
            making.push(Edit::Place(Node::Tile(window), Value::Rect(frame.rounded())));
            if making.score.track(Node::Tile(window), Channel::Base).is_some() {
                making.push(Edit::Cut(Node::Tile(window), Channel::Base));
            }
            placed.push(window);
        }

        for &(key, to) in &delta.retargeted_groups {
            let Some(Value::Point(along)) = making.score.along(Node::Container(key), now) else {
                continue;
            };
            let to = to.rounded();
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
            let to = position(key).unwrap_or(new.install).rounded();
            let install = new.install.rounded();
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
                    making.push(Edit::Place(Node::Tile(window), Value::Rect(rel.rounded())))
                }
                Member::Changing { from, to }
                | Member::Entrance { from, to }
                | Member::Floating { from, to } => {
                    making.move_tile(window, from.rounded(), to.rounded(), now, seconds)
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
            making.move_tile(window, from, to.rounded(), now, seconds);
        }

        let tiles: Vec<(WindowId, GroupKey)> = making.score.tiles().collect();
        let containers: Vec<GroupKey> = making.score.containers().collect();
        let (stale, empty) = stale_overlay_layers(&tiles, pass.drawn, &containers);
        for window in stale {
            making.push(Edit::Drop(Node::Tile(window)));
        }
        for key in empty {
            making.push(Edit::Drop(Node::Container(key)));
            self.legs.remove(&key);
        }

        if let Some((offset, plain)) = step {
            for track in step_tracks(&making.score, offset, plain * NUDGE_STRETCH, now) {
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

impl Slide {
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
    use rini_geometry::space::{Rect, ScreenRect, Size};
    use rini_geometry::to_overlay_space;

    use super::*;
    use crate::animation::domain::motion::easing::EASE_IN_OUT;
    use crate::animation::domain::motion::nudge::nudge_track;
    use crate::animation::domain::motion::plan::{entrance_from, installed, reflow_plan};
    use crate::animation::domain::motion::score::{Curve, Track};
    use crate::animation::domain::motion::testing::{DISPLAY, VIEWPORT, rect};

    const PLAIN: f64 = 0.35;
    const BEGIN: f64 = 10.0;
    const GAP: f64 = 3.0;

    fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

    fn column(i: f64, width: f64) -> ScreenRect { rect(4.0 + i * 863.0, 32.0, width, 1081.0) }

    /// A frame on `DISPLAY`, which sits at the origin, as a fresh plan holds it.
    fn held(frame: ScreenRect) -> GroupRect { installed(to_overlay_space(frame, DISPLAY)) }

    fn shifted<U>(frame: Rect<U>, dx: f64) -> Rect<U> {
        rect(
            frame.origin.x + dx,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        )
    }

    /// A strip of `count` columns stepping `dx`, from `first`.
    fn strip(first: u32, count: u32, dx: f64) -> Vec<(WindowId, ScreenRect, ScreenRect, bool)> {
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
        fn posed(requests: &[(WindowId, ScreenRect, ScreenRect, bool)]) -> Flight {
            let plan = FlightPlan::from(reflow_plan(requests, DISPLAY));
            let drawn = plan.windows();
            let slide = Slide::default();
            let score = slide.pose(&Stage {
                plan: &plan,
                drawn: &drawn,
                seconds: PLAIN,
                cue: Cue::Plain,
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
                cue: Cue::Plain,
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

    fn point(value: Option<Value>) -> OverlayPoint {
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
    fn mixed() -> Vec<(WindowId, ScreenRect, ScreenRect, bool)> {
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
                        from: from.rounded(),
                        to: to.rounded(),
                        begin: BEGIN,
                        seconds: PLAIN,
                        curve: MOTION_CURVE,
                    };
                    let node = Node::Container(key);
                    assert_eq!(
                        track_on(&edits, node, Channel::Base),
                        Some(leg.track(key, SAMPLE_STEP))
                    );
                    assert_eq!(flight.score.model(node), Some(Value::Point(to.rounded())));
                }
                AnimationTarget::Tile { window, from, to } => {
                    let key = flight.score.parent(window).expect("posed");
                    let at = point(flight.score.model(Node::Container(key)));
                    let (from, to) = (
                        group_relative(on_overlay(from), at).rounded(),
                        group_relative(on_overlay(to), at).rounded(),
                    );
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
            Some(Value::Point(OverlayPoint::new(-863.0, 0.0)))
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
        let offset = OverlayVector::new(-576.0, 0.0);
        let edits = flight.start(PLAIN, &[Accent::Step { offset, seconds: PLAIN }]);
        let stretched = PLAIN * NUDGE_STRETCH;
        let mut strips = 0;
        for target in animation_targets(&flight.plan) {
            let AnimationTarget::Container { key, from, to } = target else {
                continue;
            };
            let leg = Leg::Curve {
                from: from.rounded(),
                to: to.rounded(),
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

    /// A step that waited for the motion runs over its own press's length stretched, whatever pass was
    /// folded in after it, and the flight over that or its own length, whichever is longer. Stretched
    /// from the latest pass folded in, a two-row switch folded in after a full-width move ran the
    /// step and the flight 0.64s, where the move's step runs 0.455s.
    #[test]
    fn a_step_that_waited_runs_over_its_own_press_stretched() {
        let offset = OverlayVector::new(-576.0, 0.0);
        let stretched = PLAIN * NUDGE_STRETCH;
        for flies in [0.64, PLAIN, 0.2] {
            let mut flight = Flight::posed(&strip(1, 2, -863.0));
            let edits = flight.start(flies, &[Accent::Step {
                offset,
                seconds: PLAIN,
            }]);
            let node = Node::Container(key_of(&flight.plan, wid(1)));
            let step = track_on(&edits, node, Channel::Nudge).expect("steps");
            assert!((step.seconds() - stretched).abs() < 1e-9, "{flies}: {}", step.seconds());
            let leg = track_on(&edits, node, Channel::Base).expect("flies");
            assert!(
                (leg.seconds() - flies.max(stretched)).abs() < 1e-9,
                "{flies}: {}",
                leg.seconds()
            );
        }
    }

    /// A step a press brings mid-flight while none plays is taken: every strip container steps from
    /// the merge, the floating one not, and every leg of the merge runs `NUDGE_STRETCH` longer, its
    /// spring stiffened to match.
    #[test]
    fn a_step_a_merge_brings_while_none_plays_is_taken_and_stretches_the_merge() {
        let mut requests = strip(1, 3, -863.0);
        let floating = rect(224.0, 95.0, 600.0, 400.0);
        requests.push((wid(5), floating, floating, true));
        let mut flight = Flight::posed(&requests);
        flight.start(PLAIN, &[]);
        let key = key_of(&flight.plan, wid(1));
        let node = Node::Container(key);
        let leg = flight.slide.legs[&key];
        let now = BEGIN + 0.1;
        let along = point(flight.score.along(node, now));
        let offset = OverlayVector::new(-576.0, 0.0);
        let incoming = reflow_plan(
            &strip(1, 3, -1726.0)
                .into_iter()
                .map(|(w, _, to, f)| (w, shifted(to, 863.0), to, f))
                .collect::<Vec<_>>(),
            DISPLAY,
        );
        let pan = OverlayVector::new(-863.0, 0.0);
        let bent = flight.merge(&incoming, Some(pan), Some(Accent::Step { offset, seconds: PLAIN }), now);
        let stretched = PLAIN * NUDGE_STRETCH;
        assert_eq!(
            track_on(&bent.edits, node, Channel::Nudge),
            Some(nudge_track(key, offset, now, stretched, SAMPLE_STEP))
        );
        assert!(flight.score.containers().any(|key| key == GroupKey::Floating));
        assert_eq!(
            track_on(&bent.edits, Node::Container(GroupKey::Floating), Channel::Nudge),
            None
        );
        let to = point(flight.score.model(node));
        let bends = Leg::toward(Some(&leg), along, to, now, spring_omega(stretched));
        assert_eq!(
            track_on(&bent.edits, node, Channel::Base),
            Some(bends.track(key, SAMPLE_STEP))
        );
    }

    /// A step a press brings while one plays is not taken: no second step, and the merge's legs keep
    /// their plain length; the step playing goes on as it was.
    #[test]
    fn a_step_while_one_plays_is_not_taken() {
        let mut flight = Flight::posed(&strip(1, 3, -863.0));
        let offset = OverlayVector::new(-576.0, 0.0);
        flight.start(PLAIN, &[Accent::Step { offset, seconds: PLAIN }]);
        let key = key_of(&flight.plan, wid(1));
        let node = Node::Container(key);
        let playing = flight.score.track(node, Channel::Nudge).cloned();
        let leg = flight.slide.legs[&key];
        let now = BEGIN + 0.1;
        let along = point(flight.score.along(node, now));
        let pan = OverlayVector::new(-863.0, 0.0);
        let incoming = reflow_plan(
            &strip(1, 3, -1726.0)
                .into_iter()
                .map(|(w, _, to, f)| (w, shifted(to, 863.0), to, f))
                .collect::<Vec<_>>(),
            DISPLAY,
        );
        let bent = flight.merge(&incoming, Some(pan), Some(Accent::Step { offset, seconds: PLAIN }), now);
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
                Some(OverlayVector::new(-861.0, 0.0)),
                None,
                now,
            );
            let to = OverlayPoint::new(destination, 0.0);
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
        let carried = Leg::toward(Some(&leg), opened.install.rounded(), to, now, spring_omega(PLAIN));
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
        assert_eq!(track.keys()[1].1, Value::Rect(held(column(0.0, 1200.0))));
        assert_eq!(flight.score.sample(node, now), Some(drawn));
    }

    /// fold1's lab-C folding under the window beside it flies its leg in two steps, from where it
    /// starts as the flight's first leg and from where its tile is drawn as a merge unfolding it bends
    /// it: every tile's leg is `tile::leg`'s.
    #[test]
    fn a_fold_flies_in_two_steps_as_composed_and_as_bent() {
        let (column, folded) = (rect(866.0, 35.0, 860.0, 1079.0), rect(3.0, 576.0, 1148.0, 538.0));
        let mut flight = Flight::posed(&[(wid(1), column, folded, false)]);
        let edits = flight.start(PLAIN, &[]);
        let node = Node::Tile(wid(1));
        let track = track_on(&edits, node, Channel::Base).expect("a leg");
        assert_eq!(track, tile::leg(wid(1), held(column), held(folded), BEGIN, PLAIN, MOTION_CURVE));
        assert!(tile::corner_key(&track).is_some(), "in two steps");
        let now = BEGIN + 0.05;
        let Some(Value::Rect(drawn)) = flight.score.sample(node, now) else { panic!("drawn") };
        let unfolded = reflow_plan(&[(wid(1), folded, column, false)], DISPLAY);
        let bent = flight.merge(&unfolded, None, None, now);
        let track = track_on(&bent.edits, node, Channel::Base).expect("bent");
        assert_eq!(track, tile::leg(wid(1), drawn, held(column), now, PLAIN, MOTION_CURVE));
        assert!(tile::corner_key(&track).is_some(), "in two steps from where it is drawn");
    }

    /// A column a second pan brings mid-leg starts beside the column it sits beside in the layout,
    /// wherever the strip is drawn, and lands beside it: the pass places it where the first pan sends
    /// the strip, which the strip is still short of by the travel left.
    #[test]
    fn a_column_a_pan_brings_in_mid_flight_starts_beside_the_strip() {
        let d = -863.0;
        let mut flight = Flight::posed(&strip(1, 3, d));
        flight.start(PLAIN, &[]);
        let now = BEGIN + 0.02;
        let sent = |i: u32| shifted(column(i as f64, 859.0), d);
        let left = flight.score.drawn(wid(3), now).unwrap().origin.x - sent(2).origin.x;
        assert!(left > 100.0, "the strip has {left}pt to go");
        let requests: Vec<(WindowId, ScreenRect, ScreenRect, bool)> =
            (0..4).map(|i| (wid(1 + i), sent(i), shifted(sent(i), d), false)).collect();
        flight.drawn.push(wid(4));
        flight.merge(
            &reflow_plan(&requests, DISPLAY),
            Some(OverlayVector::new(d, 0.0)),
            None,
            now,
        );
        let apart = |at: f64| {
            let drawn = |window| flight.score.drawn(window, at).expect("drawn");
            drawn(wid(4)).origin.x - drawn(wid(3)).origin.x
        };
        assert!((apart(now) - 863.0).abs() <= 0.5, "starts {} from 3", apart(now));
        let end = now + flight.score.remaining(now);
        assert!((apart(end) - 863.0).abs() <= 0.5, "lands {} from 3", apart(end));
        let lands = flight.score.drawn(wid(4), end).unwrap();
        assert!(
            (lands.origin.x - shifted(sent(3), d).origin.x).abs() <= 0.5,
            "{lands:?}"
        );
    }

    /// A window entering once the motion runs joins the loose container on a track of its own,
    /// growing from no width over what is left of the flight, and nothing else moves.
    #[test]
    fn a_late_entrance_joins_on_a_track_of_its_own() {
        let mut flight = Flight::posed(&strip(1, 2, -863.0));
        flight.start(PLAIN, &[]);
        let slot = held(column(2.0, 859.0));
        let mut entrance = ReflowPlan::empty();
        entrance.entrances.push((wid(9), entrance_from(slot), slot));
        flight.drawn.push(wid(9));
        let now = BEGIN + 0.2;
        let pass = Pass {
            plan: &entrance,
            pan: None,
            focus: None,
            viewport: VIEWPORT,
            drawn: &flight.drawn,
            seconds: 0.15,
            cue: Cue::Plain,
            accent: None,
        };
        let bent = flight.slide.retarget(&flight.score, &flight.plan, &pass, now);
        let node = Node::Tile(wid(9));
        assert_eq!(bent.edits, [
            Edit::Place(
                Node::Container(GroupKey::Loose),
                Value::Point(OverlayPoint::new(0.0, 0.0))
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

    /// A window entering once nothing is left of the flight's clock, while the lift waits, is placed
    /// in its slot and plays nothing.
    #[test]
    fn a_late_entrance_with_no_flight_left_is_placed_in_its_slot() {
        let mut flight = Flight::posed(&strip(1, 2, -863.0));
        flight.start(PLAIN, &[]);
        let slot = held(column(2.0, 859.0));
        let mut entrance = ReflowPlan::empty();
        entrance.entrances.push((wid(9), entrance_from(slot), slot));
        flight.drawn.push(wid(9));
        let now = BEGIN + PLAIN + 0.1;
        let pass = Pass {
            plan: &entrance,
            pan: None,
            focus: None,
            viewport: VIEWPORT,
            drawn: &flight.drawn,
            seconds: 0.0,
            cue: Cue::Plain,
            accent: None,
        };
        let bent = flight.slide.retarget(&flight.score, &flight.plan, &pass, now);
        assert_eq!(bent.edits, [
            Edit::Place(
                Node::Container(GroupKey::Loose),
                Value::Point(OverlayPoint::new(0.0, 0.0))
            ),
            Edit::Ride(wid(9), GroupKey::Loose),
            Edit::Place(Node::Tile(wid(9)), Value::Rect(slot)),
        ]);
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
        let (wall, offset) = (OverlayVector::new(0.0, 72.0), OverlayVector::new(-576.0, 0.0));
        let edits = flight.start(PLAIN, &[Accent::Push { wall, seconds: 0.35 }, Accent::Step {
            offset,
            seconds: PLAIN,
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
        assert_eq!(frame(wid(1)).size, Size::new(859.0, 1081.0));
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
        assert_eq!(nudge(1728.0, Direction::Right), Some(OverlayVector::new(-576.0, 0.0)));
        assert_eq!(nudge(1728.0, Direction::Left), Some(OverlayVector::new(576.0, 0.0)));
        assert_eq!(nudge(1728.0, Direction::Up), None);
        assert_eq!(nudge(1728.0, Direction::Down), None);
    }
}
