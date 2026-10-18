//! How the overview moves on the overlay: every row of its scene one strip container, its edge
//! stacks and its floating windows a container each, all under the strip's zoom; the zoom out from
//! the screen as it is and back in onto the real layout on the zoom look's arc, each half of it on
//! its own; every press inside it played Slide's way, each node from where it is drawn to where the
//! new scene has it; and the push at an end bounced on screen as far as a flight's.
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
use crate::animation::domain::motion::easing::{MOTION_CURVE, nudge_displacement};
use crate::animation::domain::motion::glide::{spring_axis, spring_omega};
use crate::animation::domain::motion::plan::{Banding, GroupKey, group_relative};
use crate::animation::domain::motion::score::{
    Channel, Curve, Edit, Node, SAMPLE_STEP, Score, Track, Value, Zoom,
};
use crate::decorations::domain::style::{Band, Decoration};
use crate::overview::domain::scene::{Holder, Scene};

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

/// How long a spring a press bends a node on runs, as a share of `1 / omega`: within 0.01% of its
/// destination by then, so its last sample lands on it.
const SPRING_RUNS: f64 = 12.0;

/// The containers a row's thumbs ride, three to a row: its strip, its edge stacks behind it and its
/// floating windows in front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Strip,
    Stacks,
    Floating,
}

fn key(row: usize, part: Part) -> GroupKey {
    let part = match part {
        Part::Strip => 1,
        Part::Stacks => 2,
        Part::Floating => 3,
    };
    GroupKey::Rigid((3 * row + part) as u16)
}

/// The strip container of `row`.
pub fn strip_of(row: usize) -> GroupKey { key(row, Part::Strip) }

/// Where every node of the overview is drawn once its tracks end: the strip's zoom, each
/// container's place, each thumb's container and frame in it, and each empty workspace's outline.
#[derive(Clone, Debug, PartialEq)]
pub struct Places {
    pub zoom: Zoom,
    pub containers: Vec<(GroupKey, OverlayPoint)>,
    pub tiles: Vec<(WindowId, GroupKey, GroupRect)>,
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
        Some(OverlayRect::new(
            self.zoom.draw(laid.origin),
            laid.size * self.zoom.scale,
        ))
    }
}

/// `decoration` with its ring and border as wide on screen under a zoom by `scale` as they are
/// drawn at rest.
pub fn screen_wide(decoration: Decoration, scale: f64) -> Decoration {
    let widened = |band: Option<Band>| {
        band.map(|band| Band {
            width: band.width / scale,
            ..band
        })
    };
    Decoration {
        ring: widened(decoration.ring),
        border: widened(decoration.border),
        ..decoration
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
/// stacks at its top and the display's left, its floating windows at the display's top-left, all
/// under the zoom by the scene's depth.
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
                (
                    key(row, Part::Floating),
                    unzoomed_point(scene_row.floating_origin, s),
                ),
            ]
        })
        .collect()
}

/// The scene as it is held: every row at its depth, each drawn thumb in its holder's container at
/// where the scene draws it, the selected window and its row in front.
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
    Places {
        zoom: Zoom {
            scale: s,
            shift: OverlayVector::zero(),
        },
        containers,
        tiles,
        banding: banding(scene),
        outlines,
    }
}

/// The scene as the display shows it at rest: the zoom at 1, its active row's windows each at its
/// frame in `shown`, and every other row where the stack has it, off the display above and below.
/// What the overview zooms out from as it opens, and back into as it closes.
pub fn at_rest(scene: &Scene, shown: &[(WindowId, OverlayRect)]) -> Places {
    let mut places = held(scene);
    let s = scene.scale;
    let active = scene.selected.row;
    let Some(row) = scene.rows.get(active) else {
        return places;
    };
    let display = unzoomed_point(row.floating_origin, s);
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
        Some(to_rest(tile.window)?.origin - laid.2.origin.to_vector().respace())
    });
    if let Some(anchor) = anchor
        && let Some((_, at)) = places.containers.iter_mut().find(|(k, _)| *k == strip)
    {
        *at = anchor;
    }
    let stacks = key(active, Part::Stacks);
    let stacks_at = places.container(stacks).unwrap_or(Point::origin());
    for (window, key, frame) in &mut places.tiles {
        if *key == stacks
            && let Some(rest) = to_rest(*window)
        {
            *frame = group_relative(rest, stacks_at);
        }
    }
    places
}

fn part_of(holder: Holder) -> Part {
    match holder {
        Holder::Row => Part::Strip,
        Holder::Stack { .. } => Part::Stacks,
        Holder::Floating => Part::Floating,
    }
}

/// Front to back: the selected row's containers first, its floating windows over its strip over
/// its stacks; in a strip the selected window first, in a stack its top card first.
fn banding(scene: &Scene) -> Banding {
    let mut rows: Vec<usize> = (0..scene.rows.len()).collect();
    rows.sort_by_key(|row| *row != scene.selected.row);
    let group_order = rows
        .iter()
        .flat_map(|row| [Part::Floating, Part::Strip, Part::Stacks].map(|part| key(*row, part)))
        .collect();
    let mut within = HashMap::new();
    for scene_row in &scene.rows {
        let mut strip_depth = 1;
        let mut floating_depth = 0;
        for tile in &scene_row.tiles {
            let depth = match tile.holder {
                Holder::Row if scene.selected.window == Some(tile.window) => 0,
                Holder::Row => {
                    strip_depth += 1;
                    strip_depth - 1
                }
                Holder::Stack { depth, .. } => depth,
                Holder::Floating => {
                    floating_depth += 1;
                    floating_depth - 1
                }
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
    let mut edits = vec![Edit::Place(Node::Strip, Value::Zoom(places.zoom))];
    for (key, at) in &places.containers {
        edits.push(Edit::Place(Node::Container(*key), Value::Point(*at)));
    }
    for (window, key, frame) in &places.tiles {
        edits.push(Edit::Ride(*window, *key));
        edits.push(Edit::Place(Node::Tile(*window), Value::Rect(*frame)));
    }
    edits.push(Edit::Bank(places.banding.clone()));
    edits
}

/// How a change of the overview moves, every node alike.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    /// Straight to where it ends: animation off, or Low Power Mode.
    Cut,
    /// The zoom out as it opens or in as it closes: one half of the zoom look's arc, over
    /// `seconds`.
    Arc { seconds: f64 },
    /// A press: Slide's curve, over `seconds`.
    Slide { seconds: f64 },
}

impl Motion {
    fn seconds(self) -> f64 {
        match self {
            Motion::Cut => 0.0,
            Motion::Arc { seconds } | Motion::Slide { seconds } => seconds.max(0.0),
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
        match motion {
            Motion::Arc { seconds } => Shape::Arc { seconds },
            Motion::Slide { seconds } => Shape::Slide { seconds },
            Motion::Cut => Shape::Cut,
        }
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
    Arc { seconds: f64 },
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
        Shape::Arc { seconds } => {
            let frames = (seconds / SAMPLE_STEP).ceil().max(1.0) as usize;
            let values = (0..=frames).map(|k| {
                let share = nudge_displacement(k as f64 / frames as f64 / 2.0);
                lerp(start, to, share)
            });
            Track::samples(node, Channel::Base, values, at, seconds)
        }
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

fn lerp(from: Value, to: Value, share: f64) -> Value {
    let (a, b) = (from.parts(), to.parts());
    from.with_parts(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * share))
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
                .flat_map(|row| {
                    [Part::Strip, Part::Stacks, Part::Floating].map(|part| key(row, part))
                })
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
    use crate::animation::domain::choreography::zoom::{ZOOM_STRETCH, arc_scale};
    use crate::animation::domain::motion::plan::EDGE_BOUNCE_OVERSHOOT;
    use crate::animation::domain::motion::score::Form;
    use crate::overview::domain::fixture::{Col, display, overview, rect, row, strip, w};
    use crate::overview::domain::scene::{Laid, OverviewInput, lay_out};
    use crate::overview::domain::stacks::Side;

    const PRESS: f64 = 0.35;
    const ARC: f64 = PRESS * ZOOM_STRETCH;
    const WIDE: f64 = 1400.0;
    const NARROW: f64 = 800.0;

    static IDS: [u32; 11] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

    /// Ten columns on the first workspace, the fifth a stack of two, a floating window over it, two
    /// columns on the second, none on the third and one on the fourth: the first shows, on
    /// `current`.
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
        let mut first = row(strip(&columns, 0.0), Some(current));
        first.floating.push(Laid {
            window: w(20),
            frame: rect(300.0, 200.0, 600.0, 400.0),
        });
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
    /// as the layout scrolls it, and the floating window where it floats.
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
            .chain(input.rows[0].floating.iter().map(|laid| (laid.window, on(laid.frame))))
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
    /// it, one its row stacks at an edge, the floating one and the selected one alike.
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

    /// The zoom out plays the first half of the zoom look's arc over its stretched length, from the
    /// screen as it is to every thumb where the scene draws it.
    #[test]
    fn the_entry_ends_with_every_tile_at_its_scene_place() {
        let scene = scene(&STRIP, 3);
        let shown = shown(&STRIP, 3);
        let rest = pose(&at_rest(&scene, &shown));
        let edits = toward(&rest, &held(&scene), 0.0, Motion::Arc { seconds: ARC });
        let score = played(&rest, &edits);
        for (window, frame) in &shown {
            let Some(at) = score.drawn(*window, 0.0) else { continue };
            assert!(at.is_within(1e-6, *frame), "no jump as it starts: {window:?}");
        }
        let Some(Value::Zoom(quarter)) = score.sample(Node::Strip, ARC / 4.0) else {
            panic!("the strip zooms")
        };
        assert!(
            (quarter.scale - arc_scale(scene.scale, 0.125)).abs() < 0.01,
            "{quarter:?}"
        );
        for scene_row in &scene.rows {
            for tile in scene_row.tiles.iter().filter(|tile| tile.drawn()) {
                let at = score.drawn(tile.window, ARC + 0.01).unwrap();
                assert!(at.is_within(1e-6, tile.frame), "{tile:?}: {at:?}");
            }
        }
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

    /// A card that stays in its stack through a pan stays where it is drawn at the edge, however the
    /// strip moves beside it; one leaving the stack is carried out of it from where it was drawn.
    #[test]
    fn a_stacked_card_stays_at_the_edge_while_its_row_pans() {
        let panned = |from: u32, to: u32| {
            let (before, after) = (scene(&STRIP, from), scene(&STRIP, to));
            let score = pose(&held(&before));
            let edits = toward(&score, &held(&after), 0.0, Motion::Slide { seconds: PRESS });
            let still = !tracks(&edits).iter().any(|track| track.node() == Node::Tile(w(9)));
            (played(&score, &edits), before, (after, still))
        };
        let (score, before, (after, still)) = panned(6, 7);
        assert!(still, "its stack holds it still: nothing moves it");
        let stays = before.find(w(9)).unwrap().1.holder;
        assert!(matches!(stays, Holder::Stack { side: Side::Right, depth: 0 }));
        assert_eq!(after.find(w(9)).unwrap().1.holder, stays);
        let at = |t: f64| drawn(&score, 9, t);
        for t in [0.0, 0.05, PRESS / 2.0, PRESS - 0.01, PRESS + 0.01] {
            assert!(at(t).is_within(1e-6, at(0.0)), "{t}: {:?}", at(t));
        }

        let (score, before, (after, _)) = panned(5, 6);
        assert!(matches!(before.find(w(7)).unwrap().1.holder, Holder::Stack {
            side: Side::Right,
            ..
        }));
        assert_eq!(after.find(w(7)).unwrap().1.holder, Holder::Row);
        assert!(drawn(&score, 7, 0.0).is_within(1e-6, before.find(w(7)).unwrap().1.frame));
        assert!(drawn(&score, 7, PRESS + 0.01).is_within(1e-6, after.find(w(7)).unwrap().1.frame));
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

    /// Closing zooms back in on the arc's second half onto the real layout: the workspace shown
    /// then, each of its windows landing on its real frame, the selected one as it was scrolled to.
    #[test]
    fn the_exit_lands_every_tile_on_its_real_frame() {
        let scene = scene(&STRIP, 8);
        let landing = shown(&STRIP, 8);
        let score = pose(&held(&scene));
        let edits = toward(&score, &at_rest(&scene, &landing), 0.0, Motion::Arc {
            seconds: ARC,
        });
        let score = played(&score, &edits);
        for scene_row in &scene.rows {
            for tile in scene_row.tiles.iter().filter(|tile| tile.drawn()) {
                let at = score.drawn(tile.window, 0.0).unwrap();
                assert!(at.is_within(1e-6, tile.frame), "no jump as it starts: {tile:?}");
            }
        }
        for (window, frame) in &landing {
            let Some(at) = score.drawn(*window, ARC + 0.01) else {
                continue;
            };
            assert!(
                at.is_within(1e-6, *frame),
                "{window:?}: {at:?} against {frame:?}"
            );
        }
        let Some(Value::Zoom(zoom)) = score.sample(Node::Strip, ARC + 0.01) else {
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
            &toward(&start, &held(&scene), 0.0, Motion::Arc { seconds: ARC }),
        );
        let turn = ARC * 0.4;
        let edits = toward(&entering, &rest, turn, Motion::Arc { seconds: ARC });
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
        for window in [1, 11, 20] {
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
