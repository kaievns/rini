//! A pass as rigid pieces: the pure half of the container model.
//! See "The overlay engine" in `src/animation/docs/animation-smoothness.md`, and "Layout changes and
//! strip movements" in `src/transitions/docs/coordination.md` for what starts a flight.

use std::collections::HashMap;

use rini_core::ids::WindowId;
use rini_geometry::space::{
    Group, GroupRect, Overlay, OverlayPoint, OverlayRect, OverlayVector, Rect, Respace, ScreenRect,
    Size, SurfacePoint, Translation,
};
use rini_geometry::stretch::is_a_resize;
use rini_geometry::{SameAs, to_overlay_space};
use rini_ipc::protocol::Direction;

use crate::animation::domain::motion::surface::{SurfaceGeometry, pan_travel};

mod merge;

pub use merge::merge_plans;

/// Two translation vectors this close on both axes ride one container.
pub const GROUP_TOLERANCE: f64 = 2.0;

/// Which container a member lives in. Stable for the flight; a reparent changes a member's key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GroupKey {
    /// A rigid strip piece. `0` is the still group; others are allocated in plan order.
    Rigid(u16),
    /// Changing members and entrances: strip band, per-tile animations.
    Loose,
    Floating,
}

impl GroupKey {
    pub const STILL: GroupKey = GroupKey::Rigid(0);
}

/// A window of a layout pass, its tile going from `start` to `end` on the display, and whether it
/// floats: what [`reflow_plan`] takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolved {
    pub window: WindowId,
    pub start: ScreenRect,
    pub end: ScreenRect,
    pub floating: bool,
}

impl Resolved {
    /// A window of the strip.
    pub fn tiled(window: WindowId, start: ScreenRect, end: ScreenRect) -> Self {
        Self {
            window,
            start,
            end,
            floating: false,
        }
    }

    /// A floating window.
    pub fn floating(window: WindowId, start: ScreenRect, end: ScreenRect) -> Self {
        Self {
            window,
            start,
            end,
            floating: true,
        }
    }
}

/// A window riding no rigid group, on a path of its own in its container: a resize, an entrance or a
/// floating window, from `from` to `to`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OwnPath {
    pub window: WindowId,
    pub from: GroupRect,
    pub to: GroupRect,
}

/// One rigid piece of the strip.
#[derive(Clone, Debug, PartialEq)]
pub struct RigidGroup {
    pub key: GroupKey,
    /// Total displacement of the container from install to the current destination.
    pub travel: OverlayVector,
    /// Members with their frames in it (constant for the flight unless reparented).
    pub members: Vec<GroupMember>,
}

impl RigidGroup {
    fn new(key: GroupKey, travel: OverlayVector) -> Self {
        RigidGroup {
            key,
            travel,
            members: Vec::new(),
        }
    }

    pub fn is_still(&self) -> bool { self.travel == OverlayVector::zero() }
}

/// A tile riding a container. The picture itself stays in `RunningAnimation.tiles`, found by window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupMember {
    pub window: WindowId,
    /// Frame inside the container: `from` less the container's position at install.
    pub rel: GroupRect,
}

/// How one window takes part in a plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Member {
    /// Rigid: rides its container. No per-tile animation.
    Rigid { key: GroupKey, rel: GroupRect },
    /// Its size changes: own layer in `GroupKey::Loose`, animated as a resize from `from` to `to`.
    Changing { from: GroupRect, to: GroupRect },
    /// New window: own layer in `GroupKey::Loose`, from spawn frame to slot, crop-drawn with its spawn
    /// picture and its ground where that picture ends until its slot picture lands.
    Entrance { from: GroupRect, to: GroupRect },
    /// In the floating container; animated per tile only when `from != to`.
    Floating { from: GroupRect, to: GroupRect },
}

impl Member {
    /// The frame this member starts at, inside its container.
    ///
    /// A `Rigid` member is not moving relative to its container — the container moves and carries it
    /// — so it has no separate `from` and its `rel` is both start and end. Every other variant is
    /// animated in its own right and starts where `from` says.
    ///
    /// The distinction matters when a window is REPARENTED into a different container mid-flight:
    /// the layer has to be installed at the frame the animation is about to run from, and reading
    /// `to` there would land it at its destination and animate nowhere.
    pub fn start_frame(&self) -> GroupRect {
        match self {
            Self::Rigid { rel, .. } => *rel,
            Self::Changing { from, .. }
            | Self::Entrance { from, .. }
            | Self::Floating { from, .. } => *from,
        }
    }
}

/// Where an entering window grows in from: zero width at its own left edge, full height.
pub fn entrance_from<U>(to: Rect<U>) -> Rect<U> {
    Rect::new(to.origin, Size::new(0.0, to.size.height))
}

/// A pass, described as rigid pieces. Pure output of [`reflow_plan`] / [`surface_plan`]. Its
/// containers install at the overlay's origin (`installed`).
#[derive(Clone, Debug, PartialEq)]
pub struct ReflowPlan {
    /// `groups[0]` is the still group, always present, possibly empty.
    pub groups: Vec<RigidGroup>,
    pub changing: Vec<OwnPath>,
    pub entrances: Vec<OwnPath>,
    pub floating: Vec<OwnPath>,
    /// Travel the floating container itself takes (a switch); zero for a pan or a layout pass.
    pub floating_travel: OverlayVector,
}

impl ReflowPlan {
    pub fn empty() -> Self {
        ReflowPlan {
            groups: vec![RigidGroup::new(GroupKey::STILL, OverlayVector::zero())],
            changing: Vec::new(),
            entrances: Vec::new(),
            floating: Vec::new(),
            floating_travel: OverlayVector::zero(),
        }
    }

    /// Puts a rigid member starting at `from` into the group travelling by `vector`, opening one
    /// when none is within `GROUP_TOLERANCE`.
    fn place(&mut self, window: WindowId, from: GroupRect, vector: OverlayVector) -> GroupKey {
        let member = GroupMember { window, rel: from };
        if let Some(group) = self.groups.iter_mut().find(|g| same_vector(g.travel, vector)) {
            group.members.push(member);
            return group.key;
        }
        let key = GroupKey::Rigid(self.groups.len() as u16);
        let mut group = RigidGroup::new(key, vector);
        group.members.push(member);
        self.groups.push(group);
        key
    }

    /// The group `window` rides, if it is a rigid member.
    #[cfg(test)]
    pub fn group_of(&self, window: WindowId) -> Option<&RigidGroup> {
        self.groups.iter().find(|g| g.members.iter().any(|m| m.window == window))
    }

    /// How `window` takes part, or `None` when the plan does not name it.
    #[cfg(test)]
    pub fn member(&self, window: WindowId) -> Option<Member> {
        member_in(
            &self.groups,
            &self.changing,
            &self.entrances,
            &self.floating,
            window,
        )
    }

    /// Every window the plan names, in plan order: groups, changing, entrances, floating.
    #[cfg(test)]
    pub fn windows(&self) -> Vec<WindowId> {
        windows_in(&self.groups, &self.changing, &self.entrances, &self.floating)
    }
}

/// How `window` takes part in a plan's lists, or `None` when none names it.
fn member_in(
    groups: &[RigidGroup],
    changing: &[OwnPath],
    entrances: &[OwnPath],
    floating: &[OwnPath],
    window: WindowId,
) -> Option<Member> {
    for group in groups {
        if let Some(m) = group.members.iter().find(|m| m.window == window) {
            return Some(Member::Rigid { key: group.key, rel: m.rel });
        }
    }
    let find = |list: &[OwnPath]| {
        list.iter().find(|path| path.window == window).map(|path| (path.from, path.to))
    };
    if let Some((from, to)) = find(changing) {
        return Some(Member::Changing { from, to });
    }
    if let Some((from, to)) = find(entrances) {
        return Some(Member::Entrance { from, to });
    }
    if let Some((from, to)) = find(floating) {
        return Some(Member::Floating { from, to });
    }
    None
}

/// Every window in the lists, in plan order.
#[cfg(test)]
fn windows_in(
    groups: &[RigidGroup],
    changing: &[OwnPath],
    entrances: &[OwnPath],
    floating: &[OwnPath],
) -> Vec<WindowId> {
    let mut out: Vec<WindowId> =
        groups.iter().flat_map(|g| g.members.iter().map(|m| m.window)).collect();
    out.extend(changing.iter().chain(entrances).chain(floating).map(|path| path.window));
    out
}

/// The running flight's plan. Adds container state the overlay needs to retarget.
#[derive(Clone, Debug, PartialEq)]
pub struct FlightPlan {
    pub groups: Vec<RigidGroup>,
    /// Model position of each container: install position plus every retarget's travel.
    pub positions: HashMap<GroupKey, OverlayPoint>,
    pub changing: Vec<OwnPath>,
    pub entrances: Vec<OwnPath>,
    pub floating: Vec<OwnPath>,
    pub floating_travel: OverlayVector,
    pub next_key: u16,
}

impl FlightPlan {
    /// A flight with nothing in it: the still group only.
    #[cfg(test)]
    pub fn empty() -> Self { FlightPlan::from(ReflowPlan::empty()) }

    /// How `window` takes part. Floating frames are in the floating container's space.
    pub fn member(&self, window: WindowId) -> Option<Member> {
        member_in(
            &self.groups,
            &self.changing,
            &self.entrances,
            &self.floating,
            window,
        )
    }

    /// Where `window` lands on the overlay, whatever it rides: a rigid member where its container
    /// ends, any other where the loose or the floating container does.
    pub fn destination(&self, window: WindowId) -> Option<OverlayRect> {
        Some(match self.member(window)? {
            Member::Rigid { key, rel } => overlay_of(rel, self.position_of(key)),
            Member::Changing { to, .. } | Member::Entrance { to, .. } => {
                overlay_of(to, self.position_of(GroupKey::Loose))
            }
            Member::Floating { to, .. } => overlay_of(to, self.position_of(GroupKey::Floating)),
        })
    }

    /// Every window the flight names.
    #[cfg(test)]
    pub fn windows(&self) -> Vec<WindowId> {
        windows_in(&self.groups, &self.changing, &self.entrances, &self.floating)
    }
}

impl From<ReflowPlan> for FlightPlan {
    /// A fresh flight: every container installs at the overlay's origin, so each group's model
    /// position is its travel from there.
    fn from(plan: ReflowPlan) -> Self {
        let mut positions: HashMap<GroupKey, OverlayPoint> =
            plan.groups.iter().map(|g| (g.key, g.travel.to_point())).collect();
        positions.insert(GroupKey::Loose, OverlayPoint::origin());
        positions.insert(GroupKey::Floating, plan.floating_travel.to_point());
        let next_key = plan.groups.len() as u16;
        FlightPlan {
            groups: plan.groups,
            positions,
            changing: plan.changing,
            entrances: plan.entrances,
            floating: plan.floating,
            floating_travel: plan.floating_travel,
            next_key,
        }
    }
}

/// Where every container and tile sits front to back. Pure output of `band_plan`: the floating
/// windows split between the band in front of the strip and the band behind it, strip containers in
/// `group_order`, each tile at its within-band depth. `container_z - within` is `-depth`
/// (`motion/z_group.rs`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Banding {
    /// Whether the window gaining focus is off the strip, which moves the strip back a band.
    pub focus_off_strip: bool,
    /// The floating windows drawn in front of the strip: the application gaining focus, and any the
    /// server already has there. Every other floating window is drawn behind it.
    pub lifted: Vec<WindowId>,
    /// Depth inside its container per tile.
    pub within: HashMap<WindowId, usize>,
    /// Strip containers front to back: the one holding focus first, then by shallowest member.
    pub group_order: Vec<GroupKey>,
}

/// Where each container of a flight is at the moment of a merge. See "Mid-flight passes" in
/// `src/animation/docs/animation-smoothness.md` for where a look takes it from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Presented {
    /// Where it is drawn, any bounce or nudge riding it included: where a member leaving it starts.
    pub drawn: HashMap<GroupKey, OverlayPoint>,
    /// Where its movement has it, with nothing riding it: what a member leaving it still has to
    /// travel is judged against.
    pub along: HashMap<GroupKey, OverlayPoint>,
}

/// A flight with nothing riding its containers: each is drawn where its leg has it.
#[cfg(test)]
impl From<HashMap<GroupKey, OverlayPoint>> for Presented {
    fn from(positions: HashMap<GroupKey, OverlayPoint>) -> Self {
        Presented {
            drawn: positions.clone(),
            along: positions,
        }
    }
}

/// A container a merge opens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NewGroup {
    pub key: GroupKey,
    /// Where it installs; its members and destination are in the plan.
    pub install: OverlayPoint,
    /// The container its member left, whose motion it carries on; `None` for a newcomer's.
    pub leaving: Option<GroupKey>,
}

/// What one merge changed, for the overlay. Pure output of [`merge_plans`]; every frame the
/// overlay needs is in the merged plan, found by window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanDelta {
    /// Containers to animate from their presented position to `to`.
    pub retargeted_groups: Vec<(GroupKey, OverlayPoint)>,
    pub new_groups: Vec<NewGroup>,
    /// Members moving container: (window, from_key, to_key). The new frame is in the plan.
    pub reparented: Vec<(WindowId, GroupKey, GroupKey)>,
    /// Loose tiles to bend toward a new destination.
    pub retargeted_tiles: Vec<(WindowId, GroupRect)>,
    /// Tiles joining the flight, with their container; frames are in the plan.
    pub joined_tiles: Vec<(WindowId, GroupKey)>,
    pub focus_changed: bool,
}

impl PlanDelta {
    /// A merge that changed nothing: the redundant pass of a rapid press.
    pub fn is_empty(&self) -> bool { !self.moves_anything() && !self.focus_changed }

    /// Whether any geometry changed: what restarts the flight's clock.
    pub fn moves_anything(&self) -> bool {
        !(self.retargeted_groups.is_empty()
            && self.new_groups.is_empty()
            && self.reparented.is_empty()
            && self.retargeted_tiles.is_empty()
            && self.joined_tiles.is_empty())
    }
}

/// Where a window lives in a flight plan.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Located {
    Group(GroupKey, GroupRect),
    Changing(usize),
    Entrance(usize),
    Floating(usize),
    Absent,
}

impl FlightPlan {
    fn locate(&self, window: WindowId) -> Located {
        for group in &self.groups {
            if let Some(m) = group.members.iter().find(|m| m.window == window) {
                return Located::Group(group.key, m.rel);
            }
        }
        let index = |list: &[OwnPath]| list.iter().position(|path| path.window == window);
        if let Some(i) = index(&self.changing) {
            return Located::Changing(i);
        }
        if let Some(i) = index(&self.entrances) {
            return Located::Entrance(i);
        }
        if let Some(i) = index(&self.floating) {
            return Located::Floating(i);
        }
        Located::Absent
    }

    fn position(&self, key: GroupKey) -> OverlayPoint {
        self.positions.get(&key).copied().unwrap_or(OverlayPoint::origin())
    }

    /// Model position of container `key`: where its members' `rel` frames land.
    pub fn position_of(&self, key: GroupKey) -> OverlayPoint { self.position(key) }

    fn group_mut(&mut self, key: GroupKey) -> &mut RigidGroup {
        self.groups.iter_mut().find(|g| g.key == key).expect("a key the plan allocated")
    }

    fn take_member(&mut self, key: GroupKey, window: WindowId) -> GroupMember {
        let group = self.group_mut(key);
        let at = group.members.iter().position(|m| m.window == window).expect("located there");
        group.members.remove(at)
    }

    /// Moves container `key`'s destination to `p`; its travel grows by the same amount.
    fn move_group(&mut self, key: GroupKey, p: OverlayPoint, delta: &mut PlanDelta) {
        let old = self.position(key);
        if p.same_as(old) {
            return;
        }
        self.positions.insert(key, p);
        let group = self.group_mut(key);
        group.travel =
            OverlayVector::new(group.travel.x + p.x - old.x, group.travel.y + p.y - old.y);
        match delta.retargeted_groups.iter_mut().find(|(k, _)| *k == key) {
            Some(retarget) => retarget.1 = p,
            None => delta.retargeted_groups.push((key, p)),
        }
    }

    /// Opens a group installing at `install` and landing at `destination`, with one member, which
    /// left container `leaving` if it was in one.
    fn open_group(
        &mut self,
        install: OverlayPoint,
        destination: OverlayPoint,
        member: GroupMember,
        leaving: Option<GroupKey>,
        presented: &mut Presented,
        delta: &mut PlanDelta,
    ) -> GroupKey {
        let key = GroupKey::Rigid(self.next_key);
        self.next_key += 1;
        let mut group = RigidGroup::new(key, destination - install);
        group.members.push(member);
        self.groups.push(group);
        self.positions.insert(key, destination);
        presented.drawn.insert(key, install);
        presented.along.insert(key, install);
        delta.new_groups.push(NewGroup { key, install, leaving });
        key
    }
}

/// Two translation vectors within `GROUP_TOLERANCE` on both axes.
pub fn same_vector(a: OverlayVector, b: OverlayVector) -> bool {
    (a.x - b.x).abs() <= GROUP_TOLERANCE && (a.y - b.y).abs() <= GROUP_TOLERANCE
}

/// A frame in its container: `frame` less the container's position.
pub fn group_relative(frame: OverlayRect, position: OverlayPoint) -> GroupRect {
    container_at(position).inverse().transform_rect(&frame)
}

/// The inverse of [`group_relative`]: a frame in its container back on the overlay.
pub fn overlay_of(rel: GroupRect, position: OverlayPoint) -> OverlayRect {
    container_at(position).transform_rect(&rel)
}

fn container_at(position: OverlayPoint) -> Translation<Group, Overlay> {
    Translation::new(position.x, position.y)
}

/// `frame` in a container of a fresh plan, which installs at the overlay's origin: the same place.
pub fn installed(frame: OverlayRect) -> GroupRect {
    Rect::new(
        frame.origin.to_vector().respace().to_point(),
        frame.size.respace(),
    )
}

/// The inverse of [`installed`]: a fresh plan's frame on the overlay.
pub fn on_overlay(rel: GroupRect) -> OverlayRect {
    Rect::new(rel.origin.to_vector().respace().to_point(), rel.size.respace())
}

/// A layout pass as rigid pieces. `requests` are after `start` has resolved parks and dropped what
/// is not worth animating.
pub fn reflow_plan(requests: &[Resolved], display: ScreenRect) -> ReflowPlan {
    let mut plan = ReflowPlan::empty();
    for &Resolved { window, start, end, floating } in requests {
        let from = to_overlay_space(start, display);
        let to = to_overlay_space(end, display);
        let path = OwnPath {
            window,
            from: installed(from),
            to: installed(to),
        };
        if floating {
            plan.floating.push(path);
        } else if is_a_resize(from.size, to.size) {
            plan.changing.push(path);
        } else {
            plan.place(window, installed(from), to.origin - from.origin);
        }
    }
    plan
}

/// A strip movement as rigid pieces: the strip is one piece, and each column a move swapped that
/// crosses it is a piece of its own, grouped by the vector it travels. Pinned windows stand in the
/// floating container; unpinned floating windows ride it by the strip's travel.
pub fn surface_plan<W: SurfaceGeometry>(
    windows: &[W],
    from_offset: SurfacePoint,
    to_offset: SurfacePoint,
) -> ReflowPlan {
    let travel = pan_travel(from_offset, to_offset);
    let mut plan = ReflowPlan::empty();
    // A pan pins its floating windows; a switch pins none. Both in one pass would need the floating
    // container to move some tiles and not others, so that case falls back to per-tile floating moves.
    let mixed =
        windows.iter().any(|w| w.pinned()) && windows.iter().any(|w| w.floating() && !w.pinned());
    for window in windows {
        let (from, to) = window.travel(from_offset, to_offset);
        if window.pinned() {
            plan.floating.push(OwnPath {
                window: window.window(),
                from: installed(from),
                to: installed(from),
            });
        } else if window.floating() {
            if mixed {
                plan.floating.push(OwnPath {
                    window: window.window(),
                    from: installed(from),
                    to: installed(to),
                });
            } else {
                plan.floating.push(OwnPath {
                    window: window.window(),
                    from: installed(from),
                    to: installed(from),
                });
                plan.floating_travel = travel;
            }
        } else if window.from().is_some() {
            plan.place(window.window(), installed(from), to.origin - from.origin);
        } else {
            plan.place(window.window(), installed(from), travel);
        }
    }
    plan
}

/// Whether the container `key` names takes part in an out-and-back by `offset`, an edge bounce or
/// a move's nudge. Strip containers always; the floating container only when it is vertical, the
/// same rule a pan (floating pinned) and a switch (floating carried) follow. A nudge is always
/// sideways, so it steps the whole strip and never a floating window.
pub fn carries_out_and_back(key: GroupKey, offset: OverlayVector) -> bool {
    match key {
        GroupKey::Floating => offset.y != 0.0,
        GroupKey::Rigid(_) | GroupKey::Loose => true,
    }
}

/// How far the surface gives when a command pushes past an end, in points. Enough to read as
/// the view straining against a stop, small enough that no column leaves its place.
///
/// 36 at first, which was reported as too small to notice: on a 1720pt strip it is 2% of the
/// viewport, and a glance away misses it. 72 is 4%, still well inside the inner gap's own column so
/// nothing appears to swap places.
pub const EDGE_BOUNCE_OVERSHOOT: f64 = 72.0;

/// The surface's nudge for a push in `direction`: the way the view was pushed, so the content
/// moves the opposite way, as it would have had there been anything further. Focus right at the
/// last column pulls the strip left; the next workspace at the bottom of the stack pulls the
/// row up.
pub fn edge_bounce_overshoot(direction: Direction) -> OverlayVector {
    match direction {
        Direction::Left => OverlayVector::new(EDGE_BOUNCE_OVERSHOOT, 0.0),
        Direction::Right => OverlayVector::new(-EDGE_BOUNCE_OVERSHOOT, 0.0),
        Direction::Up => OverlayVector::new(0.0, EDGE_BOUNCE_OVERSHOOT),
        Direction::Down => OverlayVector::new(0.0, -EDGE_BOUNCE_OVERSHOOT),
    }
}
/// One movement a flight's motion carries: a container's translation, or one loose tile's own
/// animation. Pure output of [`animation_targets`], which a look composes its tracks from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnimationTarget {
    Container {
        key: GroupKey,
        from: OverlayPoint,
        to: OverlayPoint,
    },
    Tile {
        window: WindowId,
        from: GroupRect,
        to: GroupRect,
    },
}

/// What a flight animates, and nothing else: one `Container` per strip group with travel, the
/// floating container when it travels, and a `Tile` per changing, entrance and moving floating
/// member. Rigid members are never named. See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
pub fn animation_targets(plan: &FlightPlan) -> Vec<AnimationTarget> {
    let mut out = Vec::new();
    let position = |key: GroupKey, travel: OverlayVector| {
        plan.positions.get(&key).copied().unwrap_or(travel.to_point())
    };
    for group in plan.groups.iter().filter(|g| !g.is_still()) {
        let to = position(group.key, group.travel);
        let from = to - group.travel;
        out.push(AnimationTarget::Container { key: group.key, from, to });
    }
    let ft = plan.floating_travel;
    if ft != OverlayVector::zero() {
        let to = position(GroupKey::Floating, ft);
        let from = to - ft;
        out.push(AnimationTarget::Container {
            key: GroupKey::Floating,
            from,
            to,
        });
    }
    for &OwnPath { window, from, to } in plan.changing.iter().chain(&plan.entrances) {
        out.push(AnimationTarget::Tile { window, from, to });
    }
    for &OwnPath { window, from, to } in &plan.floating {
        if !from.same_as(to) {
            out.push(AnimationTarget::Tile { window, from, to });
        }
    }
    out
}

/// What a flight stops drawing once a pass is in: the tiles of windows it no longer draws (`keep`
/// names the rest), and the containers left with nothing in them. `tiles` is every tile drawn, with
/// the container it rides.
///
/// Two stages, and the order between them is the point. A container is judged empty by the tiles
/// that REMAIN, so the tiles have to go first. Judging containers against the original set keeps a
/// container whose only tile just left, and an empty container is a layer the compositor keeps
/// compositing.
pub fn stale_overlay_layers<K: Copy + PartialEq>(
    tiles: &[(WindowId, K)],
    keep: &[WindowId],
    containers: &[K],
) -> (Vec<WindowId>, Vec<K>) {
    let stale_tiles: Vec<WindowId> = tiles
        .iter()
        .map(|(window, _)| *window)
        .filter(|window| !keep.contains(window))
        .collect();
    let occupied: Vec<K> = tiles
        .iter()
        .filter(|(window, _)| !stale_tiles.contains(window))
        .map(|(_, key)| *key)
        .collect();
    let empty: Vec<K> = containers.iter().copied().filter(|key| !occupied.contains(key)).collect();
    (stale_tiles, empty)
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Point, ScreenVector, SurfaceRect, Vector};
    use rini_geometry::{from_overlay_space, to_overlay_space};

    use super::*;
    use crate::animation::domain::motion::surface::viewport;
    use crate::animation::domain::motion::testing::DISPLAY;

    fn rect<U>(x: f64, y: f64, w: f64, h: f64) -> Rect<U> {
        Rect::new(Point::new(x, y), Size::new(w, h))
    }

    /// `frame` moved by `delta`.
    fn shifted<U>(frame: Rect<U>, delta: Vector<U>) -> Rect<U> {
        Rect::new(
            Point::new(frame.origin.x + delta.x, frame.origin.y + delta.y),
            frame.size,
        )
    }

    /// A frame on `DISPLAY`, which sits at the origin, on the overlay over it.
    fn on(frame: ScreenRect) -> OverlayRect { to_overlay_space(frame, DISPLAY) }

    /// An overlay frame back on `DISPLAY`.
    fn screen(frame: OverlayRect) -> ScreenRect { from_overlay_space(frame, DISPLAY) }

    /// A frame on `DISPLAY` as a fresh plan holds it, its containers at the overlay's origin.
    fn held(frame: ScreenRect) -> GroupRect { installed(on(frame)) }

    /// A frame on `DISPLAY` on the strip surface, as the viewport at rest shows it.
    fn on_surface(frame: ScreenRect) -> SurfaceRect {
        viewport(SurfacePoint::origin()).inverse().transform_rect(&on(frame))
    }

    #[test]
    fn each_member_lands_where_its_container_ends_and_a_window_the_plan_does_not_name_nowhere() {
        let wid = |idx| WindowId::new(7, idx);
        let mut reflow = ReflowPlan::empty();
        reflow.groups[0].members.push(GroupMember {
            window: wid(1),
            rel: rect(10.0, 20.0, 300.0, 400.0),
        });
        reflow.groups.push(RigidGroup {
            key: GroupKey::Rigid(1),
            travel: OverlayVector::new(-863.0, 0.0),
            members: vec![GroupMember {
                window: wid(2),
                rel: rect(900.0, 20.0, 300.0, 400.0),
            }],
        });
        reflow.changing.push(OwnPath {
            window: wid(3),
            from: rect(0.0, 0.0, 100.0, 100.0),
            to: rect(50.0, 60.0, 200.0, 100.0),
        });
        reflow.entrances.push(OwnPath {
            window: wid(4),
            from: rect(0.0, 0.0, 0.0, 100.0),
            to: rect(70.0, 80.0, 90.0, 100.0),
        });
        reflow.floating.push(OwnPath {
            window: wid(5),
            from: rect(0.0, 0.0, 50.0, 50.0),
            to: rect(100.0, 100.0, 50.0, 50.0),
        });
        reflow.floating_travel = OverlayVector::new(0.0, 1117.0);
        let mut plan = FlightPlan::from(reflow);
        plan.positions.insert(GroupKey::Loose, OverlayPoint::new(5.0, 7.0));

        assert_eq!(plan.destination(wid(1)), Some(rect(10.0, 20.0, 300.0, 400.0)));
        assert_eq!(plan.destination(wid(2)), Some(rect(37.0, 20.0, 300.0, 400.0)));
        assert_eq!(plan.destination(wid(3)), Some(rect(55.0, 67.0, 200.0, 100.0)));
        assert_eq!(plan.destination(wid(4)), Some(rect(75.0, 87.0, 90.0, 100.0)));
        assert_eq!(plan.destination(wid(5)), Some(rect(100.0, 1217.0, 50.0, 50.0)));
        assert_eq!(plan.destination(wid(6)), None);
    }

    /// `animation_targets`: what a look composes the motion from and nothing else (Requirement 11.4 of
    /// `rigid-strip-groups`). Plans built with `reflow_plan` on a 1728x1117 display at the origin,
    /// so a frame on the display, on the overlay and in a fresh container are the same numbers.
    mod targets {
        use super::*;

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
        }

        fn column(i: f64) -> ScreenRect { rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0) }

        fn shifted(frame: ScreenRect, dx: f64) -> ScreenRect {
            super::shifted(frame, ScreenVector::new(dx, 0.0))
        }

        fn flight(requests: &[Resolved]) -> FlightPlan {
            FlightPlan::from(reflow_plan(requests, DISPLAY))
        }

        fn containers(targets: &[AnimationTarget]) -> Vec<GroupKey> {
            targets
                .iter()
                .filter_map(|t| match t {
                    AnimationTarget::Container { key, .. } => Some(*key),
                    AnimationTarget::Tile { .. } => None,
                })
                .collect()
        }

        fn tiles(targets: &[AnimationTarget]) -> Vec<WindowId> {
            targets
                .iter()
                .filter_map(|t| match t {
                    AnimationTarget::Tile { window, .. } => Some(*window),
                    AnimationTarget::Container { .. } => None,
                })
                .collect()
        }

        #[test]
        fn a_one_group_plan_is_exactly_one_container_target() {
            let (a, b) = (column(0.0), column(1.0));
            let plan = flight(&[
                Resolved::tiled(wid(1), a, shifted(a, -859.0)),
                Resolved::tiled(wid(2), b, shifted(b, -859.0)),
            ]);
            let targets = animation_targets(&plan);
            assert_eq!(targets, vec![AnimationTarget::Container {
                key: GroupKey::Rigid(1),
                from: OverlayPoint::new(0.0, 0.0),
                to: OverlayPoint::new(-859.0, 0.0),
            }]);
        }

        #[test]
        fn a_still_group_and_two_moving_groups_are_two_container_targets() {
            let (a, b, c) = (column(0.0), column(1.0), column(2.0));
            let plan = flight(&[
                Resolved::tiled(wid(1), a, a),
                Resolved::tiled(wid(2), b, shifted(b, 300.0)),
                Resolved::tiled(wid(3), c, shifted(c, -300.0)),
            ]);
            let targets = animation_targets(&plan);
            assert_eq!(containers(&targets), vec![
                GroupKey::Rigid(1),
                GroupKey::Rigid(2)
            ]);
            assert!(tiles(&targets).is_empty(), "no rigid member is a tile target");
        }

        #[test]
        fn a_changing_member_is_a_tile_target_and_rigid_members_are_not() {
            let (a, b) = (column(0.0), column(1.0));
            let grown = rect(a.origin.x, a.origin.y, a.size.width + 400.0, a.size.height);
            let plan = flight(&[
                Resolved::tiled(wid(1), a, grown),
                Resolved::tiled(wid(2), b, shifted(b, 400.0)),
            ]);
            let targets = animation_targets(&plan);
            assert_eq!(tiles(&targets), vec![wid(1)]);
            assert_eq!(containers(&targets), vec![GroupKey::Rigid(1)]);
            assert!(targets.contains(&AnimationTarget::Tile {
                window: wid(1),
                from: held(a),
                to: held(grown)
            }));
        }

        #[test]
        fn the_floating_container_is_a_target_iff_it_travels() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let mut plan = flight(&[Resolved::floating(wid(1), settings, settings)]);
            assert!(
                animation_targets(&plan).is_empty(),
                "a standing floating window: nothing flies"
            );

            plan.floating_travel = OverlayVector::new(0.0, -1117.0);
            plan.positions.insert(GroupKey::Floating, plan.floating_travel.to_point());
            assert_eq!(animation_targets(&plan), vec![AnimationTarget::Container {
                key: GroupKey::Floating,
                from: OverlayPoint::new(0.0, 0.0),
                to: OverlayPoint::new(0.0, -1117.0),
            }]);

            let moved = shifted(settings, -100.0);
            let plan = flight(&[Resolved::floating(wid(1), settings, moved)]);
            assert_eq!(
                animation_targets(&plan),
                vec![AnimationTarget::Tile {
                    window: wid(1),
                    from: held(settings),
                    to: held(moved)
                }],
                "a floating window moving on its own is a tile target, not a container"
            );
        }

        #[test]
        fn zero_travel_groups_are_never_targets() {
            let (a, b) = (column(0.0), column(1.0));
            let plan = flight(&[Resolved::tiled(wid(1), a, a), Resolved::tiled(wid(2), b, b)]);
            assert!(animation_targets(&plan).is_empty());
            assert_eq!(plan.groups.len(), 1, "everything standing is the still group");
        }
    }

    /// Strip containers always bounce and step; the floating container only with the stack
    /// (vertical), the way a pan pins floating windows and a switch carries them. A move's step is
    /// sideways, so it never carries a floating window.
    #[test]
    fn the_floating_container_bounces_only_vertically_and_never_steps() {
        let sideways = OverlayVector::new(-EDGE_BOUNCE_OVERSHOOT, 0.0);
        let upward = OverlayVector::new(0.0, -EDGE_BOUNCE_OVERSHOOT);
        let share = 1728.0 * crate::animation::domain::motion::nudge::NUDGE_SHARE;
        let steps = [
            OverlayVector::new(-share, 0.0),
            OverlayVector::new(share, 0.0),
        ];
        for key in [GroupKey::Rigid(0), GroupKey::Rigid(3), GroupKey::Loose] {
            assert!(carries_out_and_back(key, sideways), "{key:?}");
            assert!(carries_out_and_back(key, upward), "{key:?}");
            for step in steps {
                assert!(carries_out_and_back(key, step), "{key:?}");
            }
        }
        assert!(!carries_out_and_back(GroupKey::Floating, sideways));
        assert!(carries_out_and_back(GroupKey::Floating, upward));
        for step in steps {
            assert!(!carries_out_and_back(GroupKey::Floating, step));
        }
    }

    /// The surface gives the way the view was pushed: focus right at the last column pulls the
    /// strip left, the next workspace at the bottom pulls the row up.
    #[test]
    fn an_edge_bounce_moves_the_content_the_way_it_would_have_gone() {
        let o = EDGE_BOUNCE_OVERSHOOT;
        assert_eq!(
            edge_bounce_overshoot(Direction::Right),
            OverlayVector::new(-o, 0.0)
        );
        assert_eq!(
            edge_bounce_overshoot(Direction::Left),
            OverlayVector::new(o, 0.0)
        );
        assert_eq!(
            edge_bounce_overshoot(Direction::Down),
            OverlayVector::new(0.0, -o)
        );
        assert_eq!(edge_bounce_overshoot(Direction::Up), OverlayVector::new(0.0, o));
        assert!(o < 100.0, "a nudge, not a scroll");
    }

    #[test]
    fn a_tile_the_plan_still_names_is_kept() {
        let tiles = [(WindowId::new(1, 1), 7u8), (WindowId::new(1, 2), 7)];
        let keep = [WindowId::new(1, 1), WindowId::new(1, 2)];
        let (stale, empty) = stale_overlay_layers(&tiles, &keep, &[7]);
        assert!(stale.is_empty());
        assert!(empty.is_empty(), "the container still has both tiles");
    }

    #[test]
    fn a_tile_the_plan_dropped_is_stale() {
        let tiles = [(WindowId::new(1, 1), 7u8), (WindowId::new(1, 2), 7)];
        let keep = [WindowId::new(1, 1)];
        let (stale, empty) = stale_overlay_layers(&tiles, &keep, &[7]);
        assert_eq!(stale, [WindowId::new(1, 2)]);
        assert!(empty.is_empty(), "window 1 is still in the container");
    }

    /// The reason the two stages are ordered. A container is judged by the tiles that REMAIN, so
    /// removing the tiles has to come first. Judging against the original set keeps a container whose
    /// only tile just left, and an empty container is a layer the compositor keeps compositing.
    #[test]
    fn a_container_whose_last_tile_left_is_empty() {
        let tiles = [(WindowId::new(1, 1), 7u8), (WindowId::new(1, 2), 8)];
        let keep = [WindowId::new(1, 1)];
        let (stale, empty) = stale_overlay_layers(&tiles, &keep, &[7, 8]);
        assert_eq!(stale, [WindowId::new(1, 2)]);
        assert_eq!(empty, [8], "container 8 lost its only tile");
    }

    #[test]
    fn a_container_no_tile_ever_claimed_is_empty() {
        let tiles = [(WindowId::new(1, 1), 7u8)];
        let keep = [WindowId::new(1, 1)];
        let (_, empty) = stale_overlay_layers(&tiles, &keep, &[7, 9]);
        assert_eq!(empty, [9]);
    }

    /// A rigid member rides its container, so it is not moving on its own and `rel` is both its
    /// start and its end. Reading `to` for a reparented one would install the layer at its
    /// destination and animate nowhere.
    #[test]
    fn a_rigid_member_starts_where_it_sits() {
        let rel: GroupRect = rect(10.0, 20.0, 100.0, 50.0);
        let member = Member::Rigid { key: GroupKey::STILL, rel };
        assert_eq!(member.start_frame(), rel);
    }

    #[test]
    fn every_animated_member_starts_at_its_from() {
        let from: GroupRect = rect(1.0, 2.0, 3.0, 4.0);
        let to = rect(90.0, 90.0, 3.0, 4.0);
        for member in [
            Member::Changing { from, to },
            Member::Entrance { from, to },
            Member::Floating { from, to },
        ] {
            assert_eq!(member.start_frame(), from, "{member:?}");
            assert_ne!(member.start_frame(), to);
        }
    }

    /// An entering window grows from zero width at its own left edge, so it unfurls rightward from
    /// where it will sit rather than sliding in from somewhere else.
    #[test]
    fn an_entrance_grows_from_no_width_at_its_own_left_edge() {
        let to: GroupRect = rect(300., 50., 400., 600.);
        let from = entrance_from(to);
        assert_eq!(from.origin, to.origin);
        assert_eq!(from.size.width, 0.0);
        assert_eq!(from.size.height, to.size.height, "full height from the start");
    }

    /// A pass merging into a flight in progress (`merge_plans`). The 3:27:20 tear is the case it
    /// exists for. See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
    mod merges {
        use rini_geometry::SameAs;

        use super::*;
        use crate::animation::domain::motion::testing::{
            Gen, OnSurface, RUNS, VIEWPORT, random_requests,
        };

        /// An external display at a non-zero origin, so a retarget that forgot the conversion shows.
        const EXTERNAL: ScreenRect =
            Rect::new(Point::new(1728.0, -300.0), Size::new(3008.0, 1692.0));

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
        }

        fn column(i: f64) -> ScreenRect { rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0) }

        fn flight_of(requests: &[Resolved]) -> FlightPlan {
            FlightPlan::from(reflow_plan(requests, DISPLAY))
        }

        fn key_of(plan: &FlightPlan, window: WindowId) -> Option<GroupKey> {
            match plan.member(window)? {
                Member::Rigid { key, .. } => Some(key),
                Member::Changing { .. } | Member::Entrance { .. } => Some(GroupKey::Loose),
                Member::Floating { .. } => Some(GroupKey::Floating),
            }
        }

        /// Model positions as the presented ones: a flight that has not started drawing.
        fn at_model(plan: &FlightPlan) -> HashMap<GroupKey, OverlayPoint> { plan.positions.clone() }

        /// Halfway between install (`p - travel`) and destination, per container.
        fn midway(plan: &FlightPlan) -> HashMap<GroupKey, OverlayPoint> {
            plan.positions
                .iter()
                .map(|(key, p)| {
                    let travel = match key {
                        GroupKey::Floating => plan.floating_travel,
                        GroupKey::Loose => OverlayVector::zero(),
                        key => plan
                            .groups
                            .iter()
                            .find(|g| g.key == *key)
                            .map(|g| g.travel)
                            .unwrap_or(OverlayVector::zero()),
                    };
                    (
                        *key,
                        OverlayPoint::new(p.x - travel.x / 2.0, p.y - travel.y / 2.0),
                    )
                })
                .collect()
        }

        #[test]
        fn a_pan_merging_into_an_open_shifts_every_group_and_reparents_nothing() {
            let (a, b, c) = (column(0.0), column(1.0), column(2.0));
            let slot = held(rect(867.0, 32.0, 859.0, 1081.0));
            let push = ScreenVector::new(859.0, 0.0);
            let mut current = flight_of(&[
                Resolved::tiled(wid(1), a, a),
                Resolved::tiled(wid(2), b, shifted(b, push)),
                Resolved::tiled(wid(3), c, shifted(c, push)),
            ]);
            current.entrances.push(OwnPath {
                window: wid(9),
                from: entrance_from(slot),
                to: slot,
            });
            let before = current.clone();

            let d = OverlayVector::new(-574.0, 0.0);
            let pan = surface_plan(
                &[
                    OnSurface {
                        window: wid(1),
                        frame: on_surface(a),
                        pinned: false,
                        floating: false,
                        from: None,
                    },
                    OnSurface {
                        window: wid(2),
                        frame: on_surface(shifted(b, push)),
                        pinned: false,
                        floating: false,
                        from: None,
                    },
                ],
                SurfacePoint::new(-574.0, 0.0),
                SurfacePoint::new(0.0, 0.0),
            );
            let (merged, delta) = merge_plans(
                &current,
                &pan,
                Some(d),
                &at_model(&current).into(),
                None,
                VIEWPORT,
            );

            for group in before.groups.iter().filter(|g| !g.members.is_empty()) {
                let p = merged.position_of(group.key);
                assert_eq!(
                    p,
                    OverlayPoint::new(
                        before.position_of(group.key).x + d.x,
                        before.position_of(group.key).y + d.y
                    )
                );
                assert!(
                    delta.retargeted_groups.contains(&(group.key, p)),
                    "{:?} retargeted",
                    group.key
                );
                let after = merged.groups.iter().find(|g| g.key == group.key).unwrap();
                assert_eq!(after.members, group.members, "membership and rel untouched");
            }
            assert!(delta.reparented.is_empty());
            assert!(delta.new_groups.is_empty(), "the pan names known windows only");
            assert_eq!(merged.entrances[0].to, shifted(slot, d.respace()));
            assert_eq!(delta.retargeted_tiles, vec![(
                wid(9),
                shifted(slot, d.respace())
            )]);
            assert_eq!(
                merged.destination(wid(3)),
                Some(shifted(on(c), OverlayVector::new(859.0 + d.x, 0.0))),
                "a member the pan did not compose rides its group"
            );
        }

        #[test]
        fn a_redundant_pass_is_an_empty_delta() {
            let (a, b) = (column(0.0), column(1.0));
            let requests = [
                Resolved::tiled(wid(1), a, a),
                Resolved::tiled(wid(2), b, shifted(b, ScreenVector::new(-300.0, 0.0))),
            ];
            let current = flight_of(&requests);
            let again = reflow_plan(&requests, DISPLAY);
            let (merged, delta) =
                merge_plans(&current, &again, None, &midway(&current).into(), None, VIEWPORT);
            assert!(delta.is_empty(), "{delta:?}");
            assert_eq!(merged, current);
        }

        #[test]
        fn a_pass_moving_one_member_elsewhere_reparents_it_and_keeps_the_other() {
            let (a, b) = (column(0.0), column(1.0));
            let v = ScreenVector::new(-300.0, 0.0);
            let current = flight_of(&[
                Resolved::tiled(wid(1), a, shifted(a, v)),
                Resolved::tiled(wid(2), b, shifted(b, v)),
            ]);
            let group = current.groups[1].key;
            let presented = midway(&current);
            let elsewhere = shifted(b, ScreenVector::new(500.0, 0.0));
            let pass = reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, shifted(a, v)),
                    Resolved::tiled(wid(2), b, elsewhere),
                ],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &presented.clone().into(), None, VIEWPORT);
            assert_eq!(delta.reparented.len(), 1);
            let (window, from, to) = delta.reparented[0];
            assert_eq!((window, from), (wid(2), group));
            assert_ne!(to, group);
            assert_eq!(
                key_of(&merged, wid(1)),
                Some(group),
                "the other member keeps the container"
            );
            assert!(
                delta.retargeted_groups.is_empty(),
                "the winning cluster confirmed the destination"
            );
            assert!(
                merged.destination(wid(2)).unwrap().same_as(on(elsewhere)),
                "{:?}",
                merged.destination(wid(2))
            );
            assert!(merged.destination(wid(1)).unwrap().same_as(on(shifted(a, v))));
            assert_eq!(delta.new_groups.len(), 1, "no group had that remaining travel");
            assert_eq!(
                delta.new_groups[0].install, presented[&group],
                "installs where the old container is drawn"
            );
        }

        /// The 1:07 zig-zag: members parked by a later pass ride their moving container out; a still
        /// container's member leaves on its own.
        #[test]
        fn a_member_parked_by_a_later_pass_rides_its_moving_container_out() {
            let (a, b, c) = (column(0.0), column(1.0), column(2.0));
            let up = ScreenVector::new(0.0, -DISPLAY.size.height);
            let current = flight_of(&[
                Resolved::tiled(wid(1), a, shifted(a, up)),
                Resolved::tiled(wid(2), b, shifted(b, up)),
                Resolved::tiled(wid(3), c, shifted(c, up)),
            ]);
            let group = key_of(&current, wid(1)).unwrap();
            let past_right =
                rect(DISPLAY.size.width + 1.0, b.origin.y, b.size.width, b.size.height);
            let past_left = rect(-c.size.width - 1.0, c.origin.y, c.size.width, c.size.height);
            let pass = reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, shifted(a, up)),
                    Resolved::tiled(wid(2), b, past_right),
                    Resolved::tiled(wid(3), c, past_left),
                ],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, VIEWPORT);
            assert!(delta.is_empty(), "{delta:?}");
            assert_eq!(merged, current);
            for w in [wid(1), wid(2), wid(3)] {
                assert_eq!(key_of(&merged, w), Some(group));
            }

            // A still container lends no motion: the member leaves on its own vector.
            let still = flight_of(&[Resolved::tiled(wid(1), a, a), Resolved::tiled(wid(2), b, b)]);
            let pass = reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, a),
                    Resolved::tiled(wid(2), b, past_right),
                ],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&still, &pass, None, &midway(&still).into(), None, VIEWPORT);
            assert_eq!(delta.reparented.len(), 1, "{delta:?}");
            assert!(merged.destination(wid(2)).unwrap().same_as(on(past_right)));
        }

        #[test]
        fn a_rigid_member_turning_into_a_resize_goes_loose() {
            let (a, b) = (column(0.0), column(1.0));
            let v = ScreenVector::new(-300.0, 0.0);
            let current = flight_of(&[
                Resolved::tiled(wid(1), a, shifted(a, v)),
                Resolved::tiled(wid(2), b, shifted(b, v)),
            ]);
            let group = current.groups[1].key;
            let presented = midway(&current);
            let grown = rect(b.origin.x + v.x, b.origin.y, b.size.width + 400.0, b.size.height);
            let pass = reflow_plan(&[Resolved::tiled(wid(2), shifted(b, v), grown)], DISPLAY);
            let (merged, delta) =
                merge_plans(&current, &pass, None, &presented.clone().into(), None, VIEWPORT);
            assert_eq!(delta.reparented, vec![(wid(2), group, GroupKey::Loose)]);
            assert_eq!(delta.retargeted_tiles, vec![(wid(2), held(grown))]);
            let Some(Member::Changing { from, to }) = merged.member(wid(2)) else {
                panic!("loose")
            };
            assert_eq!(
                from,
                installed(overlay_of(held(b), presented[&group])),
                "leaves at the presented frame"
            );
            assert_eq!(to, held(grown));
            assert_eq!(key_of(&merged, wid(1)), Some(group));
        }

        /// A newcomer travelling as a group does from where the last pass sent it rides that group,
        /// placed in it where that pass left it, wherever the group's leg has it: beside the window
        /// it sits beside in the layout. Against where the leg has it, it matched no group mid-leg
        /// and started out of place by the travel left.
        #[test]
        fn a_join_travelling_as_a_group_does_rides_it_where_the_last_pass_left_it() {
            let (a, b) = (column(0.0), column(1.0));
            let v = ScreenVector::new(-400.0, 0.0);
            let current = flight_of(&[Resolved::tiled(wid(1), a, shifted(a, v))]);
            let group = key_of(&current, wid(1)).unwrap();
            let further = ScreenVector::new(-300.0, 0.0);
            let (one, beside) = (shifted(a, v), shifted(b, v));
            let pass = reflow_plan(
                &[
                    Resolved::tiled(wid(1), one, shifted(one, further)),
                    Resolved::tiled(wid(2), beside, shifted(beside, further)),
                ],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, VIEWPORT);
            assert_eq!(delta.joined_tiles, vec![(wid(2), group)]);
            assert!(delta.new_groups.is_empty());
            let rel = |window| match merged.member(window) {
                Some(Member::Rigid { key, rel }) if key == group => rel,
                other => panic!("{window:?} is {other:?}"),
            };
            assert_eq!(
                rel(wid(2)),
                group_relative(on(beside), current.position_of(group))
            );
            assert_eq!(rel(wid(2)).origin.x - rel(wid(1)).origin.x, 863.0, "beside 1");
            assert!(merged.destination(wid(2)).unwrap().same_as(on(shifted(beside, further))));
        }

        /// Of several groups travelling as a newcomer does, it rides the one holding the window
        /// nearest it as the last pass left them.
        #[test]
        fn a_newcomer_rides_the_group_of_the_window_nearest_it() {
            let (a, b) = (column(0.0), column(1.0));
            let current = flight_of(&[
                Resolved::tiled(wid(1), a, shifted(a, ScreenVector::new(-400.0, 0.0))),
                Resolved::tiled(wid(2), b, shifted(b, ScreenVector::new(200.0, 0.0))),
            ]);
            let second = key_of(&current, wid(2)).unwrap();
            assert_ne!(key_of(&current, wid(1)), Some(second));
            let beside = shifted(
                current.destination(wid(2)).unwrap(),
                OverlayVector::new(863.0, 0.0),
            );
            let pass = reflow_plan(
                &[Resolved::tiled(wid(3), screen(beside), screen(beside))],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, VIEWPORT);
            assert_eq!(delta.joined_tiles, vec![(wid(3), second)]);
            assert!(merged.destination(wid(3)).unwrap().same_as(beside));
        }

        /// A floating window a switch brings is placed in the floating container where the last pass
        /// left that container, as the floating windows already in it are.
        #[test]
        fn a_floating_newcomer_starts_where_the_last_pass_left_the_floating_container() {
            let up = OverlayVector::new(0.0, -DISPLAY.size.height);
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let mut current = flight_of(&[Resolved::floating(wid(1), settings, settings)]);
            current.floating_travel = up;
            current.positions.insert(GroupKey::Floating, up.to_point());
            let panel = rect(1300.0, 300.0, 400.0, 300.0);
            let mut pass = reflow_plan(&[Resolved::floating(wid(2), panel, panel)], DISPLAY);
            pass.floating_travel = up;
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, VIEWPORT);
            assert_eq!(delta.joined_tiles, vec![(wid(2), GroupKey::Floating)]);
            let Some(Member::Floating { from, .. }) = merged.member(wid(2)) else {
                panic!("floating")
            };
            assert_eq!(
                from,
                group_relative(to_overlay_space(panel, DISPLAY), up.to_point())
            );
        }

        #[test]
        fn a_join_with_a_new_vector_opens_a_group() {
            let (a, b) = (column(0.0), column(1.0));
            let away = ScreenVector::new(-400.0, 0.0);
            let current = flight_of(&[Resolved::tiled(wid(1), a, shifted(a, away))]);
            let pass = reflow_plan(
                &[Resolved::tiled(
                    wid(2),
                    b,
                    shifted(b, ScreenVector::new(120.0, 0.0)),
                )],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, VIEWPORT);
            assert!(delta.joined_tiles.is_empty());
            assert_eq!(delta.new_groups.len(), 1);
            let NewGroup { key, install, .. } = delta.new_groups[0];
            assert_eq!(install, OverlayPoint::new(0.0, 0.0));
            assert_eq!(key_of(&merged, wid(2)), Some(key));
            assert_eq!(merged.position_of(key), OverlayPoint::new(120.0, 0.0));
            assert_eq!(merged.next_key, current.next_key + 1);
        }

        /// The 3:27:20 case: an open with 22 survivors and one entrance, then a 574pt pan 56ms later.
        #[test]
        fn the_3_27_20_open_then_pan_ends_everything_at_the_pans_frames() {
            let col = |i: f64| {
                rect(
                    EXTERNAL.origin.x + 4.0 + i * 863.0,
                    EXTERNAL.origin.y + 32.0,
                    859.0,
                    1081.0,
                )
            };
            let push = ScreenVector::new(859.0, 0.0);
            // The open: columns from index 1 shift right by the newcomer's width.
            let requests: Vec<Resolved> = (0..22)
                .map(|i| {
                    let f = col(i as f64);
                    let to = if i == 0 { f } else { shifted(f, push) };
                    Resolved::tiled(wid(i + 1), f, to)
                })
                .collect();
            let slot = col(1.0);
            let mut current = FlightPlan::from(reflow_plan(&requests, EXTERNAL));
            let slot_o = installed(to_overlay_space(slot, EXTERNAL));
            current.entrances.push(OwnPath {
                window: wid(100),
                from: entrance_from(slot_o),
                to: slot_o,
            });

            let d = OverlayVector::new(-574.0, 0.0);
            let at_rest = viewport(SurfacePoint::origin()).inverse();
            let windows: Vec<OnSurface> = requests
                .iter()
                .map(|r| OnSurface {
                    window: r.window,
                    frame: at_rest.transform_rect(&to_overlay_space(r.end, EXTERNAL)),
                    pinned: false,
                    floating: false,
                    from: None,
                })
                .collect();
            let pan = surface_plan(
                &windows,
                SurfacePoint::new(-574.0, 0.0),
                SurfacePoint::new(0.0, 0.0),
            );
            let (merged, delta) =
                merge_plans(&current, &pan, Some(d), &midway(&current).into(), None, VIEWPORT);

            for Resolved { window: w, end: to, .. } in &requests {
                let expected = shifted(to_overlay_space(*to, EXTERNAL), d);
                assert!(
                    merged.destination(*w).unwrap().same_as(expected),
                    "{w:?}: {:?} vs {expected:?}",
                    merged.destination(*w)
                );
            }
            assert_eq!(merged.entrances[0].to, shifted(slot_o, d.respace()));
            assert!(delta.reparented.is_empty());
            assert_eq!(
                delta.retargeted_groups.len(),
                2,
                "the still group and the pushed group"
            );
            assert!(delta.moves_anything());
        }

        #[test]
        fn a_member_the_pass_names_by_frame_only_rides_its_group() {
            let (a, b, c) = (column(0.0), column(1.0), column(2.0));
            let v = ScreenVector::new(-300.0, 0.0);
            let current = flight_of(&[
                Resolved::tiled(wid(1), a, shifted(a, v)),
                Resolved::tiled(wid(2), b, shifted(b, v)),
                Resolved::tiled(wid(3), c, shifted(c, v)),
            ]);
            let group = current.groups[1].key;
            // The pass composes 1 and 3 with a further 100pt; 2 has no picture.
            let further = ScreenVector::new(-400.0, 0.0);
            let pass = reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, shifted(a, further)),
                    Resolved::tiled(wid(3), c, shifted(c, further)),
                ],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, VIEWPORT);
            assert_eq!(delta.retargeted_groups, vec![(
                group,
                OverlayPoint::new(-400.0, 0.0)
            )]);
            assert!(delta.reparented.is_empty() && delta.joined_tiles.is_empty());
            assert!(
                merged.destination(wid(2)).unwrap().same_as(on(shifted(b, further))),
                "rides along"
            );
        }

        /// Every named window ends within 2pt of its destination in the key the delta says; no
        /// window is in two groups; a pan changes no membership.
        #[test]
        fn merges_honour_every_destination_and_keep_the_partition() {
            let mut rng = Gen(149);
            for run in 0..RUNS {
                let mut plan = flight_of(&random_requests(&mut rng));
                let steps = 1 + rng.below(4) as usize;
                for step in 0..steps {
                    let tag = format!("seed 149 run {run} step {step}");
                    let presented = if rng.coin() {
                        at_model(&plan)
                    } else {
                        midway(&plan)
                    };
                    let windows = plan.windows();
                    let before = plan.clone();
                    let mut pan: Option<OverlayVector> = None;
                    let incoming: ReflowPlan = match rng.below(4) {
                        // A pan.
                        0 => {
                            let mut d = OverlayVector::new(rng.pt(-9.0, 9.0) * 100.0, 0.0);
                            if d == OverlayVector::zero() {
                                d.x = 100.0;
                            }
                            pan = Some(d);
                            let mut p = ReflowPlan::empty();
                            for w in &windows {
                                let Some(from) = plan.destination(*w) else { continue };
                                if let Some(Member::Rigid { .. }) = plan.member(*w) {
                                    p.place(*w, installed(from), d);
                                }
                            }
                            p
                        }
                        // A layout pass over a subset with a fresh vector or two.
                        1 | 2 => {
                            let v1 = ScreenVector::new(rng.pt(-9.0, 9.0) * 50.0, 0.0);
                            let v2 = ScreenVector::new(rng.pt(-9.0, 9.0) * 50.0, 0.0);
                            let mut reqs: Vec<Resolved> = Vec::new();
                            for w in &windows {
                                if rng.coin() {
                                    continue;
                                }
                                let Some(Member::Rigid { key, rel }) = plan.member(*w) else {
                                    continue;
                                };
                                let from = screen(overlay_of(rel, presented[&key]));
                                let v = if rng.coin() { v1 } else { v2 };
                                reqs.push(Resolved::tiled(*w, from, shifted(from, v)));
                            }
                            // Sometimes a newcomer.
                            if rng.coin() {
                                let from = rng.on_screen();
                                reqs.push(Resolved::tiled(
                                    wid(500 + step as u32),
                                    from,
                                    shifted(from, v1),
                                ));
                            }
                            reflow_plan(&reqs, DISPLAY)
                        }
                        // A resize of a random rigid member.
                        _ => {
                            let rigid: Vec<WindowId> = windows
                                .iter()
                                .copied()
                                .filter(|w| matches!(plan.member(*w), Some(Member::Rigid { .. })))
                                .collect();
                            let mut reqs: Vec<Resolved> = Vec::new();
                            if let Some(w) = rigid.first() {
                                let Some(Member::Rigid { key, rel }) = plan.member(*w) else {
                                    unreachable!()
                                };
                                let from = screen(overlay_of(rel, presented[&key]));
                                let to = rect(
                                    from.origin.x,
                                    from.origin.y,
                                    from.size.width + 300.0,
                                    from.size.height,
                                );
                                reqs.push(Resolved::tiled(*w, from, to));
                            }
                            reflow_plan(&reqs, DISPLAY)
                        }
                    };
                    let (merged, delta) = merge_plans(
                        &plan,
                        &incoming,
                        pan,
                        &presented.clone().into(),
                        None,
                        VIEWPORT,
                    );

                    // Partition: no window in two groups, none lost.
                    let mut named = merged.windows();
                    let count = named.len();
                    named.sort();
                    named.dedup();
                    assert_eq!(named.len(), count, "{tag}: a window in two places");
                    for w in &windows {
                        assert!(merged.member(*w).is_some(), "{tag}: {w:?} dropped");
                    }

                    // Every named window ends within 2pt of its destination, in the key the delta says.
                    for w in incoming.windows() {
                        let want = match incoming.member(w).unwrap() {
                            Member::Rigid { key, rel } => {
                                let group = incoming.groups.iter().find(|g| g.key == key).unwrap();
                                overlay_of(rel, group.travel.to_point())
                            }
                            Member::Changing { to, .. }
                            | Member::Entrance { to, .. }
                            | Member::Floating { to, .. } => on_overlay(to),
                        };
                        let got = merged
                            .destination(w)
                            .unwrap_or_else(|| panic!("{tag}: {w:?} unnamed after merge"));
                        // The one exception: a member sent off the viewport rides its moving container (`rides_out`).
                        let rode_out = pan.is_none()
                            && rini_geometry::is_off_screen(VIEWPORT, want)
                            && matches!(before.member(w), Some(Member::Rigid { key, .. })
                                if before.groups.iter().any(|g| g.key == key && !g.is_still()));
                        if rode_out {
                            assert_eq!(
                                key_of(&merged, w),
                                key_of(&before, w),
                                "{tag}: rides its container"
                            );
                            continue;
                        }
                        assert!(
                            (got.origin.x - want.origin.x).abs() <= GROUP_TOLERANCE + 0.01
                                && (got.origin.y - want.origin.y).abs() <= GROUP_TOLERANCE + 0.01,
                            "{tag}: {w:?} ends at {got:?}, pass wants {want:?}"
                        );
                        if let Some((_, _, to_key)) =
                            delta.reparented.iter().find(|(x, _, _)| *x == w)
                        {
                            assert_eq!(key_of(&merged, w), Some(*to_key), "{tag}");
                        }
                        if let Some((_, key)) = delta.joined_tiles.iter().find(|(x, _)| *x == w) {
                            assert_eq!(key_of(&merged, w), Some(*key), "{tag}");
                        }
                    }

                    // A pan: no membership change, every occupied position shifted by d.
                    if let Some(d) = pan {
                        assert!(delta.reparented.is_empty(), "{tag}");
                        for g in before.groups.iter().filter(|g| !g.members.is_empty()) {
                            let after = merged.groups.iter().find(|x| x.key == g.key).unwrap();
                            assert_eq!(after.members, g.members, "{tag}");
                            let (p0, p1) = (before.position_of(g.key), merged.position_of(g.key));
                            assert_eq!((p1.x - p0.x, p1.y - p0.y), (d.x, d.y), "{tag}");
                        }
                        assert_eq!(
                            delta.retargeted_groups.is_empty(),
                            d == OverlayVector::zero()
                                || before.groups.iter().all(|g| g.members.is_empty()),
                            "{tag}"
                        );
                    }
                    plan = merged;
                }
            }
        }

        #[test]
        fn a_pass_with_the_same_destinations_is_an_empty_delta() {
            let mut rng = Gen(151);
            for run in 0..RUNS {
                let requests = random_requests(&mut rng);
                let current = flight_of(&requests);
                let presented = if rng.coin() {
                    at_model(&current)
                } else {
                    midway(&current)
                };
                // The same destinations, expressed from the presented frames.
                let mut reqs: Vec<Resolved> = Vec::new();
                for &Resolved { window: w, floating, .. } in &requests {
                    let (from, to) = match current.member(w).unwrap() {
                        Member::Rigid { key, rel } => (
                            overlay_of(rel, presented[&key]),
                            overlay_of(rel, current.position_of(key)),
                        ),
                        Member::Changing { from, to } | Member::Entrance { from, to } => {
                            (on_overlay(from), on_overlay(to))
                        }
                        Member::Floating { from, to } => (
                            overlay_of(from, presented[&GroupKey::Floating]),
                            overlay_of(to, current.position_of(GroupKey::Floating)),
                        ),
                    };
                    reqs.push(Resolved {
                        window: w,
                        start: screen(from),
                        end: screen(to),
                        floating,
                    });
                }
                let same = reflow_plan(&reqs, DISPLAY);
                let (merged, delta) =
                    merge_plans(&current, &same, None, &presented.clone().into(), None, VIEWPORT);
                assert!(delta.is_empty(), "seed 151 run {run}: {delta:?}");
                assert_eq!(merged, current, "seed 151 run {run}");
            }
        }

        /// Where a container is drawn includes the nudge riding it, and a member leaving it mid-nudge
        /// leaves from there, carrying on its container's motion: continuous, and still landing on
        /// its destination.
        #[test]
        fn a_member_leaving_a_nudged_container_leaves_from_where_it_is_drawn() {
            let (a, b): (ScreenRect, ScreenRect) =
                (rect(4.0, 32.0, 860.0, 1081.0), rect(868.0, 32.0, 860.0, 1081.0));
            let v = ScreenVector::new(-300.0, 0.0);
            let current = FlightPlan::from(reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, rect(a.origin.x + v.x, 32.0, 860.0, 1081.0)),
                    Resolved::tiled(wid(2), b, rect(b.origin.x + v.x, 32.0, 860.0, 1081.0)),
                ],
                DISPLAY,
            ));
            let group = key_of(&current, wid(2)).unwrap();
            let nudge = OverlayVector::new(576.0, 0.0);
            let along = OverlayPoint::new(v.x / 2.0, 0.0);
            let drawn = OverlayPoint::new(along.x + nudge.x, 0.0);
            let presented = Presented {
                drawn: [(group, drawn)].into_iter().collect(),
                along: [(group, along)].into_iter().collect(),
            };
            let rel = |plan: &FlightPlan, window| match plan.member(window) {
                Some(Member::Rigid { rel, .. }) => rel,
                other => panic!("{other:?}"),
            };
            let before = overlay_of(rel(&current, wid(2)), drawn);

            let elsewhere = rect(1500.0, 32.0, 860.0, 1081.0);
            let pass = reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, rect(a.origin.x + v.x, 32.0, 860.0, 1081.0)),
                    Resolved::tiled(wid(2), b, elsewhere),
                ],
                DISPLAY,
            );
            let (merged, delta) = merge_plans(&current, &pass, None, &presented, None, VIEWPORT);
            let NewGroup { install, leaving, .. } = delta.new_groups[0];
            assert_eq!(leaving, Some(group));
            let key = key_of(&merged, wid(2)).unwrap();
            assert_ne!(key, group);
            assert_eq!(overlay_of(rel(&merged, wid(2)), install), before, "no jump");
            assert!(merged.destination(wid(2)).unwrap().same_as(on(elsewhere)));
        }
    }

    /// A pass as rigid pieces: `reflow_plan`, `surface_plan` and what `animation_targets` names. See "Layout changes and strip movements" in
    /// `src/transitions/docs/coordination.md`.
    mod pieces {
        use rini_geometry::SameAs;

        use super::*;
        use crate::animation::domain::motion::surface::{pan_travel, surface_travel};
        use crate::animation::domain::motion::testing::{Gen, OnSurface, RUNS, random_requests};

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
        }

        fn shifted<U>(frame: Rect<U>, dx: f64, dy: f64) -> Rect<U> {
            super::shifted(frame, Vector::new(dx, dy))
        }

        fn column<U>(i: f64) -> Rect<U> { rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0) }

        fn moving(plan: &ReflowPlan) -> Vec<&RigidGroup> {
            plan.groups
                .iter()
                .filter(|g| !g.members.is_empty() && g.key != GroupKey::STILL)
                .collect()
        }

        fn members(group: &RigidGroup) -> Vec<WindowId> {
            group.members.iter().map(|m| m.window).collect()
        }

        fn tiled(idx: u32, frame: SurfaceRect) -> OnSurface {
            OnSurface {
                window: wid(idx),
                frame,
                pinned: false,
                floating: false,
                from: None,
            }
        }

        fn floating(idx: u32, frame: SurfaceRect) -> OnSurface {
            OnSurface {
                floating: true,
                ..tiled(idx, frame)
            }
        }

        #[test]
        fn two_columns_shifting_by_the_same_vector_are_one_group() {
            let a: ScreenRect = column(0.0);
            let b = column(1.0);
            let plan = reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, shifted(a, -859.0, 0.0)),
                    Resolved::tiled(wid(2), b, shifted(b, -859.0, 0.0)),
                ],
                DISPLAY,
            );
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].travel, OverlayVector::new(-859.0, 0.0));
            assert_eq!(members(groups[0]), vec![wid(1), wid(2)]);
            assert_eq!(
                groups[0].members[0].rel,
                held(a),
                "a container installs at (0,0), so rel is from"
            );
            assert!(plan.groups[0].members.is_empty(), "nothing stands still");
            assert!(
                plan.changing.is_empty() && plan.floating.is_empty() && plan.entrances.is_empty()
            );
        }

        #[test]
        fn a_column_two_points_off_shares_the_group_and_three_points_off_opens_one() {
            let a: ScreenRect = column(0.0);
            let b = column(1.0);
            let same = reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, shifted(a, 300.0, 0.0)),
                    Resolved::tiled(wid(2), b, shifted(b, 302.0, 0.0)),
                ],
                DISPLAY,
            );
            assert_eq!(moving(&same).len(), 1, "2pt apart: one group");
            assert_eq!(members(moving(&same)[0]), vec![wid(1), wid(2)]);

            let split = reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, shifted(a, 300.0, 0.0)),
                    Resolved::tiled(wid(2), b, shifted(b, 303.0, 0.0)),
                ],
                DISPLAY,
            );
            let groups = moving(&split);
            assert_eq!(groups.len(), 2, "3pt apart: two groups");
            assert_eq!(members(groups[0]), vec![wid(1)]);
            assert_eq!(members(groups[1]), vec![wid(2)]);
            assert_eq!(groups[1].travel, OverlayVector::new(303.0, 0.0));
            assert_eq!(groups[0].key, GroupKey::Rigid(1));
            assert_eq!(groups[1].key, GroupKey::Rigid(2));
        }

        #[test]
        fn a_resizing_column_is_changing_and_in_no_group() {
            let a: ScreenRect = column(0.0);
            let grown = rect(a.origin.x, a.origin.y, a.size.width + 400.0, a.size.height);
            let plan = reflow_plan(&[Resolved::tiled(wid(1), a, grown)], DISPLAY);
            assert_eq!(plan.changing, vec![OwnPath {
                window: wid(1),
                from: held(a),
                to: held(grown)
            }]);
            assert!(plan.group_of(wid(1)).is_none());
            assert!(moving(&plan).is_empty());
            assert_eq!(
                plan.member(wid(1)),
                Some(Member::Changing { from: held(a), to: held(grown) })
            );
        }

        #[test]
        fn a_floating_window_is_floating_and_never_grouped() {
            let a: ScreenRect = column(0.0);
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let plan = reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, shifted(a, -100.0, 0.0)),
                    Resolved::floating(wid(2), settings, shifted(settings, -100.0, 0.0)),
                    Resolved::floating(wid(3), settings, settings),
                ],
                DISPLAY,
            );
            assert_eq!(
                members(moving(&plan)[0]),
                vec![wid(1)],
                "the same vector does not pull it in"
            );
            assert_eq!(plan.floating, vec![
                OwnPath {
                    window: wid(2),
                    from: held(settings),
                    to: held(shifted(settings, -100.0, 0.0))
                },
                OwnPath {
                    window: wid(3),
                    from: held(settings),
                    to: held(settings)
                }
            ]);
            assert!(plan.group_of(wid(2)).is_none() && plan.group_of(wid(3)).is_none());
            assert_eq!(
                plan.floating_travel,
                OverlayVector::zero(),
                "a layout pass never moves the container"
            );
        }

        #[test]
        fn the_still_window_is_in_the_still_group() {
            let a: ScreenRect = column(0.0);
            let b = column(1.0);
            let plan = reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, a),
                    Resolved::tiled(wid(2), b, shifted(b, 40.0, 0.0)),
                ],
                DISPLAY,
            );
            assert_eq!(plan.groups[0].key, GroupKey::STILL);
            assert_eq!(plan.groups[0].travel, OverlayVector::zero());
            assert_eq!(members(&plan.groups[0]), vec![wid(1)]);
            assert_eq!(
                plan.member(wid(1)),
                Some(Member::Rigid {
                    key: GroupKey::STILL,
                    rel: held(a)
                })
            );
            assert_eq!(members(moving(&plan)[0]), vec![wid(2)]);
        }

        /// The 3:27:20 pan: 22 survivors scrolled 574pt. One group, 22 members, one travel.
        #[test]
        fn the_3_27_20_pan_is_one_group_of_twenty_two() {
            let windows: Vec<OnSurface> = (0..22).map(|i| tiled(i + 1, column(i as f64))).collect();
            let from_offset = SurfacePoint::new(-574.0, 0.0);
            let to_offset = SurfacePoint::new(0.0, 0.0);
            let plan = surface_plan(&windows, from_offset, to_offset);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].members.len(), 22);
            assert_eq!(groups[0].travel, OverlayVector::new(-574.0, 0.0));
            assert_eq!(groups[0].travel, pan_travel(from_offset, to_offset));
            for (window, member) in windows.iter().zip(&groups[0].members) {
                let (from, to) = surface_travel(window.frame, from_offset, to_offset, false);
                assert_eq!(member.window, window.window);
                assert_eq!(member.rel, installed(from));
                assert_eq!(
                    overlay_of(member.rel, groups[0].travel.to_point()),
                    to,
                    "rel plus travel is the destination"
                );
            }
            assert!(plan.groups[0].members.is_empty());
            assert!(plan.floating.is_empty() && plan.changing.is_empty());
            assert_eq!(plan.floating_travel, OverlayVector::zero());
        }

        #[test]
        fn pinned_windows_are_floating_with_zero_travel() {
            let settings: SurfaceRect = rect(500.0, 300.0, 700.0, 500.0);
            let windows = vec![tiled(1, column(0.0)), OnSurface {
                pinned: true,
                ..floating(2, settings)
            }];
            let plan = surface_plan(
                &windows,
                SurfacePoint::new(-574.0, 0.0),
                SurfacePoint::new(0.0, 0.0),
            );
            let standing: GroupRect = rect(500.0, 300.0, 700.0, 500.0);
            assert_eq!(plan.floating, vec![OwnPath {
                window: wid(2),
                from: standing,
                to: standing
            }]);
            assert_eq!(plan.floating_travel, OverlayVector::zero());
            assert_eq!(
                plan.member(wid(2)),
                Some(Member::Floating { from: standing, to: standing })
            );
            assert_eq!(members(moving(&plan)[0]), vec![wid(1)]);
        }

        #[test]
        fn a_switch_moves_the_floating_container_by_the_surface_travel() {
            let settings: SurfaceRect = rect(500.0, 300.0, 700.0, 500.0);
            let windows = vec![tiled(1, column(0.0)), floating(2, settings)];
            let from_offset = SurfacePoint::new(0.0, 0.0);
            let to_offset = SurfacePoint::new(0.0, 1117.0);
            let plan = surface_plan(&windows, from_offset, to_offset);
            let travel = pan_travel(from_offset, to_offset);
            assert_eq!(travel, OverlayVector::new(0.0, -1117.0));
            assert_eq!(moving(&plan)[0].travel, travel);
            assert_eq!(plan.floating_travel, travel);
            let standing: GroupRect = rect(500.0, 300.0, 700.0, 500.0);
            assert_eq!(
                plan.floating,
                vec![OwnPath {
                    window: wid(2),
                    from: standing,
                    to: standing
                }],
                "the tile itself stands in its container"
            );
            let (_, to) = surface_travel(settings, from_offset, to_offset, false);
            assert_eq!(overlay_of(standing, plan.floating_travel.to_point()), to);
        }

        #[test]
        fn group_relative_and_overlay_of_are_inverses() {
            let f: OverlayRect = rect(867.0, 32.0, 859.0, 1081.0);
            let p = OverlayPoint::new(-574.0, 12.0);
            assert_eq!(overlay_of(group_relative(f, p), p), f);
            assert_eq!(group_relative(f, p), rect(1441.0, 20.0, 859.0, 1081.0));
            assert_eq!(
                group_relative(f, OverlayPoint::origin()),
                installed(f),
                "at install rel is from"
            );
            assert_eq!(on_overlay(installed(f)), f);
            let mut rng = Gen(3);
            for _ in 0..RUNS {
                let f: OverlayRect = rng.on_screen();
                // Whole and half points, as the strip's positions are.
                let (x, y) = (rng.pt(-4000.0, 4000.0), rng.pt(-4000.0, 4000.0));
                let p = OverlayPoint::new(x / 2.0, y / 2.0);
                assert_eq!(overlay_of(group_relative(f, p), p), f);
                assert_eq!(group_relative(f, p).size, f.size.respace());
            }
        }

        #[test]
        fn a_fresh_flight_plan_positions_every_container_at_its_travel() {
            let a: ScreenRect = column(0.0);
            let plan = reflow_plan(
                &[
                    Resolved::tiled(wid(1), a, shifted(a, -100.0, 0.0)),
                    Resolved::tiled(wid(2), a, a),
                ],
                DISPLAY,
            );
            let flight = FlightPlan::from(plan.clone());
            assert_eq!(flight.groups, plan.groups);
            assert_eq!(flight.positions[&GroupKey::STILL], OverlayPoint::new(0.0, 0.0));
            assert_eq!(
                flight.positions[&GroupKey::Rigid(1)],
                OverlayPoint::new(-100.0, 0.0)
            );
            assert_eq!(flight.positions[&GroupKey::Loose], OverlayPoint::new(0.0, 0.0));
            assert_eq!(
                flight.positions[&GroupKey::Floating],
                OverlayPoint::new(0.0, 0.0)
            );
            assert_eq!(flight.next_key, 2);
            assert!(PlanDelta::default().is_empty());
        }

        #[test]
        fn a_reflow_plan_partitions_the_pass_into_rigid_groups() {
            let mut rng = Gen(131);
            for run in 0..RUNS {
                let requests = random_requests(&mut rng);
                let display = if run % 2 == 0 {
                    DISPLAY
                } else {
                    rect(1728.0, -300.0, 3008.0, 1692.0)
                };
                let plan = reflow_plan(&requests, display);
                let tag = format!("seed 131 run {run}");

                let mut named = plan.windows();
                named.sort();
                let mut asked: Vec<WindowId> = requests.iter().map(|r| r.window).collect();
                asked.sort();
                assert_eq!(named, asked, "{tag}: partition");
                assert!(plan.entrances.is_empty(), "{tag}");

                assert_eq!(plan.groups[0].key, GroupKey::STILL, "{tag}");
                assert_eq!(plan.groups[0].travel, OverlayVector::zero(), "{tag}");
                for (i, group) in plan.groups.iter().enumerate() {
                    assert_eq!(group.key, GroupKey::Rigid(i as u16), "{tag}: keys in plan order");
                    assert!(
                        i == 0 || !group.members.is_empty(),
                        "{tag}: an empty moving group"
                    );
                }

                for &Resolved { window, start, end, floating } in &requests {
                    let from = to_overlay_space(start, display);
                    let to = to_overlay_space(end, display);
                    let v = to.origin - from.origin;
                    match plan.member(window).unwrap_or_else(|| panic!("{tag}: {window:?} unnamed"))
                    {
                        Member::Rigid { key, rel } => {
                            assert!(!floating, "{tag}: a floating window grouped");
                            assert!(!is_a_resize(from.size, to.size), "{tag}: a resize grouped");
                            assert_eq!(rel, installed(from), "{tag}");
                            let group = plan.group_of(window).unwrap();
                            assert_eq!(group.key, key, "{tag}");
                            assert!(
                                same_vector(group.travel, v),
                                "{tag}: {v:?} in group at {:?}",
                                group.travel
                            );
                            if from.same_as(to) {
                                assert_eq!(
                                    key,
                                    GroupKey::STILL,
                                    "{tag}: a still window off the still group"
                                );
                            }
                        }
                        Member::Changing { from: f, to: t } => {
                            assert!(!floating && is_a_resize(from.size, to.size), "{tag}");
                            assert_eq!((f, t), (installed(from), installed(to)), "{tag}");
                        }
                        Member::Floating { from: f, to: t } => {
                            assert!(floating, "{tag}");
                            assert_eq!((f, t), (installed(from), installed(to)), "{tag}");
                        }
                        Member::Entrance { .. } => panic!("{tag}: no entrance was asked for"),
                    }
                }

                let vector_of = |window: WindowId| {
                    let r = requests.iter().find(|r| r.window == window).unwrap();
                    ScreenVector::new(
                        r.end.origin.x - r.start.origin.x,
                        r.end.origin.y - r.start.origin.y,
                    )
                };
                for (i, a) in plan.groups.iter().enumerate() {
                    for b in plan.groups.iter().skip(i + 1) {
                        for ma in &a.members {
                            for mb in &b.members {
                                let (va, vb) = (vector_of(ma.window), vector_of(mb.window));
                                assert!(
                                    (va.x - vb.x).abs() > GROUP_TOLERANCE
                                        || (va.y - vb.y).abs() > GROUP_TOLERANCE,
                                    "{tag}: {va:?} and {vb:?} in different groups"
                                );
                            }
                        }
                    }
                }
            }
        }

        /// The 3:28:10 open: the newcomer's slot at x=867, the neighbour shifted right by 859.
        #[test]
        fn an_open_beside_a_column_moves_the_neighbour_as_one_group() {
            let neighbour = rect(867.0, 32.0, 859.0, 1081.0);
            let left = column(0.0);
            let plan = reflow_plan(
                &[
                    Resolved::tiled(wid(1), left, left),
                    Resolved::tiled(wid(2), neighbour, shifted(neighbour, 859.0, 0.0)),
                ],
                DISPLAY,
            );
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![wid(2)]);
            assert_eq!(groups[0].travel, OverlayVector::new(859.0, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![wid(1)]);
            assert!(plan.changing.is_empty() && plan.entrances.is_empty());
        }

        #[test]
        fn a_preset_resize_changes_the_middle_and_shifts_the_right_neighbour_as_a_group() {
            let (left, middle, right) = (column(0.0), column(1.0), column(2.0));
            let dw = 300.0;
            let grown = rect(
                middle.origin.x,
                middle.origin.y,
                middle.size.width + dw,
                middle.size.height,
            );
            let plan = reflow_plan(
                &[
                    Resolved::tiled(wid(1), left, left),
                    Resolved::tiled(wid(2), middle, grown),
                    Resolved::tiled(wid(3), right, shifted(right, dw, 0.0)),
                ],
                DISPLAY,
            );
            assert_eq!(plan.changing, vec![OwnPath {
                window: wid(2),
                from: held(middle),
                to: held(grown)
            }]);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![wid(3)]);
            assert_eq!(groups[0].travel, OverlayVector::new(dw, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![wid(1)]);
        }

        #[test]
        fn a_close_shifts_every_survivor_as_one_group() {
            let w = 863.0;
            let survivors: Vec<Resolved> = (1..=4)
                .map(|i| {
                    let f = column(i as f64);
                    Resolved::tiled(wid(i as u32), f, shifted(f, -w, 0.0))
                })
                .collect();
            let plan = reflow_plan(&survivors, DISPLAY);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), (1..=4).map(wid).collect::<Vec<_>>());
            assert_eq!(groups[0].travel, OverlayVector::new(-w, 0.0));
            assert!(plan.groups[0].members.is_empty());
        }

        #[test]
        fn a_floating_only_pass_has_no_groups_and_one_floating_member() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let plan = reflow_plan(
                &[Resolved::floating(
                    wid(1),
                    settings,
                    shifted(settings, 40.0, 20.0),
                )],
                DISPLAY,
            );
            assert!(moving(&plan).is_empty());
            assert!(plan.groups[0].members.is_empty());
            assert_eq!(plan.floating, vec![OwnPath {
                window: wid(1),
                from: held(settings),
                to: held(shifted(settings, 40.0, 20.0)),
            }]);
            let flight = FlightPlan::from(plan);
            let targets = animation_targets(&flight);
            assert_eq!(targets.len(), 1, "the floating tile flies on its own");
        }

        /// A look composes what `animation_targets` names and nothing else.
        #[test]
        fn animation_targets_name_every_moving_piece_once_and_no_rigid_member() {
            let mut rng = Gen(137);
            for run in 0..RUNS {
                let requests = random_requests(&mut rng);
                let mut plan = reflow_plan(&requests, DISPLAY);
                // A switch now and then: the floating container itself travels.
                let switch = run % 5 == 0 && !plan.floating.is_empty();
                if switch {
                    plan.floating_travel = OverlayVector::new(0.0, -DISPLAY.size.height);
                    for path in plan.floating.iter_mut() {
                        path.to = path.from;
                    }
                }
                let flight = FlightPlan::from(plan.clone());
                let targets = animation_targets(&flight);
                let tag = format!("seed 137 run {run}");

                let mut containers: Vec<GroupKey> = Vec::new();
                let mut tiles: Vec<WindowId> = Vec::new();
                for target in &targets {
                    match *target {
                        AnimationTarget::Container { key, from, to } => {
                            assert!(!containers.contains(&key), "{tag}: {key:?} named twice");
                            containers.push(key);
                            let travel = match key {
                                GroupKey::Floating => plan.floating_travel,
                                key => plan.groups.iter().find(|g| g.key == key).unwrap().travel,
                            };
                            assert_eq!(
                                from,
                                OverlayPoint::origin(),
                                "{tag}: a fresh flight installs at the origin"
                            );
                            assert_eq!(to, travel.to_point(), "{tag}");
                            assert!(
                                travel.x != 0.0 || travel.y != 0.0,
                                "{tag}: a zero-travel container"
                            );
                        }
                        AnimationTarget::Tile { window, .. } => {
                            assert!(!tiles.contains(&window), "{tag}: {window:?} named twice");
                            tiles.push(window);
                        }
                    }
                }
                for group in &plan.groups {
                    for member in &group.members {
                        assert!(
                            !tiles.contains(&member.window),
                            "{tag}: rigid {:?} as a tile",
                            member.window
                        );
                    }
                    assert_eq!(
                        containers.contains(&group.key),
                        !group.is_still(),
                        "{tag}: group {:?} travel {:?}",
                        group.key,
                        group.travel
                    );
                }
                assert_eq!(containers.contains(&GroupKey::Floating), switch, "{tag}");
                let mut expected: Vec<WindowId> = plan
                    .changing
                    .iter()
                    .chain(&plan.entrances)
                    .chain(plan.floating.iter().filter(|path| !path.from.same_as(path.to)))
                    .map(|path| path.window)
                    .collect();
                expected.sort();
                tiles.sort();
                assert_eq!(tiles, expected, "{tag}: tile targets");
            }
        }
    }
}
