//! How the overview moves on the overlay: every row of its scene one strip container and its edge
//! stacks another, all under the strip's zoom; the zoom out from
//! the screen as it is and back in onto the real layout, and every press inside it, played Slide's
//! way at a press's pace, each node from where it is drawn to where the new scene has it; every
//! thumb lifted off the overlay while it is held, the selected one furthest, and the desktop behind
//! them darkened and blurred; the shown workspace's floating windows where the screen has them,
//! outside the zoom, faded out as the desktop darkens; and the push at an end bounced on screen as
//! far as a flight's.
//!
//! Not a `Look`, nor in `Look::ALL`: the overview is a view the presses inside it change, and its
//! moves are the same under every look (`specs/overview.md`, "How it moves").

use std::collections::HashMap;
use std::time::Duration;

use rini_core::ids::WindowId;
use rini_geometry::space::{
    GroupRect, OverlayPoint, OverlayRect, OverlayVector, Point, Respace, Size,
};

use super::Cue;
use crate::animation::domain::motion::bounce::Bounce;
use crate::animation::domain::motion::easing::MOTION_CURVE;
use crate::animation::domain::motion::glide::{spring_axis, spring_omega};
use crate::animation::domain::motion::plan::{Banding, GroupKey, group_relative};
use crate::animation::domain::motion::score::{
    Channel, Curve, Edit, Node, SAMPLE_STEP, Score, Track, Value, Zoom,
};
use crate::overview::domain::scene::{Holder, Scene};

pub mod card;

/// A press inside the overview, as the flight it held back named it: what the scene's next change
/// plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Press {
    /// A layout pass, a move or a step along the strip, or a workspace switch.
    Moved(Cue),
    /// A push against an end of the strip or of the stack, giving by `overshoot` on screen.
    Pushed {
        press: Cue,
        overshoot: OverlayVector,
    },
}

/// How far every thumb but the selected one is lifted while the overview is held, of the lift a
/// flight's carried window takes (`LIFT_SHADOW`): enough of a shadow to float it, under the
/// selected one's, which is lifted all the way.
pub const THUMB_LIFT: f64 = 0.4;
/// How far the selected thumb is lifted while the overview is held.
pub const SELECTED_LIFT: f64 = 1.0;

/// How long a spring a press bends a node on runs, as a share of `1 / omega`: within 0.01% of its
/// destination by then, so its last sample lands on it.
const SPRING_RUNS: f64 = 12.0;

/// The containers a row's thumbs ride, two to a row: its strip, and its edge stacks behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Strip,
    Stacks,
}

fn key(row: usize, part: Part) -> GroupKey {
    let part = match part {
        Part::Strip => 1,
        Part::Stacks => 2,
    };
    GroupKey::Rigid((2 * row + part) as u16)
}

/// The strip container of `row`.
pub fn strip_of(row: usize) -> GroupKey { key(row, Part::Strip) }

/// A floating window of the shown workspace as the screen has it: where it is on the overlay, and
/// whether the window server has it in front of the strip's windows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Floating {
    pub window: WindowId,
    pub frame: OverlayRect,
    pub in_front: bool,
}

/// Where every node of the overview is drawn once its tracks end: the strip's zoom, each
/// container's place, each tile's container and frame in it and how far a thumb is lifted, and
/// each empty workspace's outline.
#[derive(Clone, Debug, PartialEq)]
pub struct Places {
    pub zoom: Zoom,
    pub containers: Vec<(GroupKey, OverlayPoint)>,
    pub tiles: Vec<(WindowId, GroupKey, GroupRect)>,
    /// Each thumb's lift, from 0 to 1, in `tiles`' order; a floating window casts none.
    pub lifts: Vec<(WindowId, f64)>,
    /// How far the overview is held (`Node::Held`): all the way while held, none at rest.
    pub held: f64,
    pub banding: Banding,
    pub outlines: Vec<Outline>,
}

/// An empty workspace's outline: its tiling area in `key`'s container, its row selected or not.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outline {
    pub key: GroupKey,
    pub frame: GroupRect,
    pub selected: bool,
}

impl Places {
    fn container(&self, key: GroupKey) -> Option<OverlayPoint> {
        self.containers.iter().find(|(k, _)| *k == key).map(|(_, at)| *at)
    }

    /// Where `window`'s thumb is drawn: its frame in its container, through the zoom.
    #[cfg(test)]
    pub fn drawn(&self, window: WindowId) -> Option<OverlayRect> {
        let (_, key, frame) = self.tiles.iter().find(|(w, ..)| *w == window)?;
        let at = self.container(*key)?;
        let laid = crate::animation::domain::motion::plan::overlay_of(*frame, at);
        if *key == GroupKey::Floating {
            return Some(laid);
        }
        Some(OverlayRect::new(
            self.zoom.draw(laid.origin),
            laid.size * self.zoom.scale,
        ))
    }
}

/// `rect` as the strip layer has it under a zoom by `scale` with no shift.
fn unzoomed(rect: OverlayRect, scale: f64) -> OverlayRect {
    OverlayRect::new(
        Point::new(rect.min_x() / scale, rect.min_y() / scale),
        Size::new(rect.width() / scale, rect.height() / scale),
    )
}

fn unzoomed_point(point: OverlayPoint, scale: f64) -> OverlayPoint {
    Point::new(point.x / scale, point.y / scale)
}

/// Every container of `scene`'s rows where it is held: a row's strip at where it starts, its
/// stacks at its top and the display's left, all under the zoom by the scene's depth.
fn held_containers(scene: &Scene) -> Vec<(GroupKey, OverlayPoint)> {
    let s = scene.scale;
    scene
        .rows
        .iter()
        .enumerate()
        .flat_map(|(row, scene_row)| {
            let strip = unzoomed_point(scene_row.strip, s);
            [
                (key(row, Part::Strip), strip),
                (key(row, Part::Stacks), Point::new(0.0, strip.y)),
            ]
        })
        .collect()
}

/// The scene as it is held: every row at its depth, each drawn thumb in its holder's container at
/// where the scene draws it and lifted, the selected window furthest, the selected window and its
/// row in front, over the desktop darkened and blurred.
pub fn held(scene: &Scene) -> Places {
    let s = scene.scale;
    let containers = held_containers(scene);
    let at = |key: GroupKey| {
        containers
            .iter()
            .find(|(k, _)| *k == key)
            .map_or(Point::origin(), |(_, at)| *at)
    };
    let mut tiles = Vec::new();
    let mut outlines = Vec::new();
    for (row, scene_row) in scene.rows.iter().enumerate() {
        for tile in scene_row.tiles.iter().filter(|tile| tile.drawn()) {
            let part = part_of(tile.holder);
            let key = key(row, part);
            tiles.push((
                tile.window,
                key,
                group_relative(unzoomed(tile.frame, s), at(key)),
            ));
        }
        if let Some(outline) = scene_row.outline {
            let key = key(row, Part::Stacks);
            outlines.push(Outline {
                key,
                frame: group_relative(unzoomed(outline, s), at(key)),
                selected: scene.selected.row == row,
            });
        }
    }
    let lifts = tiles
        .iter()
        .map(|(window, ..)| {
            let lift = if scene.selected.window == Some(*window) {
                SELECTED_LIFT
            } else {
                THUMB_LIFT
            };
            (*window, lift)
        })
        .collect();
    Places {
        zoom: Zoom {
            scale: s,
            shift: OverlayVector::zero(),
        },
        containers,
        tiles,
        lifts,
        held: 1.0,
        banding: banding(scene),
        outlines,
    }
}

/// The scene as the display shows it at rest: the zoom at 1, its active row's windows each at its
/// frame in `shown`, its strip about where its thumbs' centres put it, every other row where the
/// stack has it, off the display above and below, no thumb lifted and nothing darkened or blurred.
/// What the overview zooms out from as it opens, and back into as it closes.
pub fn at_rest(scene: &Scene, shown: &[(WindowId, OverlayRect)]) -> Places {
    let mut places = held(scene);
    places.held = 0.0;
    for (_, lift) in &mut places.lifts {
        *lift = 0.0;
    }
    let s = scene.scale;
    let active = scene.selected.row;
    let Some(row) = scene.rows.get(active) else {
        return places;
    };
    let display = unzoomed_point(row.display_origin, s);
    places.zoom = Zoom {
        scale: 1.0,
        shift: -display.to_vector(),
    };
    let to_rest = |window: WindowId| {
        let (_, frame) = shown.iter().find(|(w, _)| *w == window)?;
        Some(OverlayRect::new(frame.origin + display.to_vector(), frame.size))
    };
    let strip = key(active, Part::Strip);
    let anchor = row.tiles.iter().find_map(|tile| {
        let laid = places.tiles.iter().find(|(w, k, _)| *w == tile.window && *k == strip)?;
        Some(to_rest(tile.window)?.center() - laid.2.center().to_vector().respace())
    });
    if let Some(anchor) = anchor
        && let Some((_, at)) = places.containers.iter_mut().find(|(k, _)| *k == strip)
    {
        *at = anchor;
    }
    let stacks = key(active, Part::Stacks);
    let at = |key: GroupKey| places.container(key).unwrap_or(Point::origin());
    let (strip_at, stacks_at) = (at(strip), at(stacks));
    for (window, key, frame) in &mut places.tiles {
        let container = match *key {
            k if k == strip => strip_at,
            k if k == stacks => stacks_at,
            _ => continue,
        };
        if let Some(rest) = to_rest(*window) {
            *frame = group_relative(rest, container);
        }
    }
    places
}

/// `places` with the shown workspace's floating windows, front to back as `floating` lists them,
/// each riding the floating container where the screen has it, outside the zoom: in front of the
/// strip where the window server has it there, the strip moved back a band for it as a flight
/// moves it for a floating focus, and behind it otherwise. The renderer draws that container at the
/// opposite of the tint, so each is on screen at rest and gone once the desktop is darkened.
pub fn with_floating(mut places: Places, floating: &[Floating]) -> Places {
    if floating.is_empty() {
        return places;
    }
    places.containers.push((GroupKey::Floating, Point::origin()));
    for (depth, window) in floating.iter().enumerate() {
        let frame = group_relative(window.frame, Point::origin());
        places.tiles.push((window.window, GroupKey::Floating, frame));
        places.banding.within.insert(window.window, depth);
    }
    let in_front = floating.iter().filter(|window| window.in_front);
    places.banding.lifted = in_front.map(|window| window.window).collect();
    places.banding.focus_off_strip = !places.banding.lifted.is_empty();
    places
}

fn part_of(holder: Holder) -> Part {
    match holder {
        Holder::Row => Part::Strip,
        Holder::Stack { .. } => Part::Stacks,
    }
}

/// Front to back: the selected row's containers first, its strip over its stacks; in a strip the
/// selected window first, in a stack its top card first.
fn banding(scene: &Scene) -> Banding {
    let mut rows: Vec<usize> = (0..scene.rows.len()).collect();
    rows.sort_by_key(|row| *row != scene.selected.row);
    let group_order = rows
        .iter()
        .flat_map(|row| [Part::Strip, Part::Stacks].map(|part| key(*row, part)))
        .collect();
    let mut within = HashMap::new();
    for scene_row in &scene.rows {
        let mut strip_depth = 1;
        for tile in &scene_row.tiles {
            let depth = match tile.holder {
                Holder::Row if scene.selected.window == Some(tile.window) => 0,
                Holder::Row => {
                    strip_depth += 1;
                    strip_depth - 1
                }
                Holder::Stack { depth, .. } => depth,
            };
            within.insert(tile.window, depth);
        }
    }
    Banding {
        focus_off_strip: false,
        lifted: Vec::new(),
        within,
        group_order,
    }
}

/// The score that draws `places` with nothing moving: frame zero, and every cut.
pub fn pose(places: &Places) -> Score {
    let mut score = Score::default();
    for edit in placed(places) {
        score.apply(&edit);
    }
    score
}

/// Every node of `places` placed where it is drawn, each tile riding its container.
fn placed(places: &Places) -> Vec<Edit> {
    let mut edits = vec![
        Edit::Place(Node::Strip, Value::Zoom(places.zoom)),
        Edit::Place(Node::Held, Value::Number(places.held)),
    ];
    for (key, at) in &places.containers {
        edits.push(Edit::Place(Node::Container(*key), Value::Point(*at)));
    }
    for (window, key, frame) in &places.tiles {
        edits.push(Edit::Ride(*window, *key));
        edits.push(Edit::Place(Node::Tile(*window), Value::Rect(*frame)));
    }
    for (window, lift) in &places.lifts {
        edits.push(Edit::Place(Node::Lift(*window), Value::Number(*lift)));
    }
    edits.push(Edit::Bank(places.banding.clone()));
    edits
}

/// How a change of the overview moves, every node alike.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    /// Straight to where it ends: animation off, or Low Power Mode.
    Cut,
    /// Slide's curve over `seconds`, as a press pans the strip: the zoom out as it opens, the zoom
    /// in as it closes, and every press inside it.
    Slide { seconds: f64 },
}

impl Motion {
    fn seconds(self) -> f64 {
        match self {
            Motion::Cut => 0.0,
            Motion::Slide { seconds } => seconds.max(0.0),
        }
    }
}

/// The edits that take `playing`, at media time `at`, to `places`, each node that changes from where
/// its movement has it: on `motion`'s own shape when nothing plays, and on one spring each, at its
/// own speed, when anything does, so a press bending the motion keeps every node's pace and a thumb
/// still on screen stays still. A thumb new to the overview is cut in; one gone from it, or not
/// drawn, is dropped; one changing container is carried from where it is drawn into its new one.
pub fn toward(playing: &Score, places: &Places, at: f64, motion: Motion) -> Vec<Edit> {
    let seconds = motion.seconds();
    let shape = if seconds <= 0.0 {
        Shape::Cut
    } else if playing_anything(playing, at) {
        Shape::Spring { omega: spring_omega(seconds) }
    } else {
        Shape::Slide { seconds }
    };
    let mut edits = Vec::new();
    let gone: Vec<WindowId> = playing
        .tiles()
        .map(|(window, _)| window)
        .filter(|window| !places.tiles.iter().any(|(w, ..)| w == window))
        .collect();
    edits.extend(gone.into_iter().map(|window| Edit::Drop(Node::Tile(window))));
    let zoom = Value::Zoom(places.zoom);
    edits.extend(leg(playing, Node::Strip, None, zoom, at, shape));
    let share = Value::Number(places.held);
    edits.extend(leg(playing, Node::Held, None, share, at, shape));
    for (key, to) in &places.containers {
        let node = Node::Container(*key);
        if playing.model(node).is_none() {
            edits.push(Edit::Place(node, Value::Point(*to)));
            continue;
        }
        edits.extend(leg(playing, node, None, Value::Point(*to), at, shape));
    }
    for (window, key, frame) in &places.tiles {
        let node = Node::Tile(*window);
        match playing.parent(*window) {
            None => {
                edits.push(Edit::Ride(*window, *key));
                edits.push(Edit::Place(node, Value::Rect(*frame)));
            }
            Some(parent) if parent == *key => {
                edits.extend(leg(playing, node, None, Value::Rect(*frame), at, shape));
            }
            Some(parent) => {
                let carried = carried_into(playing, *window, (parent, *key), places, at);
                edits.push(Edit::Ride(*window, *key));
                edits.extend(leg(playing, node, carried, Value::Rect(*frame), at, shape));
            }
        }
    }
    for (window, lift) in &places.lifts {
        let to = Value::Number(*lift);
        edits.extend(leg(playing, Node::Lift(*window), None, to, at, shape));
    }
    let stale: Vec<GroupKey> =
        playing.containers().filter(|key| places.container(*key).is_none()).collect();
    edits.extend(stale.into_iter().map(|key| Edit::Drop(Node::Container(key))));
    edits.push(Edit::Bank(places.banding.clone()));
    edits
}

/// The shape every node's leg of one change takes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    Cut,
    Slide { seconds: f64 },
    Spring { omega: f64 },
}

/// Whether any movement plays at `at`: a press then bends it rather than starting afresh.
fn playing_anything(playing: &Score, at: f64) -> bool {
    std::iter::once(Node::Strip)
        .chain(playing.containers().map(Node::Container))
        .chain(playing.tiles().map(|(window, _)| Node::Tile(window)))
        .any(|node| playing.track(node, Channel::Base).is_some_and(|track| track.at(at).is_some()))
}

/// Where a thumb leaving container `from` for `to` is drawn in `to` at `at`, and how fast it goes
/// there: what its leg in its new container starts from.
fn carried_into(
    playing: &Score,
    window: WindowId,
    (from, to): (GroupKey, GroupKey),
    places: &Places,
    at: f64,
) -> Option<(Value, [f64; 4])> {
    let point = |value: Option<Value>| match value {
        Some(Value::Point(point)) => Some(point),
        _ => None,
    };
    let Some(Value::Rect(frame)) = playing.sample(Node::Tile(window), at) else {
        return None;
    };
    let was = point(playing.sample(Node::Container(from), at))?;
    let into = point(playing.sample(Node::Container(to), at))
        .or_else(|| places.container(to))
        .unwrap_or(was);
    let shift = was - into;
    let moved = GroupRect::new(frame.origin + shift.respace(), frame.size);
    let rate = |node| playing.rate(node, at);
    let (tile, was_rate, into_rate) = (
        rate(Node::Tile(window)),
        rate(Node::Container(from)),
        rate(Node::Container(to)),
    );
    let velocity = [
        tile[0] + was_rate[0] - into_rate[0],
        tile[1] + was_rate[1] - into_rate[1],
        tile[2],
        tile[3],
    ];
    Some((Value::Rect(moved), velocity))
}

/// `node` placed at `to`, carried there from `from`, else from where its movement has it at `at`,
/// on `shape`; nothing when it is there already and nothing plays on it.
fn leg(
    playing: &Score,
    node: Node,
    from: Option<(Value, [f64; 4])>,
    to: Value,
    at: f64,
    shape: Shape,
) -> Vec<Edit> {
    let (start, velocity) = match from {
        Some(carried) => carried,
        None => {
            let Some(along) = playing.along(node, at) else {
                return vec![Edit::Place(node, to)];
            };
            (along, playing.rate(node, at))
        }
    };
    let moving = playing.track(node, Channel::Base).is_some_and(|track| track.at(at).is_some());
    if from.is_none() && !moving && same(start, to) {
        return if playing.model(node).is_some_and(|model| same(model, to)) {
            Vec::new()
        } else {
            vec![Edit::Place(node, to)]
        };
    }
    let track = match shape {
        Shape::Cut => return vec![Edit::Place(node, to), Edit::Cut(node, Channel::Base)],
        Shape::Slide { seconds } => Track::between(
            node,
            Channel::Base,
            start,
            to,
            at,
            seconds,
            Curve::Bezier(MOTION_CURVE),
        ),
        Shape::Spring { omega } => spring(node, (start, velocity), to, omega, at),
    };
    vec![Edit::Place(node, to), Edit::Add(track)]
}

/// Two values a node is drawn the same at: within a millionth of a point, or of a scale, on every
/// part, as two scenes laid out from one layout lay a thumb.
fn same(a: Value, b: Value) -> bool {
    let (a, b) = (a.parts(), b.parts());
    (0..4).all(|i| (a[i] - b[i]).abs() < 1e-6)
}

/// A critically damped spring on every part of `node` from `start` going at `velocity` to `to`,
/// at `omega`, sampled a frame apart and ending on `to`.
fn spring(
    node: Node,
    (start, velocity): (Value, [f64; 4]),
    to: Value,
    omega: f64,
    at: f64,
) -> Track {
    let runs = SPRING_RUNS / omega;
    let frames = (runs / SAMPLE_STEP).ceil().max(1.0) as usize;
    let (a, b) = (start.parts(), to.parts());
    let values = (0..frames)
        .map(|k| {
            let t = k as f64 * SAMPLE_STEP;
            start.with_parts(std::array::from_fn(|i| {
                spring_axis(a[i], b[i], velocity[i], omega, t).0
            }))
        })
        .chain(std::iter::once(to));
    Track::samples(node, Channel::Base, values, at, frames as f64 * SAMPLE_STEP)
}

/// The bounces `presses` play from `at`: a push against an end of the stack bounces every row, one
/// against an end of a strip that row's strip, `overshoot` on screen at the scene's depth.
pub fn pushes(
    playing: &Score,
    scene: &Scene,
    presses: &[Press],
    seconds: f64,
    at: f64,
) -> Vec<Edit> {
    let duration = Duration::from_secs_f64(seconds.max(0.0));
    if duration.is_zero() {
        return Vec::new();
    }
    let s = scene.scale;
    let mut edits = Vec::new();
    for press in presses {
        let Press::Pushed { press, overshoot } = press else {
            continue;
        };
        let wall = OverlayVector::new(overshoot.x / s, overshoot.y / s);
        let keys: Vec<GroupKey> = match press {
            Cue::Switch => (0..scene.rows.len())
                .flat_map(|row| [Part::Strip, Part::Stacks].map(|part| key(row, part)))
                .collect(),
            Cue::Plain | Cue::Move { .. } | Cue::Focus { .. } => {
                vec![key(scene.selected.row, Part::Strip)]
            }
        };
        for key in keys {
            let drawn = match playing
                .track(Node::Container(key), Channel::Bounce)
                .and_then(|bounce| bounce.at(at))
            {
                Some(Value::Point(drawn)) => drawn.to_vector(),
                _ => OverlayVector::zero(),
            };
            edits.push(Edit::Add(
                Bounce::from_drawn(drawn, wall, duration).track(key, at),
            ));
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::ScreenRect;
    use rini_geometry::{IsWithin, to_overlay_space};

    use super::*;
    use crate::animation::domain::motion::plan::EDGE_BOUNCE_OVERSHOOT;
    use crate::animation::domain::motion::score::Form;
    use crate::overview::domain::fixture::{Col, display, overview, row, strip, w};
    use crate::overview::domain::scene::{Laid, OverviewInput, lay_out};
    use crate::overview::domain::stacks::Side;

    const PRESS: f64 = 0.35;
    const WIDE: f64 = 1400.0;
    const NARROW: f64 = 800.0;

    static IDS: [u32; 11] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

    /// Ten columns on the first workspace, the fifth a stack of two, two columns on the second, none
    /// on the third and one on the fourth: the first shows, on `current`.
    fn input(order: &[u32], current: u32) -> OverviewInput {
        let columns: Vec<Col> = order
            .iter()
            .map(|&id| match id {
                5 => Col { width: WIDE, windows: &[5, 50] },
                id => Col {
                    width: if id % 2 == 0 { WIDE } else { NARROW },
                    windows: std::slice::from_ref(&IDS[id as usize]),
                },
            })
            .collect();
        let second = [Col { width: WIDE, windows: &[11] }, Col {
            width: NARROW,
            windows: &[12],
        }];
        let first = row(strip(&columns, 0.0), Some(current));
        let fourth = [Col { width: WIDE, windows: &[13] }];
        overview(
            vec![
                first,
                row(strip(&second, 0.0), Some(11)),
                row(Vec::new(), None),
                row(strip(&fourth, 0.0), Some(13)),
            ],
            0,
        )
    }

    const STRIP: [u32; 10] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

    fn scene(order: &[u32], current: u32) -> Scene { lay_out(&input(order, current)) }

    /// Where the display has the shown workspace's windows: the strip scrolled to show `current`,
    /// as the layout scrolls it.
    fn shown(order: &[u32], current: u32) -> Vec<(WindowId, OverlayRect)> {
        let input = input(order, current);
        let laid: Vec<&Laid> =
            input.rows[0].columns.iter().flat_map(|column| &column.windows).collect();
        let current = laid.iter().find(|laid| laid.window == w(current)).unwrap().frame;
        let scroll = current.min_x() - 16.0;
        let on = |frame: ScreenRect| to_overlay_space(frame, display());
        laid.iter()
            .map(|laid| {
                let frame = on(laid.frame);
                (laid.window, frame.translate(OverlayVector::new(-scroll, 0.0)))
            })
            .collect()
    }

    fn played(score: &Score, edits: &[Edit]) -> Score {
        let mut score = score.clone();
        for edit in edits {
            score.apply(edit);
        }
        score
    }

    fn drawn(score: &Score, window: u32, at: f64) -> OverlayRect {
        score.drawn(w(window), at).unwrap_or_else(|| panic!("w{window} drawn at {at}"))
    }

    fn tracks(edits: &[Edit]) -> Vec<&Track> {
        edits
            .iter()
            .filter_map(|edit| match edit {
                Edit::Add(track) => Some(track),
                _ => None,
            })
            .collect()
    }

    /// Frame zero is the screen: every window of the shown workspace drawn where the display has
    /// it, one its row stacks at an edge and the selected one alike.
    #[test]
    fn the_overview_s_frame_zero_is_the_screen() {
        let scene = scene(&STRIP, 3);
        let stacked = scene.rows[0]
            .tiles
            .iter()
            .filter(|tile| matches!(tile.holder, Holder::Stack { .. }) && tile.drawn())
            .count();
        assert!(stacked > 0, "a column stacks at an edge");
        let shown = shown(&STRIP, 3);
        let score = pose(&at_rest(&scene, &shown));
        for (window, frame) in &shown {
            if scene.find(*window).is_some_and(|(_, tile)| tile.drawn()) {
                let at = score.drawn(*window, 0.0).unwrap();
                assert!(
                    at.is_within(1e-6, *frame),
                    "{window:?}: {at:?} against {frame:?}"
                );
            }
        }
    }

    /// The shown workspace's floating windows, w20 over the strip and w21 behind it, the tint
    /// played from the screen to the scene held and back: each window drawn where the screen has
    /// it the whole way, outside the zoom, on the floating container the renderer fades as the tint
    /// darkens, so on screen at rest and gone held; w20 in front of the strip, which moves back a
    /// band for it, and w21 behind.
    #[test]
    fn the_floating_windows_stay_where_the_screen_has_them_as_the_tint_comes_and_goes() {
        let scene = scene(&STRIP, 3);
        let shown = shown(&STRIP, 3);
        let floating = [
            Floating {
                window: w(20),
                frame: OverlayRect::new(Point::new(300.0, 200.0), Size::new(600.0, 400.0)),
                in_front: true,
            },
            Floating {
                window: w(21),
                frame: OverlayRect::new(Point::new(900.0, 500.0), Size::new(400.0, 300.0)),
                in_front: false,
            },
        ];
        let rest = with_floating(at_rest(&scene, &shown), &floating);
        let hold = with_floating(held(&scene), &floating);
        let mut score = pose(&rest);
        for (places, tint) in [(&rest, 0.0), (&hold, 1.0)] {
            assert_eq!(places.held, tint);
            for window in floating {
                assert!(places.drawn(window.window).unwrap().is_within(1e-6, window.frame));
                assert_eq!(score.parent(window.window), Some(GroupKey::Floating));
            }
            assert_eq!(places.banding.lifted, vec![w(20)]);
            assert!(places.banding.focus_off_strip);
            assert_eq!(places.banding.within[&w(20)], 0);
            assert_eq!(places.banding.within[&w(21)], 1);
            let lifted = |window: WindowId| places.lifts.iter().any(|(w, _)| *w == window);
            assert!(!lifted(w(20)) && !lifted(w(21)), "no shadow");
        }
        let edits = toward(&score, &hold, 0.0, Motion::Slide { seconds: PRESS });
        for edit in &edits {
            score.apply(edit);
        }
        for at in [0.0, PRESS / 2.0, PRESS + 0.01] {
            for window in floating {
                let drawn = score.drawn(window.window, at).unwrap();
                assert!(drawn.is_within(1e-6, window.frame), "{at}: {drawn:?}");
            }
        }
        assert_eq!(score.sample(Node::Held, PRESS + 0.01), Some(Value::Number(1.0)));
        let none = with_floating(held(&scene), &[]);
        assert_eq!(none, held(&scene), "nothing floating adds nothing");
    }

    /// The zoom out runs as a focus press pans the strip, on Slide's curve over a press's length,
    /// from the screen as it is to every thumb where the scene draws it.
    #[test]
    fn the_entry_runs_at_a_press_s_pace_to_every_tile_s_scene_place() {
        let scene = scene(&STRIP, 3);
        let shown = shown(&STRIP, 3);
        let rest = pose(&at_rest(&scene, &shown));
        let edits = toward(&rest, &held(&scene), 0.0, Motion::Slide { seconds: PRESS });
        let zoom = tracks(&edits)
            .into_iter()
            .find(|track| track.node() == Node::Strip)
            .expect("the strip zooms");
        assert_eq!(zoom.form(), Form::Between);
        assert_eq!(zoom.curves(), [Curve::Bezier(MOTION_CURVE)]);
        assert!((zoom.seconds() - PRESS).abs() < 1e-9);
        let score = played(&rest, &edits);
        for (window, frame) in &shown {
            let Some(at) = score.drawn(*window, 0.0) else { continue };
            assert!(at.is_within(1e-6, *frame), "no jump as it starts: {window:?}");
        }
        for scene_row in &scene.rows {
            for tile in scene_row.tiles.iter().filter(|tile| tile.drawn()) {
                let at = score.drawn(tile.window, PRESS + 0.01).unwrap();
                assert!(at.is_within(1e-6, tile.frame), "{tile:?}: {at:?}");
            }
        }
    }

    /// Held, every drawn thumb is lifted, the selected one all the way; at rest none is, so frame
    /// zero is the screen without a shadow, and the zoom out lifts them as it goes.
    #[test]
    fn held_thumbs_are_lifted_the_selected_one_highest_and_none_at_rest() {
        let scene = scene(&STRIP, 3);
        let places = held(&scene);
        assert_eq!(places.lifts.len(), places.tiles.len());
        for (window, lift) in &places.lifts {
            let expected = if *window == w(3) {
                SELECTED_LIFT
            } else {
                THUMB_LIFT
            };
            assert_eq!(*lift, expected, "{window:?}");
        }
        const { assert!(SELECTED_LIFT > THUMB_LIFT && THUMB_LIFT > 0.0) };
        let rest = at_rest(&scene, &shown(&STRIP, 3));
        assert!(rest.lifts.iter().all(|(_, lift)| *lift == 0.0));
        let start = pose(&rest);
        let edits = toward(&start, &places, 0.0, Motion::Slide { seconds: PRESS });
        let score = played(&start, &edits);
        let lift = |t: f64| match score.sample(Node::Lift(w(3)), t) {
            Some(Value::Number(lift)) => lift,
            other => panic!("{other:?}"),
        };
        assert_eq!(lift(0.0), 0.0, "frame zero casts no lift");
        assert!(lift(PRESS / 4.0) > 0.0 && lift(PRESS / 4.0) < SELECTED_LIFT);
        assert!((lift(PRESS + 0.01) - SELECTED_LIFT).abs() < 1e-9);
    }

    /// The desktop behind is darkened all the way while held and not at all at rest, and the zoom
    /// out darkens it on the zoom's own curve and length, from none at frame zero.
    #[test]
    fn the_desktop_darkens_with_the_zoom_out_from_none_at_frame_zero() {
        let scene = scene(&STRIP, 3);
        let (places, rest) = (held(&scene), at_rest(&scene, &shown(&STRIP, 3)));
        assert_eq!((places.held, rest.held), (1.0, 0.0));
        let start = pose(&rest);
        assert_eq!(start.model(Node::Held), Some(Value::Number(0.0)));
        let edits = toward(&start, &places, 0.0, Motion::Slide { seconds: PRESS });
        let darkens = tracks(&edits)
            .into_iter()
            .find(|track| track.node() == Node::Held)
            .expect("the desktop darkens");
        assert_eq!(darkens.curves(), [Curve::Bezier(MOTION_CURVE)]);
        assert!((darkens.seconds() - PRESS).abs() < 1e-9);
        let score = played(&start, &edits);
        assert_eq!(score.sample(Node::Held, 0.0), Some(Value::Number(0.0)));
        assert_eq!(score.sample(Node::Held, PRESS + 0.01), Some(Value::Number(1.0)));
    }

    /// A step right pans the row on Slide's curve over the plain length, as one piece: its strip's
    /// container travels, and a thumb staying in the strip moves with it alone.
    #[test]
    fn a_row_pan_runs_on_slides_curve() {
        let (before, after) = (scene(&STRIP, 3), scene(&STRIP, 4));
        let score = pose(&held(&before));
        let edits = toward(&score, &held(&after), 0.0, Motion::Slide { seconds: PRESS });
        let pan = tracks(&edits)
            .into_iter()
            .find(|track| track.node() == Node::Container(strip_of(0)))
            .expect("the row's strip pans");
        assert_eq!(pan.form(), Form::Between);
        assert_eq!(pan.curves(), [Curve::Bezier(MOTION_CURVE)]);
        assert!((pan.seconds() - PRESS).abs() < 1e-9);
        let score = played(&score, &edits);
        assert!(
            !tracks(&edits).iter().any(|track| track.node() == Node::Tile(w(3))),
            "w3 stays in the strip, carried by it"
        );
        let moved = drawn(&score, 4, PRESS + 0.01);
        assert!((moved.center().x - after.axis).abs() < 1e-6, "{moved:?}");
        assert!(drawn(&score, 4, 0.0).is_within(1e-6, before.find(w(4)).unwrap().1.frame));
    }

    /// A card that stays in its stack through a pan stays where it is drawn at the edge while the
    /// edge holds it, however the strip moves beside it, and is carried from where its stack had it
    /// to where it has it where the thumb beside it held it out past the edge; one leaving the
    /// stack is carried out of it from where it was drawn.
    #[test]
    fn a_stacked_card_stays_at_the_edge_while_its_row_pans() {
        const ODD_FIRST: [u32; 6] = [1, 3, 7, 9, 2, 4];
        let panned = |order: &[u32], (from, to): (u32, u32), card: u32| {
            let (before, after) = (scene(order, from), scene(order, to));
            let score = pose(&held(&before));
            let edits = toward(&score, &held(&after), 0.0, Motion::Slide { seconds: PRESS });
            let still = !tracks(&edits).iter().any(|track| track.node() == Node::Tile(w(card)));
            (played(&score, &edits), before, (after, still))
        };
        let (score, before, (after, still)) = panned(&ODD_FIRST, (3, 7), 4);
        assert!(still, "its stack holds it still: nothing moves it");
        let stays = before.find(w(4)).unwrap().1.holder;
        assert!(matches!(stays, Holder::Stack { side: Side::Right, depth: 0 }));
        assert_eq!(after.find(w(4)).unwrap().1.holder, stays);
        let at = |t: f64| drawn(&score, 4, t);
        for t in [0.0, 0.05, PRESS / 2.0, PRESS - 0.01, PRESS + 0.01] {
            assert!(at(t).is_within(1e-6, at(0.0)), "{t}: {:?}", at(t));
        }

        let (score, before, (after, _)) = panned(&STRIP, (6, 7), 10);
        let stays = before.find(w(10)).unwrap().1.holder;
        assert!(matches!(stays, Holder::Stack { side: Side::Right, depth: 0 }));
        assert_eq!(after.find(w(10)).unwrap().1.holder, stays);
        let held_out = before.find(w(10)).unwrap().1.frame;
        let beside = before.find(w(9)).unwrap().1.frame;
        assert!(
            (held_out.min_x() - beside.min_x()).abs() < 1e-6,
            "held out to the thumb beside it: {held_out:?}"
        );
        assert!(drawn(&score, 10, 0.0).is_within(1e-6, held_out));
        let at_edge = after.find(w(10)).unwrap().1.frame;
        assert!(drawn(&score, 10, PRESS + 0.01).is_within(1e-6, at_edge));

        let (score, before, (after, _)) = panned(&STRIP, (4, 5), 7);
        assert!(matches!(before.find(w(7)).unwrap().1.holder, Holder::Stack {
            side: Side::Right,
            ..
        }));
        assert_eq!(after.find(w(7)).unwrap().1.holder, Holder::Row);
        assert!(drawn(&score, 7, 0.0).is_within(1e-6, before.find(w(7)).unwrap().1.frame));
        assert!(drawn(&score, 7, PRESS + 0.01).is_within(1e-6, after.find(w(7)).unwrap().1.frame));
    }

    /// Every row is drawn over its edge stacks, so the thumb beside a stack, the selected one among
    /// them, covers the stack's top card where it reaches over it rather than the reverse.
    #[test]
    fn every_row_is_drawn_over_its_stacks() {
        let scene = scene(&STRIP, 3);
        let order = held(&scene).banding.group_order;
        let at = |key: GroupKey| order.iter().position(|k| *k == key).unwrap();
        for row in 0..scene.rows.len() {
            assert!(
                at(strip_of(row)) < at(key(row, Part::Stacks)),
                "row {row}: {order:?}"
            );
        }
    }

    /// A move keeps the moved window on the axis all the way, the column it passes crossing
    /// beneath it, drawn behind it.
    #[test]
    fn a_move_keeps_the_moved_window_on_the_axis_and_its_neighbour_crosses_beneath() {
        let before = scene(&STRIP, 3);
        let mut order = STRIP;
        order.swap(1, 2);
        let after = scene(&order, 3);
        let score = pose(&held(&before));
        let edits = toward(&score, &held(&after), 0.0, Motion::Slide { seconds: PRESS });
        let score = played(&score, &edits);
        for t in [0.0, 0.03, 0.1, PRESS / 2.0, PRESS + 0.01] {
            let moved = drawn(&score, 3, t);
            assert!((moved.center().x - before.axis).abs() < 1e-6, "{t}: {moved:?}");
        }
        let passed = (drawn(&score, 2, 0.0), drawn(&score, 2, PRESS + 0.01));
        assert!(
            passed.1.min_x() > passed.0.max_x() - 1e-6,
            "w2 crosses: {passed:?}"
        );
        let within = &score.banding().within;
        assert!(within[&w(3)] < within[&w(2)], "the moved window in front");
    }

    /// Closing zooms back in at a press's pace onto the real layout: the workspace shown then, each
    /// of its windows landing on its real frame, the selected one as it was scrolled to.
    #[test]
    fn the_exit_lands_every_tile_on_its_real_frame() {
        let scene = scene(&STRIP, 8);
        let landing = shown(&STRIP, 8);
        let score = pose(&held(&scene));
        let edits = toward(&score, &at_rest(&scene, &landing), 0.0, Motion::Slide {
            seconds: PRESS,
        });
        let zoom = tracks(&edits).into_iter().find(|track| track.node() == Node::Strip).unwrap();
        assert_eq!(zoom.curves(), [Curve::Bezier(MOTION_CURVE)]);
        assert!((zoom.seconds() - PRESS).abs() < 1e-9);
        let score = played(&score, &edits);
        for scene_row in &scene.rows {
            for tile in scene_row.tiles.iter().filter(|tile| tile.drawn()) {
                let at = score.drawn(tile.window, 0.0).unwrap();
                assert!(at.is_within(1e-6, tile.frame), "no jump as it starts: {tile:?}");
            }
        }
        for (window, frame) in &landing {
            let Some(at) = score.drawn(*window, PRESS + 0.01) else {
                continue;
            };
            assert!(
                at.is_within(1e-6, *frame),
                "{window:?}: {at:?} against {frame:?}"
            );
        }
        let Some(Value::Zoom(zoom)) = score.sample(Node::Strip, PRESS + 0.01) else {
            panic!("the strip is zoomed")
        };
        assert!((zoom.scale - 1.0).abs() < 1e-9);
    }

    /// A close while the overview still zooms out turns every node back from where it is drawn and
    /// as fast as it goes, a spring each, and lands at rest.
    #[test]
    fn a_release_during_the_entry_turns_back_from_where_it_is_drawn() {
        let scene = scene(&STRIP, 3);
        let shown = shown(&STRIP, 3);
        let rest = at_rest(&scene, &shown);
        let start = pose(&rest);
        let entering = played(
            &start,
            &toward(&start, &held(&scene), 0.0, Motion::Slide { seconds: PRESS }),
        );
        let turn = PRESS * 0.2;
        let edits = toward(&entering, &rest, turn, Motion::Slide { seconds: PRESS });
        assert!(
            tracks(&edits)
                .iter()
                .all(|track| track.form() == Form::Samples && track.begin() == turn),
            "a spring each"
        );
        let turned = played(&entering, &edits);
        for (window, frame) in &shown {
            let Some(before) = entering.drawn(*window, turn) else {
                continue;
            };
            let after = turned.drawn(*window, turn).unwrap();
            assert!(
                after.is_within(1e-6, before),
                "{window:?} jumps from {before:?} to {after:?}"
            );
            let step = 1e-3;
            let pace =
                |score: &Score| score.drawn(*window, turn + step).unwrap().min_x() - after.min_x();
            let (turning, going) = (pace(&turned), pace(&entering));
            assert!(
                going.abs() < 1e-6 || turning / going > 0.2,
                "{window:?} turns back from the pace it went at: {turning} against {going}"
            );
            let landed = turned.drawn(*window, turn + 1.0).unwrap();
            assert!(landed.is_within(1e-6, *frame), "{window:?}: {landed:?}");
        }
    }

    /// With animation off every change is a cut: nothing plays, and every thumb is where it ends at
    /// once.
    #[test]
    fn animation_off_cuts() {
        let (before, after) = (scene(&STRIP, 3), scene(&STRIP, 4));
        let shown = shown(&STRIP, 3);
        let rest = pose(&at_rest(&before, &shown));
        for (from, to) in [
            (rest.clone(), held(&before)),
            (pose(&held(&before)), held(&after)),
            (pose(&held(&after)), at_rest(&after, &shown)),
        ] {
            let edits = toward(&from, &to, 0.0, Motion::Cut);
            assert!(tracks(&edits).is_empty(), "{:?}", tracks(&edits));
            let score = played(&from, &edits);
            for (window, ..) in &to.tiles {
                let at = score.drawn(*window, 0.0).unwrap();
                assert!(at.is_within(1e-6, to.drawn(*window).unwrap()), "{window:?}");
            }
        }
        assert!(
            pushes(
                &rest,
                &before,
                &[Press::Pushed {
                    press: Cue::Switch,
                    overshoot: OverlayVector::new(0.0, -EDGE_BOUNCE_OVERSHOOT),
                }],
                0.0,
                0.0
            )
            .is_empty()
        );
    }

    /// A push against the strip's end bounces the selected row alone, as far on screen as a
    /// flight's push goes; one against an end of the stack bounces every row.
    #[test]
    fn a_push_bounces_its_row_or_the_stack_as_far_as_a_flights_on_screen() {
        let scene = scene(&STRIP, 1);
        let score = pose(&held(&scene));
        let wall = OverlayVector::new(EDGE_BOUNCE_OVERSHOOT, 0.0);
        let along = Press::Pushed {
            press: Cue::Focus { from: w(1), to: w(1) },
            overshoot: wall,
        };
        let edits = pushes(&score, &scene, &[along], PRESS, 0.0);
        let keys: Vec<Node> = tracks(&edits).iter().map(|track| track.node()).collect();
        assert_eq!(keys, vec![Node::Container(strip_of(0))]);
        let bounced = played(&score, &edits);
        let turn = PRESS * crate::animation::domain::motion::easing::BOUNCE_TURN;
        let out = drawn(&bounced, 1, turn).min_x() - drawn(&score, 1, turn).min_x();
        assert!((out - EDGE_BOUNCE_OVERSHOOT).abs() < 1e-6, "{out}");
        assert!(drawn(&bounced, 11, turn).is_within(1e-9, drawn(&score, 11, turn)));

        let stack = Press::Pushed {
            press: Cue::Switch,
            overshoot: OverlayVector::new(0.0, EDGE_BOUNCE_OVERSHOOT),
        };
        let edits = pushes(&score, &scene, &[stack], PRESS, 0.0);
        let bounced = played(&score, &edits);
        for window in [1, 11, 13] {
            let out = drawn(&bounced, window, turn).min_y() - drawn(&score, window, turn).min_y();
            assert!((out - EDGE_BOUNCE_OVERSHOOT).abs() < 1e-6, "w{window}: {out}");
        }
    }

    /// A press while a pan plays bends it: every node from where it is drawn, its strip on a spring
    /// carrying the speed it was drawn going, the row landing where the second press sends it.
    #[test]
    fn a_press_while_a_pan_plays_bends_it_from_where_it_is_drawn() {
        let scenes = [scene(&STRIP, 3), scene(&STRIP, 4), scene(&STRIP, 5)];
        let score = pose(&held(&scenes[0]));
        let first = played(
            &score,
            &toward(&score, &held(&scenes[1]), 0.0, Motion::Slide { seconds: PRESS }),
        );
        let at = 0.08;
        let edits = toward(&first, &held(&scenes[2]), at, Motion::Slide { seconds: PRESS });
        let second = played(&first, &edits);
        for window in [3, 4, 5] {
            let (before, after) = (drawn(&first, window, at), drawn(&second, window, at));
            assert!(
                after.is_within(1e-6, before),
                "w{window}: {before:?} to {after:?}"
            );
        }
        let node = Node::Container(strip_of(0));
        let bent = tracks(&edits).into_iter().find(|track| track.node() == node).unwrap();
        assert_eq!(bent.form(), Form::Samples, "a spring");
        let (Some(Value::Point(from)), Some(Value::Point(to))) =
            (first.along(node, at), second.model(node))
        else {
            panic!("the strip is placed")
        };
        let speed = first.rate(node, at)[0];
        assert!(speed.abs() > 100.0, "the strip was going at {speed}pt/s");
        let omega = spring_omega(PRESS);
        let Value::Point(next) = bent.keys()[1].1 else {
            panic!("a point")
        };
        let expected = spring_axis(from.x, to.x, speed, omega, SAMPLE_STEP).0;
        assert!((next.x - expected).abs() < 1e-6, "{} against {expected}", next.x);
        let landed = drawn(&second, 5, 2.0);
        assert!((landed.center().x - scenes[2].axis).abs() < 1e-6, "{landed:?}");
    }

    /// An empty workspace's outline rides its row's stacks, drawn where the scene outlines it while
    /// held, and at its tiling area on the display at rest when it is the one shown; it is selected
    /// while its row is.
    #[test]
    fn an_empty_workspace_is_outlined_where_its_row_draws_it() {
        let mut input = input(&STRIP, 3);
        let scene = lay_out(&input);
        let places = held(&scene);
        let [outline] = places.outlines[..] else {
            panic!("{:?}", places.outlines)
        };
        assert!(!outline.selected);
        let drawn = |places: &Places, outline: Outline| {
            let at = places.container(outline.key).unwrap();
            let laid = crate::animation::domain::motion::plan::overlay_of(outline.frame, at);
            OverlayRect::new(places.zoom.draw(laid.origin), laid.size * places.zoom.scale)
        };
        assert!(drawn(&places, outline).is_within(1e-6, scene.rows[2].outline.unwrap()));

        input.active = 2;
        let scene = lay_out(&input);
        let rest = at_rest(&scene, &[]);
        let [outline] = rest.outlines[..] else {
            panic!("{:?}", rest.outlines)
        };
        assert!(outline.selected);
        let tiling = to_overlay_space(crate::overview::domain::fixture::tiling(), display());
        assert!(
            drawn(&rest, outline).is_within(1e-6, tiling),
            "{:?}",
            drawn(&rest, outline)
        );
    }
}
