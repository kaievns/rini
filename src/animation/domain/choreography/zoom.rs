//! Zoom: a press along the strip, and a switch through the workspace stack, plays as one arc. The
//! strip zooms out about the display's centre and back in on a sin² bell while every container
//! crosses to where its pass sends it, the travel they all carry eased so the view is where the swap
//! needs it at the arc's middle, and the two columns a move swapped crossing on a curve of their
//! own. A pan merged mid-arc, along or across the strip, folds into the containers' movement and
//! lands with the arc; any other pass bends them on a spring from where the score has them, as fast
//! as they are drawn moving. A push at an end bounces with the strip zoomed out. It plans, folds and
//! poses as Slide does, and composes as Slide does without an arc. See "The zoom look" in
//! `specs/animation.md`, and "The overlay engine" in `src/animation/docs/animation-smoothness.md`.

use rini_core::ids::WindowId;
use rini_geometry::space::{OverlayPoint, OverlayVector, SurfaceRect};
use rini_geometry::{Round, SameAs};

use super::slide::Slide;
use super::{
    Bending, Choreography, ContainerWay, Cue, Making, Merged, Pass, Retarget, Stage, Staging,
    Strip, TileWay,
};
use crate::animation::domain::motion::accents::{Accent, Swap, push_tracks};
use crate::animation::domain::motion::easing::{
    BOUNCE_TURN, CubicBezier, EASE_IN_OUT, MOTION_CURVE, nudge_displacement,
};
use crate::animation::domain::motion::glide::{Leg, spring_omega};
use crate::animation::domain::motion::plan::{
    AnimationTarget, FlightPlan, GroupKey, Member, ReflowPlan, animation_targets, group_relative,
    on_overlay,
};
use crate::animation::domain::motion::score::{
    Channel, Edit, Node, SAMPLE_STEP, Score, Track, Value, Zoom,
};
use crate::animation::domain::motion::tile;

/// An arc's length against its press's plain length: 0.257s at the default 0.35s.
pub const ZOOM_STRETCH: f64 = 2.2 / 3.0;

/// The shallowest an arc goes: a pair that fits the display at it goes no deeper, and a push at an
/// end or a move merged mid-flight goes this deep.
pub const STRIP_ZOOM: f64 = 0.88;

/// The deepest an arc goes, however wide the pair.
pub const ZOOM_DEEPEST: f64 = 0.7;

/// How far apart, beyond the usable height, the strips above and below the one a flight moves are
/// drawn, in points at rest: 24pt, 21pt on screen at `STRIP_ZOOM`, between their windows' margins.
pub const STACK_GAP: f64 = 24.0;

/// The share of the pair's narrower column kept in view at the arc's middle.
const NARROWER_SHOWN: f64 = 2.0 / 3.0;

/// Where the arc is deepest, as its progress: the swap is half done there.
pub const ARC_MIDDLE: f64 = 0.5;

/// The shortest share of an arc a pan folded into it plays over.
const FOLDED_PAN_MIN: f64 = 0.25;

/// Below this speed against a pan, in points per unit of the arc's progress, the travel the
/// containers carry does not turn back under it, and the pan folds in.
const CAMERA_STILL_UNDER: f64 = 1.0;

/// The curve a move's two columns cross on: at rest as the arc leaves and lands, half way at its
/// middle.
pub const ZOOM_CURVE: CubicBezier = CubicBezier {
    x1: 0.65,
    y1: 0.0,
    x2: 0.35,
    y2: 1.0,
};

/// Frames this close in x are one column.
const COLUMN_TOLERANCE: f64 = 0.5;

/// The strip's scale at progress `f` of an arc out to `depth`: `1 - (1 - depth) sin²(πf)`, leaving
/// and landing at rest, deepest at `ARC_MIDDLE`, never still between.
pub fn arc_scale(depth: f64, f: f64) -> f64 { 1.0 - (1.0 - depth) * nudge_displacement(f) }

/// How deep an arc goes for a pair `pair_width` wide whose narrower column is `narrower` wide, on a
/// display `display_width` wide: far enough that the narrower keeps `NARROWER_SHOWN` of itself in
/// view with the pair centred, between `ZOOM_DEEPEST` and `STRIP_ZOOM`.
pub fn zoom_depth(display_width: f64, pair_width: f64, narrower: f64) -> f64 {
    let shown = pair_width - 2.0 * (1.0 - NARROWER_SHOWN) * narrower;
    let depth = display_width / shown;
    if depth.is_finite() && shown > 0.0 {
        depth.clamp(ZOOM_DEEPEST, STRIP_ZOOM)
    } else {
        STRIP_ZOOM
    }
}

/// Where the view should be at the arc's middle, as the travel left to carry there: `pair` the span
/// the pair fills and `view` the tiling area's, both in x as the layout ends, zoomed to `scale`
/// about `centre`, the travel `start` at the first frame. A pair that fits takes the least change
/// from `start` that puts all of it in view; one that does not is drawn centred on the view, clipped
/// alike on both sides.
pub fn pair_view(pair: (f64, f64), view: (f64, f64), centre: f64, scale: f64, start: f64) -> f64 {
    let ((left, right), (lo, hi)) = (pair, view);
    if !scale.is_finite() || scale <= 0.0 {
        return start;
    }
    if scale * (right - left) <= hi - lo {
        let least = right - centre - (hi - centre) / scale;
        let most = left - centre + (centre - lo) / scale;
        start.max(least).min(most)
    } else {
        (left + right) / 2.0 - centre - ((lo + hi) / 2.0 - centre) / scale
    }
}

/// The share of the travel `start` carried by the arc's middle to leave `view` of it there, or as
/// near as a movement that never turns back gets: between a quarter and three quarters. Half for no
/// travel, and not a number for a travel or a view that is not one.
pub fn camera_share(start: f64, view: f64) -> f64 {
    if !start.is_finite() || !view.is_finite() {
        return f64::NAN;
    }
    if start.abs() < 1e-9 {
        return ARC_MIDDLE;
    }
    let (a, b) = (0.75 * start, 0.25 * start);
    1.0 - view.clamp(a.min(b), a.max(b)) / start
}

/// How much of the travel is carried at progress `f`, with `share` of it carried at the middle: a
/// quadratic Bezier through that share at half time, timed on `EASE_IN_OUT`, so it leaves and lands
/// at rest and, for a share between a quarter and three quarters, goes one way. Half is the
/// ease-in-out itself.
pub fn camera_progress(share: f64, f: f64) -> f64 {
    let u = EASE_IN_OUT.ease(f.clamp(0.0, 1.0));
    1.0 - (1.0 - u) * (1.0 - u) - 2.0 * u * (1.0 - u) * (1.5 - 2.0 * share)
}

/// What Zoom does with a press on the strip. A move crosses both columns of its pair from where they
/// start, nothing held and no step, on an arc as deep as the pair needs, the travel carried to show
/// the pair at its middle. A focus press that scrolls the strip between two of its windows pans on
/// an arc as deep as a move of the same two columns, half of it carried at the middle, and a switch
/// on one as deep as the two strips it travels between need (`switch_arc`). Anything else stages
/// nothing.
pub fn stage(cue: &Cue, strip: &Strip) -> Staging {
    let accent = match *cue {
        Cue::Move { window, .. } => return staged_move(window, strip),
        Cue::Focus { from, to } => focus_arc(from, to, strip),
        Cue::Switch => switch_arc(strip),
        Cue::Plain => None,
    };
    Staging { accent, ..Staging::default() }
}

fn frame_of(strip: &Strip, window: WindowId) -> Option<SurfaceRect> {
    strip.windows.iter().find(|(w, _)| *w == window).map(|&(_, frame)| frame)
}

/// The tiling area's span in x, and the x of the display's centre, which a zoom is about.
fn tiling(strip: &Strip) -> ((f64, f64), f64) {
    let area = strip.area;
    let (lo, hi) = (
        area.frame.min().x + area.outer_left,
        area.frame.max().x - area.outer_right,
    );
    ((lo, hi), area.viewport.center().x)
}

/// The span `frames` fill in x.
fn span(frames: &[SurfaceRect]) -> (f64, f64) {
    let left = frames.iter().map(|f| f.min().x).fold(f64::INFINITY, f64::min);
    let right = frames.iter().map(|f| f.max().x).fold(f64::NEG_INFINITY, f64::max);
    (left, right)
}

fn staged_move(moved: WindowId, strip: &Strip) -> Staging {
    let Some(at) = frame_of(strip, moved) else {
        return Staging::default();
    };
    if strip.pair.is_empty() {
        return Staging::default();
    }
    let frames: Vec<SurfaceRect> =
        strip.pair.iter().filter_map(|p| frame_of(strip, p.window)).collect();
    let (left, right) = span(&frames);
    let passed = frames
        .iter()
        .find(|f| (f.origin.x - at.origin.x).abs() > COLUMN_TOLERANCE)
        .map_or(at.size.width, |f| f.size.width);
    let depth = zoom_depth(
        strip.area.frame.size.width,
        right - left,
        at.size.width.min(passed),
    );
    let (view, centre) = tiling(strip);
    let start = strip.travel.x;
    let share = camera_share(start, pair_view((left, right), view, centre, depth, start));
    let finite = [depth, share, strip.travel.y, strip.seconds].iter().all(|v| v.is_finite());
    Staging {
        from: strip.pair.iter().map(|p| (p.window, p.start)).collect(),
        held: Vec::new(),
        accent: finite.then(|| Accent::Arc {
            depth,
            share,
            swap: swap_of(strip),
            seconds: strip.seconds,
        }),
    }
}

/// The pair's two columns, each by one window and how far it crosses, its frame less its start.
fn swap_of(strip: &Strip) -> Option<Swap> {
    let by = |passed: bool| {
        strip.pair.iter().filter(|p| p.passed == passed).find_map(|p| {
            let frame = frame_of(strip, p.window)?;
            let by = OverlayVector::new(
                frame.origin.x - p.start.origin.x,
                frame.origin.y - p.start.origin.y,
            );
            Some((p.window, by))
        })
    };
    let ((moved, moved_by), (passed, passed_by)) = (by(false)?, by(true)?);
    Some(Swap {
        moved,
        passed,
        moved_by,
        passed_by,
    })
}

fn focus_arc(from: WindowId, to: WindowId, strip: &Strip) -> Option<Accent> {
    let travel = strip.travel;
    if !travel.x.is_finite() || !travel.y.is_finite() || travel.x.abs() < 1.0 {
        return None;
    }
    let (a, b) = (frame_of(strip, from)?, frame_of(strip, to)?);
    let (left, right) = span(&[a, b]);
    let narrower = a.size.width.min(b.size.width);
    let depth = zoom_depth(strip.area.frame.size.width, right - left, narrower);
    depth.is_finite().then_some(Accent::Arc {
        depth,
        share: ARC_MIDDLE,
        swap: None,
        seconds: strip.seconds,
    })
}

/// A switch's arc: as deep as a move of two columns a usable height tall, the strip and the one it
/// travels to, a travel apart, so each keeps two thirds of itself in view with the two centred, 0.738
/// on the laptop for one strip and the deepest for two or more; half the travel carried at the
/// middle. None for a switch that travels no whole point up or down.
fn switch_arc(strip: &Strip) -> Option<Accent> {
    let (travel, row) = (strip.travel, strip.area.frame.size.height);
    if !travel.x.is_finite() || !travel.y.is_finite() || travel.y.abs() < 1.0 {
        return None;
    }
    let depth = zoom_depth(row, travel.y.abs() + row, row);
    (depth.is_finite() && strip.seconds.is_finite()).then_some(Accent::Arc {
        depth,
        share: ARC_MIDDLE,
        swap: None,
        seconds: strip.seconds,
    })
}

/// How many samples an arc of `length` seconds is drawn from: one a 120Hz frame or a little closer.
fn sample_count(length: f64) -> usize { (length / SAMPLE_STEP).ceil().max(1.0) as usize }

/// The strip zoomed out to `depth` about `centre` and back over `length` from `begin`, sampled, and
/// placed at rest; nothing for no length.
pub(super) fn strip_arc(
    making: &mut Making,
    depth: f64,
    centre: OverlayPoint,
    begin: f64,
    length: f64,
) {
    strip_zoom(making, centre, begin, length, |f| arc_scale(depth, f));
}

/// The strip zoomed about `centre` over `length` from `begin` through `scale` of the progress,
/// sampled, and placed at rest; nothing for no length.
fn strip_zoom(
    making: &mut Making,
    centre: OverlayPoint,
    begin: f64,
    length: f64,
    scale: impl Fn(f64) -> f64,
) {
    if length <= 0.0 {
        return;
    }
    let count = sample_count(length);
    let values = (0..=count).map(|k| {
        Value::Zoom(if k == count {
            Zoom::REST
        } else {
            Zoom::about(scale(k as f64 / count as f64), centre, OverlayVector::zero())
        })
    });
    making.push(Edit::Place(Node::Strip, Value::Zoom(Zoom::REST)));
    making.push(Edit::Add(Track::samples(
        Node::Strip,
        Channel::Base,
        values,
        begin,
        length,
    )));
}

/// The strip's scale at progress `f` of a push's arc from `from`, where it is drawn, deepest at
/// `STRIP_ZOOM` where the bounce turns, `turn` of the way, and at rest at the end: each half on a
/// quarter of `sin²`, still at both ends.
pub fn push_scale(from: f64, turn: f64, f: f64) -> f64 {
    let half = |u: f64| nudge_displacement(u.clamp(0.0, 1.0) / 2.0);
    if f <= turn {
        from + (STRIP_ZOOM - from) * half(f / turn)
    } else {
        1.0 - (1.0 - STRIP_ZOOM) * (1.0 - half((f - turn) / (1.0 - turn)))
    }
}

/// The strip's scale as `score` draws it at `at`, 1 for none.
fn scale_drawn(score: &Score, at: f64) -> f64 {
    match score.sample(Node::Strip, at) {
        Some(Value::Zoom(zoom)) if zoom.scale.is_finite() && zoom.scale > 0.0 => zoom.scale,
        _ => 1.0,
    }
}

/// Whether the strip `score` draws at `now` is zoomed out further where a bounce of `seconds`
/// turns than it is at `now`.
fn deepening(score: &Score, seconds: f64, now: f64) -> bool {
    let turn = scale_drawn(score, now + BOUNCE_TURN * seconds);
    turn < 1.0 && turn <= scale_drawn(score, now)
}

/// `window`'s tile lifted off the strip and set down again over `length` from `begin`, on the arc's
/// bell, sampled, and placed down; nothing for no length.
fn lift(making: &mut Making, window: WindowId, begin: f64, length: f64) {
    if length <= 0.0 || making.score.parent(window).is_none() {
        return;
    }
    let count = sample_count(length);
    let values = (0..=count).map(|k| {
        Value::Number(if k == count {
            0.0
        } else {
            nudge_displacement(k as f64 / count as f64)
        })
    });
    let node = Node::Lift(window);
    making.push(Edit::Place(node, Value::Number(0.0)));
    making.push(Edit::Add(Track::samples(
        node,
        Channel::Base,
        values,
        begin,
        length,
    )));
}

/// Whether a push against `wall` is against a side of the strip.
fn sideways(wall: OverlayVector) -> bool { wall.y == 0.0 && wall.x != 0.0 }

/// Whether a push against `wall` with `cue` plays zoomed out: a press along the strip against a
/// side of it, or a switch against an end of the stack.
fn zooms_push(cue: &Cue, wall: OverlayVector) -> bool {
    match cue {
        Cue::Move { .. } | Cue::Focus { .. } => sideways(wall),
        Cue::Switch => wall.x == 0.0 && wall.y != 0.0,
        Cue::Plain => false,
    }
}

/// A push's arc at `now` from where the strip is drawn (`push_scale`), over the push's `seconds`
/// stretched by `ZOOM_STRETCH`, or over `seconds` for a stretch past one.
fn push_arc(making: &mut Making, centre: OverlayPoint, seconds: f64, now: f64) {
    let from = scale_drawn(&making.score, now);
    let length = seconds.min(seconds * ZOOM_STRETCH);
    let turn = BOUNCE_TURN * seconds / length;
    strip_zoom(making, centre, now, length, |f| push_scale(from, turn, f));
}

/// Whether the strip's zoom plays at `now` on `score`.
fn arcing(score: &Score, now: f64) -> bool {
    score
        .track(Node::Strip, Channel::Base)
        .is_some_and(|track| track.at(now).is_some())
}

/// A push against `wall` at `at` on `score`, each carrier from where its own bounce is drawn: the
/// strip's containers pushed as far as the strip is zoomed where the bounce turns, so it reaches the
/// wall's distance on screen, and the floating one, drawn unzoomed, by the wall.
fn zoomed_push(score: &Score, wall: OverlayVector, seconds: f64, at: f64) -> Vec<Track> {
    let scale = scale_drawn(score, at + BOUNCE_TURN * seconds);
    if scale == 1.0 {
        return push_tracks(score, wall, seconds, at);
    }
    let floating = |track: &Track| track.node() == Node::Container(GroupKey::Floating);
    let mut tracks: Vec<Track> = push_tracks(score, wall / scale, seconds, at)
        .into_iter()
        .filter(|track| !floating(track))
        .collect();
    tracks.extend(push_tracks(score, wall, seconds, at).into_iter().filter(floating));
    tracks
}

/// A move's swap as composed: when its curve runs, and how far each of its containers crosses.
#[derive(Debug)]
struct Crossing {
    begin: f64,
    seconds: f64,
    by: Vec<(GroupKey, OverlayVector)>,
}

impl Crossing {
    /// How fast the swap alone carries `key` at `at`, in points a second.
    fn rate(&self, key: GroupKey, at: f64) -> OverlayVector {
        by_of(&self.by, key) * (ZOOM_CURVE.slope((at - self.begin) / self.seconds) / self.seconds)
    }
}

/// How far `crossing` takes `key` across, none for a container it does not cross.
fn by_of(crossing: &[(GroupKey, OverlayVector)], key: GroupKey) -> OverlayVector {
    crossing
        .iter()
        .find(|(k, _)| *k == key)
        .map_or(OverlayVector::zero(), |&(_, by)| by)
}

/// The containers of `plan` a move's `swap` crosses, each by how far: none unless its two columns
/// ride two containers.
fn crossing_of(plan: &FlightPlan, swap: Option<Swap>) -> Vec<(GroupKey, OverlayVector)> {
    let key = |window| match plan.member(window) {
        Some(Member::Rigid { key, .. }) => Some(key),
        _ => None,
    };
    match swap.map(|swap| (swap, key(swap.moved), key(swap.passed))) {
        Some((swap, Some(moved), Some(passed))) if moved != passed => {
            vec![(moved, swap.moved_by), (passed, swap.passed_by)]
        }
        _ => Vec::new(),
    }
}

/// The container `key` carried from `from` to `to` over `bell`'s arc from `begin`, `by` of it its
/// swap: its travel on `camera_progress`, the floating one's with the strip's though it is drawn
/// unzoomed, and its swap on `ZOOM_CURVE`, sampled, ending exactly at `to`. Nothing for no movement.
/// A column the view follows has no travel and still crosses.
fn carry(
    making: &mut Making,
    key: GroupKey,
    from: OverlayPoint,
    to: OverlayPoint,
    by: OverlayVector,
    begin: f64,
    bell: Bell,
) {
    if from.same_as(to) && by == OverlayVector::zero() {
        return;
    }
    let length = bell.length;
    let count = sample_count(length);
    let travel = (to - from) - by;
    let values = (0..=count).map(|k| {
        let f = k as f64 / count as f64;
        Value::Point(if k == count {
            to
        } else {
            from + travel * camera_progress(bell.share, f) + by * ZOOM_CURVE.ease(f)
        })
    });
    let track = Track::samples(Node::Container(key), Channel::Base, values, begin, length);
    making.push(Edit::Add(track));
}

/// An arc as a composition takes it: its depth, the share of the travel carried at its middle, the
/// swap and its length.
#[derive(Clone, Copy, Debug)]
struct Bell {
    depth: f64,
    share: f64,
    swap: Option<Swap>,
    length: f64,
}

/// The zoom look, the swap its arc carries, and the centre of the display the flight is staged on,
/// which a push zooms the strip about.
#[derive(Debug, Default)]
pub struct ZoomArc {
    swap: Option<Crossing>,
    centre: Option<OverlayPoint>,
}

impl ZoomArc {
    /// The motion from frame zero on an arc, from `begin` over `bell.length`: every container
    /// carried its travel on `camera_progress` and its swap on `ZOOM_CURVE`, sampled; every loose
    /// tile on `MOTION_CURVE`; the strip zoomed out and back, the window a move moves lifted on the
    /// same bell; and a push that waited, with the strip zoomed.
    fn arc(&mut self, playing: &Score, stage: &Stage, begin: f64, bell: Bell) -> Vec<Edit> {
        let pushes: Vec<Accent> = stage
            .accents
            .iter()
            .filter(|a| matches!(a, Accent::Push { .. }))
            .copied()
            .collect();
        if bell.length <= 0.0 {
            let placed = Stage {
                seconds: 0.0,
                accents: &pushes,
                ..*stage
            };
            return Slide::default().compose(playing, &placed, begin);
        }
        let crossing = crossing_of(stage.plan, bell.swap);
        let mut making = Making::from(playing);
        let mut carried: Vec<GroupKey> = Vec::new();
        for target in animation_targets(stage.plan) {
            match target {
                AnimationTarget::Container { key, from, to } => {
                    if making.container(key).is_none() {
                        continue;
                    }
                    let (from, to) = (from.rounded(), to.rounded());
                    making.push(Edit::Place(Node::Container(key), Value::Point(to)));
                    carry(&mut making, key, from, to, by_of(&crossing, key), begin, bell);
                    carried.push(key);
                }
                AnimationTarget::Tile { window, from, to } => {
                    let Some(offset) =
                        making.score.parent(window).and_then(|key| making.container(key))
                    else {
                        continue;
                    };
                    let from = group_relative(on_overlay(from), offset).rounded();
                    let to = group_relative(on_overlay(to), offset).rounded();
                    making.push(Edit::Place(Node::Tile(window), Value::Rect(to)));
                    let leg = tile::leg(window, from, to, begin, bell.length, MOTION_CURVE);
                    making.push(Edit::Add(leg));
                }
            }
        }
        for &(key, by) in crossing.iter().filter(|(key, _)| !carried.contains(key)) {
            if let Some(at) = making.container(key) {
                carry(&mut making, key, at, at, by, begin, bell);
            }
        }
        strip_arc(
            &mut making,
            bell.depth,
            stage.viewport.center(),
            begin,
            bell.length,
        );
        if let Some(swap) = bell.swap {
            lift(&mut making, swap.moved, begin, bell.length);
        }
        for accent in &pushes {
            if let Accent::Push { wall, seconds } = *accent {
                for track in zoomed_push(&making.score, wall, seconds, begin) {
                    making.push(Edit::Add(track));
                }
            }
        }
        self.swap = (!crossing.is_empty()).then_some(Crossing {
            begin,
            seconds: bell.length,
            by: crossing,
        });
        making.edits
    }

    /// The pan a pass brings folds into the arc playing on `playing` at `now` when it is along one
    /// axis, sideways along the strip or up or down the stack, at least `FOLDED_PAN_MIN` of the arc
    /// is left, and the travel every strip container it bends carries along that axis, its swap
    /// aside, does not go against it: when the arc lands, for the fold to land with it.
    fn folds(&self, playing: &Score, merged: &Merged, pass: &Pass, now: f64) -> Option<f64> {
        let strip = playing.track(Node::Strip, Channel::Base).filter(|t| t.at(now).is_some())?;
        let pan = pass.pan?;
        let (along, across) = match (pan.x.abs() >= 0.5, pan.y.abs() >= 0.5) {
            (true, false) => (0, pan.x),
            (false, true) => (1, pan.y),
            _ => return None,
        };
        let (end, length) = (strip.ends(), strip.seconds());
        if end - now + 1e-9 < FOLDED_PAN_MIN * length {
            return None;
        }
        let still = CAMERA_STILL_UNDER / length;
        let turns_back = merged
            .delta
            .retargeted_groups
            .iter()
            .filter(|(key, _)| *key != GroupKey::Floating)
            .any(|&(key, _)| {
                let drawn = playing.rate(Node::Container(key), now)[along];
                let swap = self.swap.as_ref().map_or(0.0, |crossing| {
                    let rate = crossing.rate(key, now);
                    if along == 0 { rate.x } else { rate.y }
                });
                (swap - drawn) * across.signum() >= still
            });
        (!turns_back).then_some(end)
    }
}

/// Zoom's motion at a bend: the score it lands on, the clock and length every movement of it runs
/// on, when the arc it folds into lands, if it folds, and the swap the arc carries.
struct Bent<'a> {
    playing: &'a Score,
    now: f64,
    seconds: f64,
    omega: f64,
    fold: Option<f64>,
    swap: &'a mut Option<Crossing>,
}

impl Bent<'_> {
    /// `key` carried from `from` to `to` on a spring let go at `velocity` points a second.
    fn spring(
        &self,
        key: GroupKey,
        from: OverlayPoint,
        to: OverlayPoint,
        velocity: [f64; 4],
    ) -> Track {
        let leg = Leg::Spring {
            from,
            to,
            velocity: OverlayVector::new(velocity[0], velocity[1]),
            begin: self.now,
            omega: self.omega,
        };
        leg.track(key, SAMPLE_STEP)
    }

    /// `key`'s movement as it plays, shifted on to `to` over what is left of the arc, landing at
    /// `end`, on `EASE_IN_OUT` from zero rate, so it goes on as fast as it was going: none for one
    /// whose movement outlasts the arc. Only a switch moves the floating container, which folds
    /// with the strips it travels with.
    fn folded(&self, key: GroupKey, to: OverlayPoint, end: f64) -> Option<Track> {
        let node = Node::Container(key);
        let old = self.playing.track(node, Channel::Base);
        if old.is_some_and(|track| track.ends() > end + 1e-9) {
            return None;
        }
        let Some(Value::Point(was)) = self.playing.model(node) else {
            return None;
        };
        let (shift, length) = (to - was, end - self.now);
        let count = sample_count(length);
        let values = (0..=count).map(|k| {
            let f = k as f64 / count as f64;
            let at = self.now + length * f;
            let playing = match old.and_then(|track| track.at(at)) {
                Some(Value::Point(point)) => point,
                _ => was,
            };
            Value::Point(if k == count {
                to
            } else {
                playing + shift * EASE_IN_OUT.ease(f)
            })
        });
        Some(Track::samples(node, Channel::Base, values, self.now, length))
    }
}

impl Bending for Bent<'_> {
    fn retarget(&mut self, making: &mut Making, way: ContainerWay) {
        let ContainerWay { key, from, to } = way;
        if self.seconds <= 0.0 {
            self.forget(key);
            return;
        }
        if let Some(track) = self.fold.and_then(|end| self.folded(key, to, end)) {
            making.push(Edit::Add(track));
            return;
        }
        let velocity = self.playing.rate(Node::Container(key), self.now);
        making.push(Edit::Add(self.spring(key, from, to, velocity)));
        self.forget(key);
    }

    fn open(&mut self, making: &mut Making, way: ContainerWay, leaving: Option<GroupKey>) {
        let ContainerWay { key, from, to } = way;
        let from = from.rounded();
        self.forget(key);
        if self.seconds <= 0.0 || from.same_as(to) {
            return;
        }
        let track = match leaving {
            Some(left) => {
                let velocity = self.playing.rate(Node::Container(left), self.now);
                self.spring(key, from, to, velocity)
            }
            None => {
                let leg = Leg::Curve {
                    from,
                    to,
                    begin: self.now,
                    seconds: self.seconds,
                    curve: MOTION_CURVE,
                };
                leg.track(key, SAMPLE_STEP)
            }
        };
        making.push(Edit::Add(track));
    }

    fn tile(&mut self, making: &mut Making, way: TileWay) {
        let TileWay { window, from, to } = way;
        if self.seconds > 0.0 {
            let leg = tile::leg(window, from, to, self.now, self.seconds, MOTION_CURVE);
            making.push(Edit::Add(leg));
        }
    }

    fn forget(&mut self, key: GroupKey) {
        if let Some(crossing) = self.swap.as_mut() {
            crossing.by.retain(|(crossed, _)| *crossed != key);
        }
    }
}

impl Choreography for ZoomArc {
    fn plan(&self, pass: ReflowPlan, cue: &Cue) -> FlightPlan { Slide::default().plan(pass, cue) }

    fn fold(&self, plan: &FlightPlan, pass: &Pass) -> FlightPlan {
        Slide::default().fold(plan, pass)
    }

    fn pose(&self, stage: &Stage) -> Score { Slide::default().pose(stage) }

    /// The motion from frame zero at `begin`: on the arc an accent asks for, over its press's plain
    /// length stretched by `ZOOM_STRETCH`; or as Slide composes it, a push against a side wall
    /// that waited with a press along the strip playing over a shallow arc of the push's length
    /// stretched, ending by the time the push does, the strip zoomed as it bounces.
    fn compose(&mut self, playing: &Score, stage: &Stage, begin: f64) -> Vec<Edit> {
        self.swap = None;
        self.centre = Some(stage.viewport.center());
        let arc = stage.accents.iter().find_map(|accent| match *accent {
            Accent::Arc { depth, share, swap, seconds } => Some(Bell {
                depth,
                share,
                swap,
                length: seconds * ZOOM_STRETCH,
            }),
            Accent::Push { .. } | Accent::Step { .. } => None,
        });
        if let Some(bell) = arc {
            return self.arc(playing, stage, begin, bell);
        }
        let wall = stage.accents.iter().find_map(|accent| match *accent {
            Accent::Push { wall, seconds } if zooms_push(&stage.cue, wall) => Some((wall, seconds)),
            _ => None,
        });
        let Some((wall, seconds)) = wall else {
            return Slide::default().compose(playing, stage, begin);
        };
        let others: Vec<Accent> = stage
            .accents
            .iter()
            .filter(|a| !matches!(a, Accent::Push { .. }))
            .copied()
            .collect();
        let mut making = Making::from(playing);
        let slid = Slide::default().compose(playing, &Stage { accents: &others, ..*stage }, begin);
        for edit in slid {
            making.push(edit);
        }
        push_arc(&mut making, stage.viewport.center(), seconds, begin);
        for track in zoomed_push(&making.score, wall, seconds, begin) {
            making.push(Edit::Add(track));
        }
        making.edits
    }

    /// A later pass at `now`, bent as every look bends it (`Merged::bend`), over its plain length,
    /// stretched by `ZOOM_STRETCH` when it brings an arc: a pan folded into the arc playing
    /// (`folds`), or every container on a spring from where the score has it at the speed it is
    /// drawn moving; a container opened for a member leaving another as fast as that one, and one
    /// opened for a newcomer on `MOTION_CURVE`; every loose tile on `MOTION_CURVE`. A move, a
    /// focus press or a switch brought with no arc playing plays a shallow one from now, a move
    /// lifting the window it moves on it; one playing is left as it is.
    fn retarget(&mut self, playing: &Score, plan: &FlightPlan, pass: &Pass, now: f64) -> Retarget {
        self.centre = Some(pass.viewport.center());
        let mut merged = Merged::new(playing, plan, pass, now);
        let arced = matches!(pass.accent, Some(Accent::Arc { .. }));
        let seconds = if arced {
            pass.seconds * ZOOM_STRETCH
        } else {
            pass.seconds
        };
        let fold = self.folds(playing, &merged, pass, now);
        let arcing = arcing(playing, now);
        merged.bend(&mut Bent {
            playing,
            now,
            seconds,
            omega: spring_omega(seconds),
            fold,
            swap: &mut self.swap,
        });
        if arced && !arcing {
            strip_arc(
                &mut merged.making,
                STRIP_ZOOM,
                pass.viewport.center(),
                now,
                seconds,
            );
            if let Some(Accent::Arc { swap: Some(swap), .. }) = pass.accent {
                lift(&mut merged.making, swap.moved, now, seconds);
            }
        }
        merged.retarget()
    }

    /// A push against `wall` at `now`, on every container that carries it, each from where its own
    /// bounce is drawn, with the strip zoomed as it is where the bounce turns (`zoomed_push`). A
    /// press along the strip against a side wall first zooms the strip out again from where it is
    /// drawn (`push_arc`), unless the strip is drawn deeper where the bounce turns than now.
    fn push(
        &mut self,
        playing: &Score,
        cue: &Cue,
        wall: OverlayVector,
        seconds: f64,
        now: f64,
    ) -> Vec<Edit> {
        let mut making = Making::from(playing);
        if let Some(centre) = self.centre.filter(|_| zooms_push(cue, wall))
            && !deepening(playing, seconds, now)
        {
            push_arc(&mut making, centre, seconds, now);
        }
        for track in zoomed_push(&making.score, wall, seconds, now) {
            making.push(Edit::Add(track));
        }
        making.edits
    }
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{OverlayRect, SurfacePoint};
    use rini_ipc::protocol::Direction;

    use super::*;
    use crate::animation::domain::choreography::{Area, PairStart};
    use crate::animation::domain::motion::bounce::Bounce;
    use crate::animation::domain::motion::plan::{Resolved, overlay_of, reflow_plan, surface_plan};
    use crate::animation::domain::motion::score::Form;
    use crate::animation::domain::motion::surface::pan_travel;
    use crate::animation::domain::motion::testing::{DISPLAY, VIEWPORT, rect};
    use crate::transitions::domain::request::SurfaceWindow;

    const PLAIN: f64 = 0.35;
    const BEGIN: f64 = 10.0;
    const GAP: f64 = 3.0;
    /// The arc at the default length.
    const ARC: f64 = PLAIN * ZOOM_STRETCH;
    /// The laptop: 1728pt wide, 3pt outer gaps, the tiling area [3, 1725] about x = 864.
    const AREA: Area = Area {
        frame: VIEWPORT,
        outer_left: GAP,
        outer_right: GAP,
        viewport: VIEWPORT,
    };
    const CENTRE: f64 = 864.0;
    const WIDTH: f64 = 1728.0;
    const W: u32 = 1;
    const N: u32 = 2;
    const C: u32 = 3;

    fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

    fn column(x: f64, width: f64) -> SurfaceRect { rect(x, 32.0, width, 1081.0) }

    fn close(got: f64, want: f64, slack: f64, what: &str) {
        assert!((got - want).abs() <= slack, "{what}: {got:.4}, want {want:.4}");
    }

    /// A press on a strip: every tiled window at its new frame, the pair a move swapped, the
    /// viewport's travel, and the screen.
    struct Press {
        windows: Vec<(WindowId, SurfaceRect)>,
        pair: Vec<PairStart>,
        travel: f64,
        seconds: f64,
        area: Area,
    }

    /// One flight of the look, every edit it answers folded into its score.
    struct Flight {
        zoom: ZoomArc,
        plan: FlightPlan,
        drawn: Vec<WindowId>,
        score: Score,
        posed: Score,
        accent: Option<Accent>,
    }

    impl Press {
        fn strip(&self) -> Strip<'_> {
            Strip {
                windows: &self.windows,
                pair: &self.pair,
                area: self.area,
                travel: OverlayVector::new(self.travel, 0.0),
                seconds: self.seconds,
            }
        }

        /// `cue` staged by Zoom and flown as the reactor and the engine fly it: posed, and the
        /// motion composed at `BEGIN` with the accent it staged waiting.
        fn flown(&self, cue: Cue) -> Flight { self.flown_beside(cue, &[]) }

        /// `flown`, with the windows of `rows`, each of the strip its index names, drawn beside
        /// the strip.
        fn flown_beside(&self, cue: Cue, rows: &[(WindowId, SurfaceRect, usize)]) -> Flight {
            let staging = stage(&cue, &self.strip());
            let from = |window| staging.from.iter().find(|(w, _)| *w == window).map(|&(_, at)| at);
            let on_surface: Vec<SurfaceWindow> = self
                .windows
                .iter()
                .map(|&(id, frame)| SurfaceWindow {
                    from: from(id),
                    ..on_surface(id, frame, None)
                })
                .chain(rows.iter().map(|&(id, frame, row)| on_surface(id, frame, Some(row))))
                .collect();
            let offsets = (SurfacePoint::new(self.travel, 0.0), SurfacePoint::origin());
            fly(&on_surface, offsets, cue, staging.accent, self.seconds)
        }
    }

    /// `window` on the strip surface at `frame`, of the strip beside `row` names, if any.
    fn on_surface(window: WindowId, frame: SurfaceRect, row: Option<usize>) -> SurfaceWindow {
        SurfaceWindow {
            window,
            server_id: rini_core::ids::WindowServerId::new(window.idx.get()),
            frame,
            pinned: false,
            floating: false,
            from: None,
            worn: Default::default(),
            row,
        }
    }

    /// A strip movement of `windows` from `from_offset` to `to_offset` showing `cue`, flown as the
    /// engine flies it: planned, posed, and the motion composed at `BEGIN` with `accent` waiting.
    fn fly(
        windows: &[SurfaceWindow],
        (from_offset, to_offset): (SurfacePoint, SurfacePoint),
        cue: Cue,
        accent: Option<Accent>,
        seconds: f64,
    ) -> Flight {
        let plan = FlightPlan::from(surface_plan(windows, from_offset, to_offset));
        let drawn = plan.windows();
        let accents: Vec<Accent> = accent.into_iter().collect();
        let stage = Stage {
            plan: &plan,
            drawn: &drawn,
            seconds,
            cue,
            accents: &accents,
            viewport: VIEWPORT,
        };
        let mut zoom = ZoomArc::default();
        let posed = zoom.pose(&stage);
        let mut score = posed.clone();
        for edit in zoom.compose(&posed, &stage, BEGIN) {
            score.apply(&edit);
        }
        Flight {
            zoom,
            plan,
            drawn,
            score,
            posed,
            accent,
        }
    }

    /// W, `w` wide, moved past N, `n` wide, as `swap_starts` stages it on the laptop. Right: W at
    /// [3, 3 + w] before and after, the view following it, N from its right, and C, as wide as N,
    /// left of N riding the strip. Left: the pair at the start of the strip, W from N's right, and
    /// C right of the pair, so nothing scrolls.
    fn pair_move(w: f64, n: f64, right: bool) -> Press {
        let (windows, pair, travel) = if right {
            (
                vec![
                    (wid(C), column(-n - GAP - n, n)),
                    (wid(N), column(-n, n)),
                    (wid(W), column(GAP, w)),
                ],
                vec![(W, column(-n, w), false), (N, column(GAP + w - n, n), true)],
                -n - GAP,
            )
        } else {
            (
                vec![
                    (wid(W), column(GAP, w)),
                    (wid(N), column(GAP + w + GAP, n)),
                    (wid(C), column(GAP + w + GAP + n + GAP, n)),
                ],
                vec![
                    (W, column(GAP + GAP + n, w), false),
                    (N, column(GAP, n), true),
                ],
                0.0,
            )
        };
        let pair = pair
            .into_iter()
            .map(|(idx, start, passed)| PairStart {
                window: wid(idx),
                start,
                passed,
            })
            .collect();
        Press {
            windows,
            pair,
            travel,
            seconds: PLAIN,
            area: AREA,
        }
    }

    /// Focus one column right along a strip of columns `w` wide, the strip scrolled by one column:
    /// W, which focus left, at [3, 3 + w] before and a column left of it after, N where W was, and
    /// C left of W.
    fn focus_right(w: f64) -> Press {
        Press {
            windows: vec![
                (wid(C), column(-2.0 * w - GAP, w)),
                (wid(W), column(-w, w)),
                (wid(N), column(GAP, w)),
            ],
            pair: Vec::new(),
            travel: -(w + GAP),
            seconds: PLAIN,
            area: AREA,
        }
    }

    fn arc_of(accent: Option<Accent>) -> (f64, f64, Option<Swap>) {
        match accent {
            Some(Accent::Arc { depth, share, swap, seconds }) => {
                assert_eq!(seconds, PLAIN);
                (depth, share, swap)
            }
            other => panic!("an arc: {other:?}"),
        }
    }

    fn key_of(plan: &FlightPlan, window: WindowId) -> GroupKey {
        match plan.member(window) {
            Some(Member::Rigid { key, .. }) => key,
            other => panic!("{window:?} rides a container: {other:?}"),
        }
    }

    fn scale_at(score: &Score, at: f64) -> f64 {
        match score.sample(Node::Strip, at) {
            Some(Value::Zoom(zoom)) => zoom.scale,
            other => panic!("the strip holds a zoom: {other:?}"),
        }
    }

    /// Where `score` lays `window` out at `at`, before the strip's zoom.
    fn laid(score: &Score, window: WindowId, at: f64) -> OverlayRect {
        let key = score.parent(window).expect("drawn");
        match (
            score.sample(Node::Tile(window), at),
            score.sample(Node::Container(key), at),
        ) {
            (Some(Value::Rect(frame)), Some(Value::Point(container))) => {
                overlay_of(frame, container)
            }
            other => panic!("{other:?}"),
        }
    }

    /// Kai, 2026-10-17: "when zoomout happens i should see the top/bottom strips (at a bit of a
    /// padding)". A move or a focus press arcing along the strip zooms the strips above and below
    /// with it and moves neither: each stands where it is laid out, a usable height and
    /// `STACK_GAP` away, and shows through the zoom, the bottom of the one above 38.5pt below the
    /// bar at 0.88 and 109.7pt at 0.7516, the top of the one below 42.4pt and 117.7pt above the
    /// display's bottom, a little more as the zoom is about the display's centre.
    #[test]
    fn a_horizontal_arc_zooms_the_rows_and_moves_none() {
        const BAND: f64 = 32.0;
        let pitch = 1085.0 + STACK_GAP;
        let above = rect(GAP, 36.0 - pitch, 1722.0, 1077.0);
        let below = rect(-500.0, 36.0 + pitch, 1722.0, 1077.0);
        let rows = [(wid(8), above, 0), (wid(9), below, 2)];
        for (press, cue, depth) in [
            (
                pair_move(860.0, 860.0, true),
                Cue::Move {
                    window: wid(W),
                    direction: Direction::Right,
                },
                0.88,
            ),
            (
                focus_right(1722.0),
                Cue::Focus { from: wid(W), to: wid(N) },
                0.7516,
            ),
        ] {
            let flight = press.flown_beside(cue, &rows);
            let alone = press.flown(cue);
            assert_eq!(flight.accent, alone.accent, "{cue:?}: staged as without them");
            let (got, _, _) = arc_of(flight.accent);
            close(got, depth, 1e-4, "the arc's depth");
            let middle = BEGIN + ARC * ARC_MIDDLE;
            close(
                scale_at(&flight.score, middle),
                got,
                1e-3,
                "deepest at the middle",
            );
            let at_rest = |f: SurfaceRect| -> OverlayRect {
                rect(f.origin.x, f.origin.y, f.size.width, f.size.height)
            };
            for (row, frame, _) in rows {
                let key = key_of(&flight.plan, row);
                assert_ne!(
                    key,
                    key_of(&flight.plan, wid(W)),
                    "{row:?}: a container of its own"
                );
                assert!(flight.score.track(Node::Container(key), Channel::Base).is_none());
                for k in 0..=20 {
                    let at = BEGIN + ARC * k as f64 / 20.0;
                    assert_eq!(laid(&flight.score, row, at), at_rest(frame), "{row:?} moves");
                }
            }
            let scale = scale_at(&flight.score, middle);
            let centre = AREA.viewport.center().y;
            let shown = |window| flight.score.drawn(window, middle).expect("drawn");
            let above_shows = shown(wid(8)).max().y - BAND;
            let below_shows = AREA.viewport.max().y - shown(wid(9)).min().y;
            close(
                above_shows,
                centre + (above.max().y - centre) * scale - BAND,
                1e-6,
                "zoomed",
            );
            close(
                below_shows,
                AREA.viewport.max().y - centre - (below.min().y - centre) * scale,
                1e-6,
                "zoomed",
            );
            let (want_above, want_below) = if depth == 0.88 {
                (38.5, 42.4)
            } else {
                (109.7, 117.7)
            };
            close(above_shows, want_above, 0.6, "the strip above shows");
            close(below_shows, want_below, 0.6, "the strip below shows");
            for window in [wid(8), wid(9)] {
                let end = BEGIN + flight.score.remaining(BEGIN);
                assert_eq!(
                    flight.score.drawn(window, end),
                    flight.plan.destination(window),
                    "home"
                );
            }
        }
    }

    /// A push at a side of the strip or at an end of the stack gives the whole zoomed view: the
    /// strips beside it are carried toward the wall as far as the strip is.
    #[test]
    fn a_push_at_a_side_or_an_end_of_the_stack_carries_the_rows_with_the_strip() {
        let rows = [
            (wid(8), rect(GAP, 36.0 - 1109.0, 1722.0, 1077.0), 0),
            (wid(9), rect(GAP, 36.0 + 1109.0, 1722.0, 1077.0), 2),
        ];
        let press = Cue::Focus { from: wid(W), to: wid(N) };
        let pushes = [
            (OverlayVector::new(-72.0, 0.0), press),
            (OverlayVector::new(0.0, 72.0), Cue::Switch),
        ];
        for (wall, cue) in pushes {
            let mut flight = focus_right(1722.0).flown_beside(press, &rows);
            let without = flight.score.clone();
            let now = BEGIN + 0.05;
            for edit in flight.zoom.push(&flight.score, &cue, wall, PLAIN, now) {
                flight.score.apply(&edit);
            }
            let turn = now + BOUNCE_TURN * PLAIN;
            let toward = wall / 72.0;
            let given = |window| {
                (laid(&flight.score, window, turn).origin - laid(&without, window, turn).origin)
                    .dot(toward)
            };
            let strip = given(wid(N));
            assert!(strip > 1.0, "{wall:?}: the strip gives toward the wall: {strip}");
            for (row, _, _) in rows {
                close(
                    given(row),
                    strip,
                    1e-6,
                    &format!("{wall:?}: as far as the strip"),
                );
            }
        }
    }

    /// The laptop's usable height under rini's bar, and how far apart a switch under zoom stacks
    /// its strips: that and `STACK_GAP`.
    const USABLE: f64 = 1085.0;
    const PITCH: f64 = USABLE + STACK_GAP;

    /// The laptop's screen a switch is staged on: its usable frame below the 32pt bar.
    fn usable_area() -> Area {
        Area {
            frame: rect(0.0, 32.0, WIDTH, USABLE),
            ..AREA
        }
    }

    /// One window of the strip at `row` of a switch's surface, `PITCH` apart, at `x`, `width` wide.
    fn in_row(row: f64, x: f64, width: f64) -> SurfaceRect {
        rect(x, 36.0 + row * PITCH, width, 1077.0)
    }

    /// The source's two halves, its floating window, the destination's one full column, a strip
    /// between them for each row past one, and the strips above the source and below the
    /// destination.
    const S1: u32 = 10;
    const S2: u32 = 11;
    const FLOATING: u32 = 12;
    const D: u32 = 20;
    const ABOVE: u32 = 30;
    const BELOW: u32 = 31;
    const BETWEEN: u32 = 40;

    /// A switch `rows` down the stack under zoom, as the reactor sends one and the engine stages
    /// it: the source at the second workspace, its halves and its floating window; every strip on
    /// the way; the destination; and the strips above the source and below the destination, each
    /// strip but the destination's named by its index, `PITCH` apart, over `seconds`.
    fn switch_down(rows: usize, seconds: f64) -> Flight {
        let n = rows as f64;
        let mut windows = vec![
            on_surface(wid(S1), in_row(0.0, GAP, 859.0), Some(1)),
            on_surface(wid(S2), in_row(0.0, GAP + 859.0 + GAP, 859.0), Some(1)),
            SurfaceWindow {
                floating: true,
                ..on_surface(wid(FLOATING), rect(400.0, 300.0, 600.0, 400.0), None)
            },
            on_surface(wid(D), in_row(n, GAP, 1722.0), None),
            on_surface(wid(ABOVE), in_row(-1.0, GAP, 1722.0), Some(0)),
            on_surface(wid(BELOW), in_row(n + 1.0, -500.0, 1722.0), Some(2 + rows)),
        ];
        for k in 1..rows {
            let between = on_surface(
                wid(BETWEEN + k as u32),
                in_row(k as f64, GAP, 1722.0),
                Some(1 + k),
            );
            windows.push(between);
        }
        let (from_offset, to_offset) = (SurfacePoint::origin(), SurfacePoint::new(0.0, n * PITCH));
        let strip: Vec<(WindowId, SurfaceRect)> = windows
            .iter()
            .filter(|w| !w.floating && w.row.is_none())
            .map(|w| (w.window, w.frame))
            .collect();
        let staging = stage(&Cue::Switch, &Strip {
            windows: &strip,
            pair: &[],
            area: usable_area(),
            travel: pan_travel(from_offset, to_offset),
            seconds,
        });
        assert!(staging.from.is_empty() && staging.held.is_empty());
        fly(
            &windows,
            (from_offset, to_offset),
            Cue::Switch,
            staging.accent,
            seconds,
        )
    }

    /// Kai, 2026-10-17: "it applies to both horizontal and vertical navigation". A switch stages
    /// an arc as deep as a move of two columns a usable height tall, the strip and the one it
    /// travels to, a pitch apart: 0.738 for one strip on the laptop, the deepest for two or more,
    /// half the travel carried at the middle and nothing swapped. A switch that travels no whole
    /// point, or not a number, plays none.
    #[test]
    fn a_switch_arcs_as_deep_as_the_two_strips_need() {
        let strip = |dy: f64| Strip {
            windows: &[],
            pair: &[],
            area: usable_area(),
            travel: OverlayVector::new(0.0, dy),
            seconds: PLAIN,
        };
        for (dy, depth) in [
            (-PITCH, 0.7378),
            (PITCH, 0.7378),
            (-2.0 * PITCH, ZOOM_DEEPEST),
        ] {
            let (got, share, swap) = arc_of(stage(&Cue::Switch, &strip(dy)).accent);
            close(got, depth, 5e-5, &format!("{dy}"));
            assert_eq!((share, swap), (ARC_MIDDLE, None));
        }
        for dy in [0.5, f64::NAN] {
            assert_eq!(stage(&Cue::Switch, &strip(dy)), Staging::default(), "{dy}");
        }
    }

    /// A switch zooms the strip out and back on one arc of 2.2/3 of its own length, its rows'
    /// stretch and all, while every strip and its floating windows travel on the arc's camera,
    /// half way at the middle, so the stack stays one column; and it lands every window exactly
    /// where the switch sends it, the destination at its frames, at rest.
    #[test]
    fn a_switch_zooms_out_carries_the_stack_half_way_and_lands_on_the_destination_at_rest() {
        for (rows, seconds) in [(1, PLAIN), (2, PLAIN * 2f64.sqrt())] {
            let flight = switch_down(rows, seconds);
            let tag = format!("{rows} rows");
            let (depth, _, _) = match flight.accent {
                Some(Accent::Arc { depth, share, swap, .. }) => (depth, share, swap),
                other => panic!("{tag}: an arc: {other:?}"),
            };
            let arc = flight.score.track(Node::Strip, Channel::Base).expect("an arc").clone();
            assert_eq!(arc.begin(), BEGIN);
            close(arc.seconds(), seconds * ZOOM_STRETCH, 1e-9, &tag);
            let middle = BEGIN + arc.seconds() * ARC_MIDDLE;
            close(
                scale_at(&flight.score, middle),
                depth,
                1e-3,
                "deepest at the middle",
            );
            let keys: Vec<GroupKey> =
                [S1, D, ABOVE, BELOW].map(|w| key_of(&flight.plan, wid(w))).into();
            for (i, key) in keys.iter().enumerate() {
                assert!(!keys[..i].contains(key), "{tag}: a container each strip");
            }
            let end = BEGIN + flight.score.remaining(BEGIN);
            close(end, arc.ends(), 1e-9, "the flight ends with the arc");
            assert_eq!(
                flight.score.sample(Node::Strip, end),
                Some(Value::Zoom(Zoom::REST))
            );
            for &window in &flight.drawn {
                let start = flight.posed.drawn(window, BEGIN).expect("posed");
                let sent = flight.plan.destination(window).expect("sent");
                close(sent.origin.y - start.origin.y, -(rows as f64) * PITCH, 0.5, &tag);
                let half = laid(&flight.score, window, middle).origin.y - start.origin.y;
                close(
                    half,
                    (sent.origin.y - start.origin.y) / 2.0,
                    1.0,
                    &format!("{tag}: {window:?} half way"),
                );
                assert_eq!(
                    flight.score.drawn(window, end),
                    Some(sent),
                    "{tag}: {window:?} lands"
                );
            }
            assert_eq!(
                flight.plan.destination(wid(D)),
                Some(rect(GAP, 36.0, 1722.0, 1077.0))
            );
        }
    }

    /// Kai, 2026-10-17: "i should see the top/bottom strips (at a bit of a padding)". At a switch's
    /// middle the strips it travels between are in view with the gap between them zoomed: one row
    /// down, 66% of the source below the bar and 67% of the destination above the display's
    /// bottom, 32pt apart at rest
    /// and 23.6pt on screen; two rows down, the strip passed whole in view, 138pt of the source
    /// below the bar and 148pt of the destination above the display's bottom.
    #[test]
    fn at_a_switchs_middle_the_strips_it_travels_between_show_the_gap_apart() {
        const BAND: f64 = 32.0;
        let bottom = AREA.viewport.max().y;
        let shown =
            |frame: OverlayRect| (frame.max().y.min(bottom) - frame.min().y.max(BAND)).max(0.0);

        let one = switch_down(1, PLAIN);
        let middle = BEGIN + ARC * ARC_MIDDLE;
        let scale = scale_at(&one.score, middle);
        let (source, destination) = (
            one.score.drawn(wid(S1), middle).expect("drawn"),
            one.score.drawn(wid(D), middle).expect("drawn"),
        );
        close(
            destination.min().y - source.max().y,
            (STACK_GAP + 8.0) * scale,
            1e-6,
            "the gap",
        );
        close(destination.min().y - source.max().y, 23.6, 0.05, "on screen");
        for (frame, share, what) in [
            (source, 0.66, "the source"),
            (destination, 0.67, "the destination"),
        ] {
            close(shown(frame) / frame.size.height, share, 0.005, what);
        }

        let two = switch_down(2, PLAIN * 2f64.sqrt());
        let middle = BEGIN + PLAIN * 2f64.sqrt() * ZOOM_STRETCH * ARC_MIDDLE;
        let drawn = |w: u32| two.score.drawn(wid(w), middle).expect("drawn");
        let passed = drawn(BETWEEN + 1);
        assert_eq!(
            shown(passed),
            passed.size.height,
            "the strip passed in view whole"
        );
        close(drawn(S1).max().y - BAND, 138.4, 0.6, "the source");
        close(bottom - drawn(D).min().y, 148.0, 0.6, "the destination");
        close(
            passed.min().y - drawn(S1).max().y,
            (STACK_GAP + 8.0) * ZOOM_DEEPEST,
            1e-3,
            "above",
        );
        close(
            drawn(D).min().y - passed.max().y,
            (STACK_GAP + 8.0) * ZOOM_DEEPEST,
            1e-3,
            "below",
        );
    }

    /// The press after `flight`'s switch as the reactor sends it, at the destination: the
    /// destination's column panned `dx` along its strip and the strips above and below it, the
    /// source's and the one below, named standing still.
    fn pan_after_switch(flight: &Flight, dx: f64) -> (ReflowPlan, OverlayVector) {
        let sent = |w: u32| -> SurfaceRect {
            let at = flight.plan.destination(wid(w)).expect("sent");
            rect(at.origin.x, at.origin.y, at.size.width, at.size.height)
        };
        let from_offset = SurfacePoint::new(dx, 0.0);
        let windows = [
            on_surface(
                wid(D),
                sent(D).translate(rini_geometry::space::Vector::new(dx, 0.0)),
                None,
            ),
            on_surface(wid(S1), sent(S1), Some(1)),
            on_surface(wid(S2), sent(S2), Some(1)),
            on_surface(wid(BELOW), sent(BELOW), Some(3)),
        ];
        let travel = pan_travel(from_offset, SurfacePoint::origin());
        (
            surface_plan(&windows, from_offset, SurfacePoint::origin()),
            travel,
        )
    }

    /// Ctrl+J then Ctrl+L: a focus press merged while the switch arcs pans the destination along
    /// its strip folded into the arc, landing with it where the press sends it, and leaves every
    /// other strip where the switch sends it, moving none of them sideways. The arc plays on.
    #[test]
    fn a_horizontal_press_merged_mid_switch_folds_into_the_arc_moving_only_the_destination() {
        let mut flight = switch_down(1, PLAIN);
        let arc = flight.score.track(Node::Strip, Channel::Base).cloned();
        let now = BEGIN + 0.05;
        let playing = flight.score.clone();
        let (incoming, pan) = pan_after_switch(&flight, -862.0);
        let before: Vec<(WindowId, OverlayRect)> = [S1, S2, BELOW]
            .map(|w| (wid(w), flight.plan.destination(wid(w)).unwrap()))
            .into();
        flight.merge(&incoming, Some(pan), flight.accent, now);
        assert_eq!(
            flight.score.track(Node::Strip, Channel::Base).cloned(),
            arc,
            "the arc plays on"
        );
        for &window in &flight.drawn {
            let (was, is) = (playing.drawn(window, now), flight.score.drawn(window, now));
            assert_eq!(was, is, "{window:?} jumped");
        }
        let end = BEGIN + ARC;
        close(
            now + flight.score.remaining(now),
            end,
            1e-9,
            "lands with the arc",
        );
        assert_eq!(
            flight.score.drawn(wid(D), end),
            Some(rect(GAP - 862.0, 36.0, 1722.0, 1077.0))
        );
        for (window, sent) in before {
            assert_eq!(flight.plan.destination(window), Some(sent), "{window:?} stays");
            let x = laid(&playing, window, now).origin.x;
            for k in 0..=20 {
                let at = now + (end - now) * k as f64 / 20.0;
                assert_eq!(
                    laid(&flight.score, window, at).origin.x,
                    x,
                    "{window:?} moves sideways"
                );
            }
            assert_eq!(flight.score.drawn(window, end), Some(sent));
        }
    }

    /// Kai, 2026-09-30 21:34: "there are three distinct moves in that animation. zoom out, move,
    /// zoom in. can you make it one? sort of like an arc, one combined move". One bell: at rest at
    /// both ends, deepest at the middle, going out before it and in after it, never still between,
    /// alike either side.
    #[test]
    fn the_arc_scale_is_one_bell_deepest_at_the_middle() {
        for depth in [STRIP_ZOOM, 0.752] {
            assert_eq!(arc_scale(depth, 0.0), 1.0);
            assert_eq!(arc_scale(depth, 1.0), 1.0);
            close(arc_scale(depth, ARC_MIDDLE), depth, 1e-12, "deepest");
            let h = 1e-6;
            let rate = |f: f64| (arc_scale(depth, f + h) - arc_scale(depth, f - h)) / (2.0 * h);
            assert!(
                rate(h).abs() < 1e-4 && rate(1.0 - h).abs() < 1e-4,
                "at rest at both ends"
            );
            for k in 1..50 {
                let f = k as f64 / 100.0;
                assert!(rate(f) < -1e-3, "going out at {f}");
                assert!(rate(1.0 - f) > 1e-3, "coming in at {}", 1.0 - f);
                close(arc_scale(depth, f), arc_scale(depth, 1.0 - f), 1e-12, "alike");
            }
        }
    }

    /// Kai, 2026-10-01: "speed it up 3x". The arc runs its press's plain length stretched by
    /// `ZOOM_STRETCH`, 0.257s at the default 0.35s and 0.513s at slowdown 2, drawn from samples
    /// beginning with the motion, and every container and tile it carries runs as long.
    #[test]
    fn an_arc_runs_its_presss_plain_length_stretched() {
        for (plain, length) in [(PLAIN, 0.2567), (2.0 * PLAIN, 0.5133)] {
            let mut press = pair_move(1722.0, 1722.0, true);
            press.seconds = plain;
            let mut flight = press.flown(Cue::Move {
                window: wid(W),
                direction: Direction::Right,
            });
            let strip = flight.score.track(Node::Strip, Channel::Base).expect("an arc").clone();
            assert_eq!(strip.form(), Form::Samples);
            assert_eq!(strip.begin(), BEGIN);
            close(strip.seconds(), length, 1e-4, "its length");
            let mut tracks = 0;
            for key in flight.score.containers().collect::<Vec<_>>() {
                if let Some(track) = flight.score.track(Node::Container(key), Channel::Base) {
                    assert_eq!((track.begin(), track.seconds()), (BEGIN, strip.seconds()));
                    tracks += 1;
                }
            }
            assert_eq!(tracks, 3, "the pair and the rider");
            let resized = rect(4.0, 32.0, 400.0, 1081.0);
            let incoming = reflow_plan(
                &[Resolved::tiled(
                    wid(9),
                    resized,
                    rect(4.0, 32.0, 600.0, 1081.0),
                )],
                DISPLAY,
            );
            flight.drawn.push(wid(9));
            let pass = Pass {
                plan: &incoming,
                pan: None,
                focus: None,
                viewport: VIEWPORT,
                drawn: &flight.drawn,
                seconds: plain,
                cue: Cue::Plain,
                accent: flight.accent,
            };
            let bent = flight.zoom.retarget(&flight.score, &flight.plan, &pass, BEGIN);
            let joined = bent
                .edits
                .iter()
                .find_map(|edit| match edit {
                    Edit::Add(track) if track.node() == Node::Tile(wid(9)) => Some(track.seconds()),
                    _ => None,
                })
                .expect("the resize joins");
            close(joined, length, 1e-4, "a tile runs as long");
        }
    }

    /// Kai, 2026-09-30 07:39: "way too much zoom". As deep as the pair needs: shallowest for two
    /// halves, out to keep two thirds of the narrower column in view for wider pairs, never past
    /// the deepest, and shallowest for a pair the narrower column is no part of.
    #[test]
    fn the_arc_goes_as_deep_as_the_pair_needs() {
        assert_eq!(zoom_depth(WIDTH, 1722.0, 859.5), STRIP_ZOOM, "halves");
        close(zoom_depth(WIDTH, 2585.0, 860.0), 0.859, 5e-4, "full and half");
        close(zoom_depth(WIDTH, 3447.0, 1722.0), 0.7516, 5e-5, "full and full");
        assert_eq!(zoom_depth(WIDTH, 10_000.0, 1722.0), ZOOM_DEEPEST, "the deepest");
        assert_eq!(zoom_depth(WIDTH, 900.0, 1722.0), STRIP_ZOOM, "degenerate");
        assert_eq!(zoom_depth(WIDTH, 0.0, 0.0), STRIP_ZOOM, "no pair");
        assert_eq!(zoom_depth(f64::NAN, 1722.0, 859.5), STRIP_ZOOM, "no display");
    }

    /// Kai, 2026-09-30 09:14: "i really like how the 50/50 windows swap looks like". Two halves at
    /// the start of the strip go out to the shallowest and nothing but the pair moves: both cross
    /// from the slots they held to each other's on `ZOOM_CURVE`, exactly at every sample.
    #[test]
    fn a_half_and_half_move_at_the_start_of_the_strip_moves_only_the_pair() {
        let flight = pair_move(859.5, 859.5, false).flown(Cue::Move {
            window: wid(W),
            direction: Direction::Left,
        });
        let (depth, _, swap) = arc_of(flight.accent);
        assert_eq!(depth, STRIP_ZOOM);
        let swap = swap.expect("a swap");
        assert_eq!((swap.moved, swap.passed), (wid(W), wid(N)));
        assert_eq!(swap.moved_by, OverlayVector::new(-862.5, 0.0));
        assert_eq!(swap.passed_by, OverlayVector::new(862.5, 0.0));
        let count = sample_count(ARC);
        for (window, by) in [(W, -862.5), (N, 862.5)] {
            let key = key_of(&flight.plan, wid(window));
            let track = flight.score.track(Node::Container(key), Channel::Base).expect("crosses");
            let Value::Point(from) = track.keys()[0].1 else {
                panic!()
            };
            for (k, (_, value)) in track.keys().iter().enumerate() {
                let Value::Point(at) = value else { panic!("{value:?}") };
                let want = from.x + by * ZOOM_CURVE.ease(k as f64 / count as f64);
                close(at.x, want, 0.5, &format!("{window} at key {k}"));
                assert_eq!(at.y, 0.0);
            }
            let start = flight.posed.drawn(wid(window), BEGIN).unwrap().origin.x;
            let lands = flight.score.drawn(wid(window), BEGIN + ARC).unwrap().origin.x;
            close(lands - start, by, 0.5, "to the other's slot");
        }
        let still = key_of(&flight.plan, wid(C));
        assert!(
            flight.score.track(Node::Container(still), Channel::Base).is_none(),
            "C stays"
        );
    }

    /// Kai, 2026-09-30 07:39: "so it'd look like the window is lifted above the strip", and at
    /// 11:13 "reduce the zoomout gap to 0". The window a move moves is lifted on the arc's bell,
    /// sampled as the zoom is and set down where it lands, and drawn zoomed as far as the strip,
    /// its lift its shadow alone; the window it passes and a focus press's lift nothing.
    #[test]
    fn a_move_lifts_the_window_it_moves_on_the_arc() {
        let flight = pair_move(1722.0, 1722.0, false).flown(Cue::Move {
            window: wid(W),
            direction: Direction::Left,
        });
        let lift = flight.score.track(Node::Lift(wid(W)), Channel::Base).expect("lifted");
        assert_eq!((lift.begin(), lift.form()), (BEGIN, Form::Samples));
        close(lift.seconds(), ARC, 1e-12, "the arc's length");
        assert_eq!(flight.score.model(Node::Lift(wid(W))), Some(Value::Number(0.0)));
        let count = sample_count(ARC);
        assert_eq!(lift.keys().len(), count + 1);
        for (k, (_, value)) in lift.keys().iter().enumerate() {
            let want = if k == count {
                0.0
            } else {
                nudge_displacement(k as f64 / count as f64)
            };
            assert_eq!(*value, Value::Number(want), "key {k}");
        }
        let middle = BEGIN + ARC / 2.0;
        let (moved, passed) = (
            flight.score.drawn(wid(W), middle).unwrap(),
            flight.score.drawn(wid(N), middle).unwrap(),
        );
        close(
            moved.size.width / 1722.0,
            passed.size.width / 1722.0,
            1e-12,
            "no depth",
        );
        assert!(
            flight.score.model(Node::Lift(wid(N))).is_none(),
            "the passed one"
        );

        let focused = focus_right(859.5).flown(Cue::Focus { from: wid(W), to: wid(N) });
        for window in [W, N, C] {
            assert!(
                focused.score.model(Node::Lift(wid(window))).is_none(),
                "{window}"
            );
        }
    }

    /// Kai, 2026-09-30 09:14: "for two full-sized windows swap i imagine both of them clipped by
    /// 1/3-1/4 or so". Two full-width columns go out to 0.752 and the view is centred on the pair,
    /// a third of each off the display at the middle, 431pt; a full-width column past a half goes
    /// out to 0.859, 246pt off each side, the half keeping two thirds and the full one five sixths.
    #[test]
    fn a_wide_pair_is_centred_at_the_middle_clipped_alike_on_both_sides() {
        for (w, n, depth, hidden) in [
            (1722.0, 1722.0, 0.7516, 431.0),
            (1722.0, 860.0, 0.859, 246.0),
        ] {
            let press = pair_move(w, n, true);
            let (got, share, _) = arc_of(
                stage(
                    &Cue::Move {
                        window: wid(W),
                        direction: Direction::Right,
                    },
                    &press.strip(),
                )
                .accent,
            );
            close(got, depth, 5e-4, "how deep");
            let view = press.travel * (1.0 - share);
            let drawn = |x: f64| CENTRE + got * (x - view - CENTRE);
            let (left, right) = (-n, GAP + w);
            let (off_left, off_right) = (-drawn(left), drawn(right) - WIDTH);
            close(off_left, off_right, 0.01, "alike on both sides");
            close(off_left, hidden, 0.5, "off each side");
            close(
                off_left / got / n,
                1.0 / 3.0,
                0.002,
                "the narrower keeps two thirds",
            );
            if n < w {
                close(
                    off_right / got / w,
                    1.0 / 6.0,
                    0.002,
                    "the wider keeps five sixths",
                );
            }
        }
    }

    /// A tiling area off the display's centre, by a Dock at the side or uneven outer gaps: two
    /// full-width columns are still clipped alike on both sides of it at the arc's middle, as the
    /// flight draws them zoomed about the display's centre.
    #[test]
    fn a_wide_pair_is_clipped_alike_about_a_tiling_area_off_the_display_centre() {
        let docked = Area {
            frame: rect(0.0, 0.0, WIDTH - 70.0, 1117.0),
            ..AREA
        };
        let uneven = Area { outer_right: 40.0, ..AREA };
        for area in [docked, uneven] {
            let (lo, hi) = (
                area.frame.min().x + area.outer_left,
                area.frame.max().x - area.outer_right,
            );
            let width = hi - lo;
            let press = Press {
                area,
                ..pair_move(width, width, true)
            };
            let flight = press.flown(Cue::Move {
                window: wid(W),
                direction: Direction::Right,
            });
            let (middle, end) = (BEGIN + ARC / 2.0, BEGIN + 2.0 * PLAIN);
            let Some(Value::Zoom(zoom)) = flight.score.sample(Node::Strip, middle) else {
                panic!("no zoom at the middle");
            };
            let x = |window: u32, at: f64| flight.score.drawn(wid(window), at).unwrap().min().x;
            let carried = (x(C, middle) - zoom.shift.x) / zoom.scale - x(C, end);
            let (left, right) = (x(N, end), x(W, end) + width);
            let off_left = lo - zoom.draw(OverlayPoint::new(left + carried, 0.0)).x;
            let off_right = zoom.draw(OverlayPoint::new(right + carried, 0.0)).x - hi;
            assert!(off_left > 100.0, "{area:?}: {off_left:.1} off the left");
            close(off_left, off_right, 0.01, "alike on both sides");
        }
    }

    /// The travel every container carries runs on `camera_progress`, the swap on top of it on
    /// `ZOOM_CURVE`: the rider exactly at every sample, the pair as its slot and its crossing add up.
    /// Frame zero draws every window where the pose has it, and the last frame where the plan
    /// sends it.
    #[test]
    fn every_container_carries_the_travel_on_the_camera_and_the_pair_its_swap_on_top() {
        let flight = pair_move(1722.0, 1722.0, true).flown(Cue::Move {
            window: wid(W),
            direction: Direction::Right,
        });
        let (_, share, _) = arc_of(flight.accent);
        close(share, 0.5, 1e-9, "a straight pan");
        let count = sample_count(ARC);
        for (window, by) in [(C, 0.0), (N, -1725.0), (W, 1725.0)] {
            let key = key_of(&flight.plan, wid(window));
            let track = flight.score.track(Node::Container(key), Channel::Base).expect("moves");
            let Some(Value::Point(to)) = flight.score.model(Node::Container(key)) else {
                panic!("placed")
            };
            let from = match track.keys()[0].1 {
                Value::Point(from) => from,
                other => panic!("{other:?}"),
            };
            for (k, (_, value)) in track.keys().iter().enumerate() {
                let f = k as f64 / count as f64;
                let want = from.x
                    + (to.x - from.x - by) * camera_progress(share, f)
                    + by * ZOOM_CURVE.ease(f);
                let Value::Point(at) = value else { panic!() };
                close(at.x, want, 1e-9, &format!("{window} at key {k}"));
            }
        }
        for window in [C, N, W] {
            let (start, end) = (
                flight.posed.drawn(wid(window), BEGIN).expect("posed"),
                flight.score.drawn(wid(window), BEGIN).expect("drawn"),
            );
            close(start.origin.x, end.origin.x, 1e-9, "frame zero");
            let lands = flight.score.drawn(wid(window), BEGIN + ARC).expect("drawn");
            let sent = flight.plan.destination(wid(window)).expect("sent");
            close(lands.origin.x, sent.origin.x, 0.5, "lands");
            assert_eq!(lands.size, sent.size);
        }
    }

    /// The share of the travel carried at the middle never makes it turn back: between a quarter
    /// and three quarters, whatever the view, and the travel carried rises from 0 to 1 over a grid
    /// of starts and views. Half is the ease-in-out. A view that is not a number is refused.
    #[test]
    fn the_travel_carried_goes_one_way() {
        for start in [-1725.0, -862.5, -1.0, 0.0, 862.5] {
            for view in [-2000.0, -862.5, -431.25, -117.4, 0.0, 117.4, 431.25, 2000.0] {
                let share = camera_share(start, view);
                assert!((0.25..=0.75).contains(&share), "{start} through {view}: {share}");
                let mut last = 0.0;
                for k in 1..=1000 {
                    let carried = camera_progress(share, k as f64 / 1000.0);
                    assert!(carried >= last - 1e-12, "{start} through {view} turned at {k}");
                    last = carried;
                }
                assert!((last - 1.0).abs() < 1e-12);
            }
        }
        assert_eq!(
            camera_share(-862.5, -862.5 * 0.9),
            0.25,
            "no further than a quarter"
        );
        assert_eq!(
            camera_share(-862.5, -862.5 * 0.1),
            0.75,
            "no further than three quarters"
        );
        for k in 0..=20 {
            let f = k as f64 / 20.0;
            close(camera_progress(0.5, f), EASE_IN_OUT.ease(f), 1e-12, "straight");
        }
        assert!(camera_share(-862.5, f64::NAN).is_nan());
        let mut press = pair_move(1722.0, 860.0, true);
        press.travel = f64::NAN;
        let moved = Cue::Move {
            window: wid(W),
            direction: Direction::Right,
        };
        let staging = stage(&moved, &press.strip());
        assert_eq!(staging.accent, None, "refused");
        assert_eq!(staging.from.len(), 2, "the pair still crosses");
    }

    /// Kai, 2026-09-30 11:19: "can you try to change the normal navigation as well to zoom out,
    /// move, and then zoom back in?" A focus press that scrolls goes as deep as a move of the same
    /// two columns and carries half its travel by the middle, a straight eased pan; one that does
    /// not scroll, or whose window is not on the strip, plays no arc.
    #[test]
    fn a_focus_press_that_scrolls_pans_on_an_arc_as_deep_as_a_move_of_its_two_columns() {
        for w in [859.5, 1722.0] {
            let press = focus_right(w);
            let cue = Cue::Focus { from: wid(W), to: wid(N) };
            let staging = stage(&cue, &press.strip());
            assert!(staging.from.is_empty() && staging.held.is_empty());
            let (depth, share, swap) = arc_of(staging.accent);
            let moved = pair_move(w, w, true);
            let (as_deep, _, _) = arc_of(
                stage(
                    &Cue::Move {
                        window: wid(W),
                        direction: Direction::Right,
                    },
                    &moved.strip(),
                )
                .accent,
            );
            close(depth, as_deep, 1e-9, "as deep as the move");
            assert_eq!((share, swap), (ARC_MIDDLE, None));
        }
        let mut still = focus_right(859.5);
        still.travel = 0.5;
        let cue = Cue::Focus { from: wid(W), to: wid(N) };
        assert_eq!(stage(&cue, &still.strip()).accent, None, "no scroll");
        let press = focus_right(859.5);
        let away = Cue::Focus { from: wid(W), to: wid(9) };
        assert_eq!(stage(&away, &press.strip()).accent, None, "off the strip");
        assert_eq!(stage(&Cue::Plain, &press.strip()), Staging::default());
    }

    /// A focus press flown: every column pans on the ease-in-out under the arc, and nothing crosses.
    #[test]
    fn a_focus_press_flies_every_column_on_the_ease_in_out() {
        let flight = focus_right(859.5).flown(Cue::Focus { from: wid(W), to: wid(N) });
        let key = key_of(&flight.plan, wid(W));
        assert_eq!(key, key_of(&flight.plan, wid(N)), "one piece");
        let track = flight.score.track(Node::Container(key), Channel::Base).expect("pans");
        let Some(Value::Point(to)) = flight.score.model(Node::Container(key)) else {
            panic!("placed")
        };
        assert_eq!(to.x, -863.0, "on a whole point");
        let count = sample_count(ARC);
        for (k, (_, value)) in track.keys().iter().enumerate() {
            let Value::Point(at) = value else { panic!() };
            let f = k as f64 / count as f64;
            close(at.x, to.x * EASE_IN_OUT.ease(f), 1e-9, &format!("key {k}"));
        }
        close(
            scale_at(&flight.score, BEGIN + ARC / 2.0),
            STRIP_ZOOM,
            1e-3,
            "deepest",
        );
    }

    /// A strip flight whose windows stand still, with a push against `wall` waiting and `cue`
    /// shown: a bounce flight as the engine starts one.
    fn bounce(wall: OverlayVector, cue: Cue) -> Flight {
        let mut press = focus_right(859.5);
        press.travel = 0.0;
        let mut flight = press.flown(Cue::Plain);
        let mut zoom = ZoomArc::default();
        let accents = [Accent::Push { wall, seconds: PLAIN }];
        let stage = Stage {
            plan: &flight.plan,
            drawn: &flight.drawn,
            seconds: PLAIN,
            cue,
            accents: &accents,
            viewport: VIEWPORT,
        };
        flight.score = flight.posed.clone();
        for edit in zoom.compose(&flight.posed, &stage, BEGIN) {
            flight.score.apply(&edit);
        }
        flight.zoom = zoom;
        flight
    }

    /// Kai, 2026-09-30 10:10: "the little bounce at the ends of the strip, can you make it work in
    /// the zoomed out state, please?", and 2026-10-17: "it applies to both horizontal and vertical
    /// navigation". A push at a side with a press along the strip, or at an end of the stack with a
    /// switch, plays over a shallow arc ending by the time the bounce does, nothing else moving,
    /// and reaches the edge bounce's 72pt on screen where it turns. A plain push, a press along the
    /// strip against the top or bottom, or a switch against a side plays no arc.
    #[test]
    fn a_push_at_an_end_bounces_zoomed_out_as_far_as_at_rest() {
        let side = OverlayVector::new(-72.0, 0.0);
        let stack = OverlayVector::new(0.0, 72.0);
        let along = Cue::Focus { from: wid(N), to: wid(N) };
        let zoomed = [
            (side, along),
            (side, Cue::Move {
                window: wid(N),
                direction: Direction::Left,
            }),
            (stack, Cue::Switch),
            (-stack, Cue::Switch),
        ];
        let centre = VIEWPORT.center();
        for (wall, cue) in zoomed {
            let tag = format!("{wall:?} {cue:?}");
            let toward = wall / 72.0;
            let along_wall = |p: OverlayPoint| (p - centre).dot(toward);
            let flight = bounce(wall, cue);
            let strip = flight.score.track(Node::Strip, Channel::Base).expect("an arc");
            close(strip.seconds(), ARC, 1e-9, &tag);
            assert!(strip.ends() <= BEGIN + PLAIN, "{tag}: no longer than the bounce");
            close(
                scale_at(&flight.score, BEGIN + BOUNCE_TURN * PLAIN),
                STRIP_ZOOM,
                1e-3,
                &format!("{tag}: shallow, deepest where it turns"),
            );
            let key = key_of(&flight.plan, wid(N));
            assert!(flight.score.track(Node::Container(key), Channel::Base).is_none());
            let turn = BEGIN + BOUNCE_TURN * PLAIN;
            let (was, is) = (
                laid(&flight.score, wid(N), turn),
                flight.score.drawn(wid(N), turn),
            );
            let at_rest = laid(&flight.posed, wid(N), turn);
            let scale = scale_at(&flight.score, turn);
            close(
                along_wall(is.expect("drawn").origin) - scale * along_wall(at_rest.origin),
                72.0,
                0.5,
                &format!("{tag}: on screen"),
            );
            close(
                along_wall(was.origin) - along_wall(at_rest.origin),
                72.0 / scale,
                1e-6,
                &format!("{tag}: in the strip"),
            );
        }
        for (wall, cue) in [
            (side, Cue::Plain),
            (stack, Cue::Plain),
            (stack, along),
            (side, Cue::Switch),
        ] {
            let flight = bounce(wall, cue);
            assert!(
                flight.score.track(Node::Strip, Channel::Base).is_none(),
                "{wall:?} {cue:?}"
            );
        }
    }

    /// Kai, 2026-10-03: "if you can make it react to every single press like a pure CSS based
    /// bouncer would, that'd be fantastic". A burst at the end re-kicks each bounce from where it is
    /// drawn, at once, with the strip zoomed where it turns. A press while the arc zooms out starts
    /// no second one; one while it zooms back in, or after it lands, zooms the strip out again from
    /// where it is drawn, and the strip is home an arc after the last. A plain burst zooms once. A
    /// burst of switches at an end of the stack plays as a burst at a side does.
    #[test]
    fn a_burst_at_an_end_bounces_from_where_it_is_drawn_zoomed_out_at_every_turn() {
        let side = OverlayVector::new(-72.0, 0.0);
        let at_end = Cue::Focus { from: wid(N), to: wid(N) };
        let bursts = [
            (side, at_end, at_end),
            (side, at_end, Cue::Plain),
            (OverlayVector::new(0.0, 72.0), Cue::Switch, Cue::Switch),
        ];
        for (wall, first, cue) in bursts {
            let mut flight = bounce(wall, first);
            let first = flight.score.track(Node::Strip, Channel::Base).cloned();
            let key = key_of(&flight.plan, wid(N));
            let burst = [0.05, 0.09, 0.2, 0.3, 0.5].map(|t| BEGIN + t);
            for now in burst {
                let tag = format!("{wall:?} {cue:?} at {:.2}", now - BEGIN);
                let before = flight.score.drawn(wid(N), now).expect("drawn");
                let (playing, scale_before) = (
                    flight.score.track(Node::Strip, Channel::Base).cloned(),
                    scale_at(&flight.score, now),
                );
                let deeper = deepening(&flight.score, PLAIN, now);
                let drawn = match flight.score.track(Node::Container(key), Channel::Bounce) {
                    Some(track) => match track.at(now) {
                        Some(Value::Point(p)) => p.to_vector(),
                        _ => OverlayVector::zero(),
                    },
                    None => OverlayVector::zero(),
                };
                let edits = flight.zoom.push(&flight.score, &cue, wall, PLAIN, now);
                for edit in &edits {
                    flight.score.apply(edit);
                }
                let after = flight.score.drawn(wid(N), now).expect("drawn");
                close(after.origin.x, before.origin.x, 1e-9, &format!("{tag}: no jump"));
                close(after.origin.y, before.origin.y, 1e-9, &format!("{tag}: no jump"));
                close(scale_at(&flight.score, now), scale_before, 1e-9, &tag);
                let bounce =
                    flight.score.track(Node::Container(key), Channel::Bounce).expect("kicked");
                assert_eq!(bounce.begin(), now, "{tag}: at once");
                let turn = now + BOUNCE_TURN * PLAIN;
                let scale = scale_at(&flight.score, turn);
                let want = Bounce::from_drawn(
                    drawn,
                    wall / scale,
                    std::time::Duration::from_secs_f64(PLAIN),
                )
                .track(key, now);
                assert_eq!(*bounce, want, "{tag}");
                let arc = flight.score.track(Node::Strip, Channel::Base).cloned();
                if cue == Cue::Plain {
                    assert_eq!(arc, first, "{tag}: one arc");
                } else if deeper {
                    assert_eq!(arc, playing, "{tag}: no second arc");
                } else {
                    close(
                        scale,
                        STRIP_ZOOM,
                        1e-3,
                        &format!("{tag}: zoomed out where it turns"),
                    );
                    let arc = arc.expect("an arc");
                    assert_eq!(arc.begin(), now, "{tag}: from now");
                    assert!(arc.ends() <= now + ARC + 1e-9, "{tag}: no longer than an arc");
                }
            }
            if cue != Cue::Plain {
                let last = burst[burst.len() - 1];
                let ends = flight.score.track(Node::Strip, Channel::Base).map(Track::ends);
                assert!(ends.is_some_and(|end| end > last && end <= last + ARC + 1e-9));
            }
        }
    }

    /// A pan the same way as the strip's travel at `now`, as the reactor builds the next press: every
    /// window from where the last pass sends it, on by another column.
    fn pan(flight: &Flight, dx: f64) -> ReflowPlan { panned(flight, OverlayVector::new(dx, 0.0)) }

    /// Every window of `flight` from where the last pass sends it, on by `d`.
    fn panned(flight: &Flight, d: OverlayVector) -> ReflowPlan {
        let requests: Vec<Resolved> = flight
            .drawn
            .iter()
            .map(|&window| {
                let sent = flight.plan.destination(window).expect("sent");
                let from = rect(sent.origin.x, sent.origin.y, sent.size.width, sent.size.height);
                let to = rect(
                    from.origin.x + d.x,
                    from.origin.y + d.y,
                    from.size.width,
                    from.size.height,
                );
                Resolved::tiled(window, from, to)
            })
            .collect();
        reflow_plan(&requests, DISPLAY)
    }

    /// The track `edits` give `window`'s container on `flight`.
    fn container_track(flight: &Flight, edits: &[Edit], window: WindowId) -> Track {
        let key = key_of(&flight.plan, window);
        edits
            .iter()
            .find_map(|edit| match edit {
                Edit::Add(track) if track.node() == Node::Container(key) => Some(track.clone()),
                _ => None,
            })
            .expect("bent")
    }

    impl Flight {
        /// `incoming` merged at `now`, with `pan` and `accent`.
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
            let bent = self.zoom.retarget(&self.score, &self.plan, &pass, now);
            for edit in &bent.edits {
                self.score.apply(edit);
            }
            self.plan = bent.plan.clone();
            bent
        }
    }

    /// "The zoom look" in `specs/animation.md`, a press merged into an arc: a press the same way
    /// while the arc pans folds into it. Nothing jumps, what it adds leaves from rest, covering less
    /// in the first frame than an even speed landing with the arc would, the strip goes one way and
    /// lands with the arc, where the press sends it.
    #[test]
    fn a_press_the_same_way_folds_into_the_arc_and_lands_with_it() {
        let d = -862.5;
        for press in [0.0, 0.03, 0.08, ARC * (1.0 - FOLDED_PAN_MIN)] {
            let mut flight = focus_right(859.5).flown(Cue::Focus { from: wid(W), to: wid(N) });
            let now = BEGIN + press;
            let (playing, arc) = (
                flight.score.clone(),
                flight.score.track(Node::Strip, Channel::Base).cloned(),
            );
            let incoming = pan(&flight, d);
            flight.merge(&incoming, Some(OverlayVector::new(d, 0.0)), None, now);
            assert_eq!(flight.score.track(Node::Strip, Channel::Base).cloned(), arc);
            let (was, is) = (laid(&playing, wid(N), now), laid(&flight.score, wid(N), now));
            close(is.origin.x, was.origin.x, 1e-9, "no jump");
            let frame = now + SAMPLE_STEP;
            let added = laid(&flight.score, wid(N), frame).origin.x
                - laid(&playing, wid(N), frame).origin.x;
            let end = BEGIN + ARC;
            let even = d.abs() * SAMPLE_STEP / (end - now);
            assert!(
                added.abs() < even,
                "at {press}: {added:.2}pt in a frame, {even:.2} even"
            );
            close(
                now + flight.score.remaining(now),
                end,
                1e-9,
                "lands with the arc",
            );
            let mut last = is.origin.x;
            for k in 1..=200 {
                let x = laid(&flight.score, wid(N), now + (end - now) * k as f64 / 200.0).origin.x;
                assert!(x <= last + 1e-9, "at {press}: turned back at {k}");
                last = x;
            }
            let lands = flight.score.drawn(wid(N), end).expect("drawn");
            close(
                lands.origin.x,
                flight.plan.destination(wid(N)).unwrap().origin.x,
                0.5,
                "sent",
            );
        }
    }

    /// Ctrl+L, then Ctrl+J while the arc pans: folded in, the press back would start from rest under
    /// a strip still going the old way, and it would carry on before turning. It turns on a spring
    /// let go as fast as the strip is drawn moving, carrying on less, and lands where it started.
    #[test]
    fn a_press_back_turns_the_strip_on_a_spring() {
        let back = 862.5;
        for press in [0.02, 0.04, 0.067, 0.1] {
            let mut flight = focus_right(859.5).flown(Cue::Focus { from: wid(W), to: wid(N) });
            let now = BEGIN + press;
            let playing = flight.score.clone();
            let key = key_of(&flight.plan, wid(N));
            let incoming = pan(&flight, back);
            let bent = flight.merge(&incoming, Some(OverlayVector::new(back, 0.0)), None, now);
            let track = bent
                .edits
                .iter()
                .find_map(|edit| match edit {
                    Edit::Add(track) if track.node() == Node::Container(key) => Some(track.clone()),
                    _ => None,
                })
                .expect("bent");
            let velocity = playing.rate(Node::Container(key), now);
            let Some(Value::Point(to)) = flight.score.model(Node::Container(key)) else {
                panic!()
            };
            let Some(Value::Point(from)) = playing.along(Node::Container(key), now) else {
                panic!()
            };
            let spring = Leg::Spring {
                from,
                to,
                velocity: OverlayVector::new(velocity[0], velocity[1]),
                begin: now,
                omega: spring_omega(PLAIN),
            };
            assert_eq!(track, spring.track(key, SAMPLE_STEP), "at {press}");
            let pressed = laid(&playing, wid(N), now).origin.x;
            let carried = |score: &Score| {
                (0..=400)
                    .map(|k| pressed - laid(score, wid(N), now + 0.4 * k as f64 / 400.0).origin.x)
                    .fold(0.0f64, f64::max)
            };
            let mut folded = playing.clone();
            let end = BEGIN + ARC;
            let old = playing.track(Node::Container(key), Channel::Base).cloned().unwrap();
            let Some(Value::Point(was)) = playing.model(Node::Container(key)) else {
                panic!()
            };
            let count = sample_count(end - now);
            let values = (0..=count).map(|k| {
                let f = k as f64 / count as f64;
                let at = match old.at(now + (end - now) * f) {
                    Some(Value::Point(p)) => p,
                    _ => was,
                };
                Value::Point(at + OverlayVector::new(back, 0.0) * EASE_IN_OUT.ease(f))
            });
            folded.apply(&Edit::Add(Track::samples(
                Node::Container(key),
                Channel::Base,
                values,
                now,
                end - now,
            )));
            let (sprung, eased) = (carried(&flight.score), carried(&folded));
            assert!(
                sprung < eased,
                "at {press}: sprung {sprung:.1}, folded {eased:.1}"
            );
            assert!(sprung < 30.0, "at {press}: carried on {sprung:.1}pt");
            let home = flight.score.drawn(wid(N), now + flight.score.remaining(now)).unwrap();
            close(
                home.origin.x,
                flight.posed.drawn(wid(N), BEGIN).unwrap().origin.x,
                0.5,
                "home",
            );
        }
    }

    /// With less than `FOLDED_PAN_MIN` of the arc left a press the same way goes on a spring, and so
    /// does a switch: neither touches the arc.
    #[test]
    fn a_late_press_or_switch_goes_on_a_spring_and_leaves_the_arc() {
        let late = BEGIN + ARC * (1.0 - FOLDED_PAN_MIN) + 0.005;
        for pan_by in [
            OverlayVector::new(-862.5, 0.0),
            OverlayVector::new(0.0, -PITCH),
        ] {
            let mut flight = focus_right(859.5).flown(Cue::Focus { from: wid(W), to: wid(N) });
            let arc = flight.score.track(Node::Strip, Channel::Base).cloned();
            let before = flight.score.drawn(wid(N), late).unwrap();
            let incoming = panned(&flight, pan_by);
            let bent = flight.merge(&incoming, Some(pan_by), None, late);
            let track = container_track(&flight, &bent.edits, wid(N));
            assert!(
                track.ends() > BEGIN + ARC,
                "{pan_by:?}: a spring, landing after the arc"
            );
            assert_eq!(flight.score.track(Node::Strip, Channel::Base).cloned(), arc);
            let after = flight.score.drawn(wid(N), late).unwrap();
            close(after.origin.x, before.origin.x, 1e-9, "no jump");
            close(after.origin.y, before.origin.y, 1e-9, "no jump");
        }
    }

    /// Ctrl+L then Ctrl+Opt+J while the arc pans: a switch merged with a quarter of the arc left
    /// folds into it along the stack as a press the same way does along the strip, its travel
    /// added from rest, and lands with the arc where the switch sends it, the arc playing on;
    /// a switch merged into a switch's arc does too.
    #[test]
    fn a_switch_merged_with_a_quarter_left_folds_into_the_arc_and_lands_with_it() {
        let d = OverlayVector::new(0.0, -PITCH);
        let switch_arc = stage(&Cue::Switch, &Strip {
            windows: &[],
            pair: &[],
            area: usable_area(),
            travel: d,
            seconds: PLAIN,
        })
        .accent;
        for press in [0.02, 0.05, ARC * (1.0 - FOLDED_PAN_MIN)] {
            for (mut flight, window) in [
                (
                    focus_right(859.5).flown(Cue::Focus { from: wid(W), to: wid(N) }),
                    wid(N),
                ),
                (switch_down(1, PLAIN), wid(D)),
                (switch_down(1, PLAIN), wid(BELOW)),
            ] {
                let now = BEGIN + press;
                let tag = format!("{window:?} at {press}");
                let (playing, arc) = (
                    flight.score.clone(),
                    flight.score.track(Node::Strip, Channel::Base).cloned(),
                );
                let incoming = match flight.plan.member(wid(D)) {
                    Some(_) => switch_on(&flight),
                    None => panned(&flight, d),
                };
                flight.merge(&incoming, Some(d), switch_arc, now);
                assert_eq!(
                    flight.score.track(Node::Strip, Channel::Base).cloned(),
                    arc,
                    "{tag}"
                );
                let (was, is) = (laid(&playing, window, now), laid(&flight.score, window, now));
                close(is.origin.y, was.origin.y, 1e-9, &format!("{tag}: no jump"));
                let frame = now + SAMPLE_STEP;
                let added = laid(&flight.score, window, frame).origin.y
                    - laid(&playing, window, frame).origin.y;
                let end = BEGIN + ARC;
                let even = PITCH * SAMPLE_STEP / (end - now);
                assert!(
                    added.abs() < even,
                    "{tag}: {added:.2}pt in a frame, {even:.2} even"
                );
                close(
                    now + flight.score.remaining(now),
                    end,
                    1e-9,
                    &format!("{tag}: with the arc"),
                );
                let mut last = is.origin.y;
                for k in 1..=200 {
                    let y =
                        laid(&flight.score, window, now + (end - now) * k as f64 / 200.0).origin.y;
                    assert!(y <= last + 1e-9, "{tag}: turned back at {k}");
                    last = y;
                }
                let (lands, sent) = (
                    flight.score.drawn(window, end).expect("drawn"),
                    flight.plan.destination(window).expect("sent"),
                );
                close(lands.origin.y, sent.origin.y, 0.5, &format!("{tag}: sent"));
                close(lands.origin.x, sent.origin.x, 0.5, &format!("{tag}: sent"));
            }
        }
    }

    /// The switch on down after `flight`'s, as the reactor sends it: the destination's strip a
    /// strip beside, the one below it the destination, the source's strip above, and no floating
    /// window, both of its strips having none, from where `flight` sends each.
    fn switch_on(flight: &Flight) -> ReflowPlan {
        let sent = |w: u32| -> SurfaceRect {
            let at = flight.plan.destination(wid(w)).expect("sent");
            rect(at.origin.x, at.origin.y, at.size.width, at.size.height)
        };
        let windows = [
            on_surface(wid(D), sent(D), Some(2)),
            on_surface(wid(BELOW), sent(BELOW), None),
            on_surface(wid(S1), sent(S1), Some(1)),
            on_surface(wid(S2), sent(S2), Some(1)),
        ];
        surface_plan(&windows, SurfacePoint::origin(), SurfacePoint::new(0.0, PITCH))
    }

    /// Ctrl+Opt+J then Ctrl+Opt+K while the switch arcs: folded in, the switch back would start
    /// from rest under a stack still going down, so it turns on a spring let go as fast as the
    /// stack is drawn moving, and lands where it started.
    #[test]
    fn a_switch_back_mid_switch_turns_the_stack_on_a_spring() {
        let mut flight = switch_down(1, PLAIN);
        let now = BEGIN + 0.08;
        let playing = flight.score.clone();
        let back = OverlayVector::new(0.0, PITCH);
        let incoming = panned(&flight, back);
        let bent = flight.merge(&incoming, Some(back), None, now);
        let track = container_track(&flight, &bent.edits, wid(D));
        let key = key_of(&flight.plan, wid(D));
        let velocity = playing.rate(Node::Container(key), now);
        assert!(velocity[1] < -1000.0, "the stack is going up: {velocity:?}");
        let Some(Value::Point(from)) = playing.along(Node::Container(key), now) else {
            panic!()
        };
        let Some(Value::Point(to)) = flight.score.model(Node::Container(key)) else {
            panic!()
        };
        let spring = Leg::Spring {
            from,
            to,
            velocity: OverlayVector::new(velocity[0], velocity[1]),
            begin: now,
            omega: spring_omega(PLAIN),
        };
        assert_eq!(track, spring.track(key, SAMPLE_STEP));
        let home = flight.score.drawn(wid(D), now + flight.score.remaining(now)).unwrap();
        close(
            home.origin.y,
            flight.posed.drawn(wid(D), BEGIN).unwrap().origin.y,
            0.5,
            "home",
        );
    }

    /// A switch merged into a flight with no arc playing plays a shallow arc of its own from rest,
    /// as a move or a focus press does.
    #[test]
    fn a_switch_merged_with_no_arc_playing_plays_a_shallow_one() {
        let mut plain = focus_right(859.5).flown(Cue::Plain);
        assert!(plain.score.track(Node::Strip, Channel::Base).is_none());
        let now = BEGIN + 0.1;
        let d = OverlayVector::new(0.0, -PITCH);
        let accent = stage(&Cue::Switch, &Strip {
            windows: &[],
            pair: &[],
            area: usable_area(),
            travel: d,
            seconds: PLAIN,
        })
        .accent;
        let incoming = panned(&plain, d);
        plain.merge(&incoming, Some(d), accent, now);
        let arc = plain.score.track(Node::Strip, Channel::Base).expect("a shallow arc").clone();
        assert_eq!(arc.begin(), now);
        close(arc.seconds(), ARC, 1e-9, "its length");
        assert_eq!(arc.keys()[0].1, Value::Zoom(Zoom::REST), "from rest");
        close(
            scale_at(&plain.score, now + ARC / 2.0),
            STRIP_ZOOM,
            1e-3,
            "shallow",
        );
    }

    /// A move or a focus press that scrolls, merged into a flight with no arc playing, plays a
    /// shallow arc from now, at rest at its first frame, a move lifting the window it moves on it;
    /// merged into one playing, it leaves that arc as it is.
    #[test]
    fn a_press_merged_mid_flight_zooms_only_when_no_arc_plays() {
        let moved = Accent::Arc {
            depth: 0.7516,
            share: 0.5,
            swap: Some(Swap {
                moved: wid(N),
                passed: wid(W),
                moved_by: OverlayVector::new(-862.5, 0.0),
                passed_by: OverlayVector::new(862.5, 0.0),
            }),
            seconds: PLAIN,
        };
        let focused = Accent::Arc {
            depth: 0.7516,
            share: 0.5,
            swap: None,
            seconds: PLAIN,
        };
        for accent in [moved, focused] {
            let mut plain = focus_right(859.5).flown(Cue::Plain);
            assert!(
                plain.score.track(Node::Strip, Channel::Base).is_none(),
                "a plain pan"
            );
            let now = BEGIN + 0.1;
            let incoming = pan(&plain, 0.0);
            plain.merge(&incoming, None, Some(accent), now);
            let arc = plain.score.track(Node::Strip, Channel::Base).expect("a shallow arc").clone();
            assert_eq!(arc.begin(), now, "{accent:?}");
            close(arc.seconds(), ARC, 1e-9, "its length");
            assert_eq!(arc.keys()[0].1, Value::Zoom(Zoom::REST), "from rest");
            close(
                scale_at(&plain.score, now + ARC / 2.0),
                STRIP_ZOOM,
                1e-3,
                "shallow",
            );
            let lift = plain.score.track(Node::Lift(wid(N)), Channel::Base);
            let lifts = matches!(accent, Accent::Arc { swap: Some(_), .. });
            assert_eq!(
                lift.map(|t| (t.begin(), t.ends())),
                lifts.then_some((now, arc.ends()))
            );

            let mut zoomed = focus_right(859.5).flown(Cue::Focus { from: wid(W), to: wid(N) });
            let playing = zoomed.score.track(Node::Strip, Channel::Base).cloned();
            let incoming = pan(&zoomed, 0.0);
            zoomed.merge(&incoming, None, Some(accent), BEGIN + 0.05);
            assert_eq!(
                zoomed.score.track(Node::Strip, Channel::Base).cloned(),
                playing,
                "{accent:?}"
            );
        }
    }

    /// A push's arc leaves from where the strip is drawn, is at `STRIP_ZOOM` where the bounce turns
    /// and at rest at its end, still at all three, and goes one way on each side of the turn.
    #[test]
    fn a_push_arc_is_deepest_where_the_bounce_turns() {
        let turn = BOUNCE_TURN / ZOOM_STRETCH;
        for from in [1.0, 0.95, STRIP_ZOOM, 0.8] {
            close(
                push_scale(from, turn, 0.0),
                from,
                1e-12,
                "from where it is drawn",
            );
            close(push_scale(from, turn, turn), STRIP_ZOOM, 1e-12, "at the turn");
            close(push_scale(from, turn, 1.0), 1.0, 1e-12, "at rest");
            let still =
                |f: f64| (push_scale(from, turn, f + 1e-6) - push_scale(from, turn, f)) / 1e-6;
            for f in [0.0, turn - 1e-6, 1.0 - 1e-6] {
                assert!(still(f).abs() < 1e-3, "{from} still at {f}: {}", still(f));
            }
            let samples: Vec<f64> =
                (0..=100).map(|k| push_scale(from, turn, k as f64 / 100.0)).collect();
            let split = (turn * 100.0) as usize;
            let toward = |a: f64, b: f64| (b - a) * (STRIP_ZOOM - from) >= -1e-12;
            assert!(
                samples[..=split].windows(2).all(|w| toward(w[0], w[1])),
                "{from} out"
            );
            assert!(
                samples[split + 1..].windows(2).all(|w| w[1] >= w[0]),
                "{from} home"
            );
        }
    }
}
