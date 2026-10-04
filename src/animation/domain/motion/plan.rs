//! A pass as rigid pieces: the pure half of the container model.
//! See "The overlay engine" and "Layout changes" in `src/animation/docs/animation-smoothness.md`.

use std::collections::HashMap;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use rini_geometry::stretch::is_a_resize;
use crate::animation::domain::motion::surface::{SurfaceGeometry, TileGeometry, pan_travel};
use rini_geometry::to_overlay_space;
use rini_core::ids::WindowId;
use rini_geometry::SameAs;
use rini_ipc::protocol::Direction;

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

/// One rigid piece of the strip.
#[derive(Clone, Debug, PartialEq)]
pub struct RigidGroup {
    pub key: GroupKey,
    /// Total displacement of the container from install to the current destination.
    pub travel: CGPoint,
    /// Members with their group-relative frames (constant for the flight unless reparented).
    pub members: Vec<GroupMember>,
}

impl RigidGroup {
    fn new(key: GroupKey, travel: CGPoint) -> Self {
        RigidGroup {
            key,
            travel,
            members: Vec::new(),
        }
    }

    pub fn is_still(&self) -> bool {
        self.travel.x == 0.0 && self.travel.y == 0.0
    }
}

/// A tile riding a container. The picture itself stays in `RunningAnimation.tiles`, found by window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupMember {
    pub window: WindowId,
    /// Frame inside the container: overlay-space `from` minus the container position at install.
    pub rel: CGRect,
    /// For a border window, the window it traces; drawn a quarter step in front of it.
    pub companion: Option<WindowId>,
}

/// How one window takes part in a plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Member {
    /// Rigid: rides its container. No per-tile animation.
    Rigid { key: GroupKey, rel: CGRect },
    /// Its size changes: own layer in `GroupKey::Loose`, animated as a resize from `from` to `to`.
    Changing { from: CGRect, to: CGRect },
    /// New window: own layer in `GroupKey::Loose`, from spawn frame to slot, stretched until reveal.
    Entrance { from: CGRect, to: CGRect },
    /// In the floating container; animated per tile only when `from != to`.
    Floating { from: CGRect, to: CGRect },
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
    pub fn start_frame(&self) -> CGRect {
        match self {
            Self::Rigid { rel, .. } => *rel,
            Self::Changing { from, .. }
            | Self::Entrance { from, .. }
            | Self::Floating { from, .. } => *from,
        }
    }
}

/// Where an entering window grows in from: zero width at its own left edge, full height.
pub fn entrance_from(to: CGRect) -> CGRect {
    CGRect::new(to.origin, CGSize::new(0.0, to.size.height))
}

/// A pass, described as rigid pieces. Pure output of [`reflow_plan`] / [`surface_plan`].
/// Every rect is in overlay space.
#[derive(Clone, Debug, PartialEq)]
pub struct ReflowPlan {
    /// `groups[0]` is the still group, always present, possibly empty.
    pub groups: Vec<RigidGroup>,
    pub changing: Vec<(WindowId, CGRect, CGRect)>,
    pub entrances: Vec<(WindowId, CGRect, CGRect)>,
    pub floating: Vec<(WindowId, CGRect, CGRect)>,
    /// Travel the floating container itself takes (a switch); zero for a pan or a layout pass.
    pub floating_travel: CGPoint,
}

impl ReflowPlan {
    pub fn empty() -> Self {
        ReflowPlan {
            groups: vec![RigidGroup::new(GroupKey::STILL, CGPoint::new(0.0, 0.0))],
            changing: Vec::new(),
            entrances: Vec::new(),
            floating: Vec::new(),
            floating_travel: CGPoint::new(0.0, 0.0),
        }
    }

    /// Puts a rigid member starting at `from` (overlay space) into the group travelling by `vector`,
    /// opening one when none is within `GROUP_TOLERANCE`. Containers install at `(0,0)`, so `rel == from`.
    fn place(&mut self, window: WindowId, from: CGRect, vector: CGPoint) -> GroupKey {
        let member = GroupMember {
            window,
            rel: from,
            companion: None,
        };
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

    /// Adds a tile the plan did not derive itself (a border companion) by the same
    /// rules: floating stays loose, a resize is `changing`, anything else rides the group with its vector.
    /// A floating tile's frames are in the floating container's space, as its anchor's are, so a
    /// floating window's border travels with the container and not a second time on its own.
    pub fn adopt<T: TileGeometry>(&mut self, tile: &T) {
        let (window, from, to) = (tile.window(), tile.from(), tile.to());
        if tile.floating() {
            self.floating.push((window, from, group_relative(to, self.floating_travel)));
        } else if is_a_resize(from.size, to.size) {
            self.changing.push((window, from, to));
        } else {
            let key = self.place(window, from, vector_of(from, to));
            if let Some(anchor) = tile.companion()
                && let Some(group) = self.groups.iter_mut().find(|g| g.key == key)
                && let Some(member) = group.members.iter_mut().find(|m| m.window == window)
            {
                member.companion = Some(anchor);
            }
        }
    }

    /// Moves the named windows from `changing` to `entrances`: a rebuilt plan sees only a resize.
    pub fn mark_entrances(&mut self, windows: &[WindowId]) {
        let (entrances, changing): (Vec<_>, Vec<_>) =
            self.changing.drain(..).partition(|(w, _, _)| windows.contains(w));
        self.changing = changing;
        self.entrances.extend(entrances);
    }

    /// The group `window` rides, if it is a rigid member.
    pub fn group_of(&self, window: WindowId) -> Option<&RigidGroup> {
        self.groups.iter().find(|g| g.members.iter().any(|m| m.window == window))
    }

    /// How `window` takes part, or `None` when the plan does not name it.
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
    pub fn windows(&self) -> Vec<WindowId> {
        windows_in(&self.groups, &self.changing, &self.entrances, &self.floating)
    }
}

/// How `window` takes part in a plan's lists, or `None` when none names it.
fn member_in(
    groups: &[RigidGroup],
    changing: &[(WindowId, CGRect, CGRect)],
    entrances: &[(WindowId, CGRect, CGRect)],
    floating: &[(WindowId, CGRect, CGRect)],
    window: WindowId,
) -> Option<Member> {
    for group in groups {
        if let Some(m) = group.members.iter().find(|m| m.window == window) {
            return Some(Member::Rigid { key: group.key, rel: m.rel });
        }
    }
    let find = |list: &[(WindowId, CGRect, CGRect)]| {
        list.iter().find(|(w, _, _)| *w == window).map(|(_, from, to)| (*from, *to))
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

/// Every window in the lists, in plan order, companions left out.
fn windows_in(
    groups: &[RigidGroup],
    changing: &[(WindowId, CGRect, CGRect)],
    entrances: &[(WindowId, CGRect, CGRect)],
    floating: &[(WindowId, CGRect, CGRect)],
) -> Vec<WindowId> {
    let mut out: Vec<WindowId> = groups
        .iter()
        .flat_map(|g| g.members.iter().filter(|m| m.companion.is_none()).map(|m| m.window))
        .collect();
    out.extend(changing.iter().map(|(w, _, _)| *w));
    out.extend(entrances.iter().map(|(w, _, _)| *w));
    out.extend(floating.iter().map(|(w, _, _)| *w));
    out
}

/// The running flight's plan. Adds container state the overlay needs to retarget.
#[derive(Clone, Debug, PartialEq)]
pub struct FlightPlan {
    pub groups: Vec<RigidGroup>,
    /// Model position of each container: install position plus every retarget's travel.
    pub positions: HashMap<GroupKey, CGPoint>,
    pub changing: Vec<(WindowId, CGRect, CGRect)>,
    pub entrances: Vec<(WindowId, CGRect, CGRect)>,
    pub floating: Vec<(WindowId, CGRect, CGRect)>,
    pub floating_travel: CGPoint,
    pub next_key: u16,
}

impl FlightPlan {
    /// A flight with nothing in it: the still group only.
    pub fn empty() -> Self {
        FlightPlan::from(ReflowPlan::empty())
    }

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

    /// Every window the flight names, companions left out.
    pub fn windows(&self) -> Vec<WindowId> {
        windows_in(&self.groups, &self.changing, &self.entrances, &self.floating)
    }
}

impl From<ReflowPlan> for FlightPlan {
    /// A fresh flight: every container installs at `(0,0)`, so each group's model position is its travel.
    fn from(plan: ReflowPlan) -> Self {
        let mut positions: HashMap<GroupKey, CGPoint> =
            plan.groups.iter().map(|g| (g.key, g.travel)).collect();
        positions.insert(GroupKey::Loose, CGPoint::new(0.0, 0.0));
        positions.insert(GroupKey::Floating, plan.floating_travel);
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
    /// Depth inside its container per tile; a companion carries its window's.
    pub within: HashMap<WindowId, usize>,
    /// Strip containers front to back: the one holding focus first, then by shallowest member.
    pub group_order: Vec<GroupKey>,
}

/// Where each container of a flight is at the moment of a merge, read by the overlay just before.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Presented {
    /// Where it is drawn, any bounce or nudge riding it included: where a member leaving it starts.
    pub drawn: HashMap<GroupKey, CGPoint>,
    /// Where its own leg has it, with nothing riding it: what remaining travel and a newcomer's
    /// frame are judged against, since every destination and every frame a pass carries is one of
    /// these.
    pub along: HashMap<GroupKey, CGPoint>,
}

/// A flight with nothing riding its containers: each is drawn where its leg has it.
#[cfg(test)]
impl From<HashMap<GroupKey, CGPoint>> for Presented {
    fn from(positions: HashMap<GroupKey, CGPoint>) -> Self {
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
    /// Where it installs, overlay space; its members and destination are in the plan.
    pub install: CGPoint,
    /// The container its member left, whose motion it carries on; `None` for a newcomer's.
    pub leaving: Option<GroupKey>,
}

/// What one merge changed, for the overlay. Pure output of [`merge_plans`]; every frame the
/// overlay needs is in the merged plan, found by window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanDelta {
    /// Containers to animate from their presented position to `to` (overlay space).
    pub retargeted_groups: Vec<(GroupKey, CGPoint)>,
    pub new_groups: Vec<NewGroup>,
    /// Members moving container: (window, from_key, to_key). The new frame is in the plan.
    pub reparented: Vec<(WindowId, GroupKey, GroupKey)>,
    /// Loose tiles to bend toward a new destination, in their container's space.
    pub retargeted_tiles: Vec<(WindowId, CGRect)>,
    /// Tiles joining the flight, with their container; frames are in the plan.
    pub joined_tiles: Vec<(WindowId, GroupKey)>,
    pub focus_changed: bool,
}

impl PlanDelta {
    /// A merge that changed nothing: the redundant pass of a rapid press.
    pub fn is_empty(&self) -> bool {
        !self.moves_anything() && !self.focus_changed
    }

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
    Group(GroupKey, CGRect),
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
        let index =
            |list: &[(WindowId, CGRect, CGRect)]| list.iter().position(|(w, _, _)| *w == window);
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

    fn position(&self, key: GroupKey) -> CGPoint {
        self.positions.get(&key).copied().unwrap_or(CGPoint::new(0.0, 0.0))
    }

    /// Model position of container `key`: where its members' `rel` frames land.
    pub fn position_of(&self, key: GroupKey) -> CGPoint {
        self.position(key)
    }

    fn group_mut(&mut self, key: GroupKey) -> &mut RigidGroup {
        self.groups.iter_mut().find(|g| g.key == key).expect("a key the plan allocated")
    }

    fn take_member(&mut self, key: GroupKey, window: WindowId) -> GroupMember {
        let group = self.group_mut(key);
        let at = group.members.iter().position(|m| m.window == window).expect("located there");
        group.members.remove(at)
    }

    /// Moves container `key`'s destination to `p`; its travel grows by the same amount.
    fn move_group(&mut self, key: GroupKey, p: CGPoint, delta: &mut PlanDelta) {
        let old = self.position(key);
        if p.same_as(old) {
            return;
        }
        self.positions.insert(key, p);
        let group = self.group_mut(key);
        group.travel = CGPoint::new(group.travel.x + p.x - old.x, group.travel.y + p.y - old.y);
        match delta.retargeted_groups.iter_mut().find(|(k, _)| *k == key) {
            Some(retarget) => retarget.1 = p,
            None => delta.retargeted_groups.push((key, p)),
        }
    }

    /// Opens a group installing at `install` and landing at `destination`, with one member, which
    /// left container `leaving` if it was in one.
    fn open_group(
        &mut self,
        install: CGPoint,
        destination: CGPoint,
        member: GroupMember,
        leaving: Option<GroupKey>,
        presented: &mut Presented,
        delta: &mut PlanDelta,
    ) -> GroupKey {
        let key = GroupKey::Rigid(self.next_key);
        self.next_key += 1;
        let travel = CGPoint::new(destination.x - install.x, destination.y - install.y);
        let mut group = RigidGroup::new(key, travel);
        group.members.push(member);
        self.groups.push(group);
        self.positions.insert(key, destination);
        presented.drawn.insert(key, install);
        presented.along.insert(key, install);
        delta.new_groups.push(NewGroup { key, install, leaving });
        key
    }
}

fn add(a: CGPoint, b: CGPoint) -> CGPoint {
    CGPoint::new(a.x + b.x, a.y + b.y)
}

fn sub(a: CGPoint, b: CGPoint) -> CGPoint {
    CGPoint::new(a.x - b.x, a.y - b.y)
}

fn is_zero(p: CGPoint) -> bool {
    p.x == 0.0 && p.y == 0.0
}

/// Folds a later pass into a flight in progress: containers are retargeted, membership changes
/// are reparented at the frames they are drawn at, a pan adds its travel to every group and the
/// columns a move swapped still vote under it. `presented` is where each container is, read by the
/// overlay just before. See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
pub fn merge_plans(
    current: &FlightPlan,
    incoming: &ReflowPlan,
    pan: Option<CGPoint>,
    presented: &Presented,
    focus: Option<WindowId>,
    viewport: CGRect,
) -> (FlightPlan, PlanDelta) {
    let mut next = current.clone();
    let mut delta = PlanDelta::default();
    let mut presented = presented.clone();
    for (key, p) in &current.positions {
        presented.drawn.entry(*key).or_insert(*p);
        presented.along.entry(*key).or_insert(*p);
    }
    let position_in = |positions: &HashMap<GroupKey, CGPoint>, key: GroupKey| {
        positions.get(&key).copied().unwrap_or(CGPoint::new(0.0, 0.0))
    };

    // 1. A strip pan: every group and every loose strip tile moves by `d`; nothing changes hands.
    let pan = pan.filter(|d| !is_zero(*d));
    if let Some(d) = pan {
        let keys: Vec<GroupKey> =
            next.groups.iter().filter(|g| !g.members.is_empty()).map(|g| g.key).collect();
        for key in keys {
            let p = add(next.position(key), d);
            next.move_group(key, p, &mut delta);
        }
        for i in 0..next.changing.len() {
            next.changing[i].2.origin = add(next.changing[i].2.origin, d);
            delta.retargeted_tiles.push((next.changing[i].0, next.changing[i].2));
        }
        for i in 0..next.entrances.len() {
            next.entrances[i].2.origin = add(next.entrances[i].2.origin, d);
            delta.retargeted_tiles.push((next.entrances[i].0, next.entrances[i].2));
        }
    }
    // A switch: the floating container travels too. Floating frames stay container-relative.
    let floating_before = next.position(GroupKey::Floating);
    if !is_zero(incoming.floating_travel) {
        let p = add(floating_before, incoming.floating_travel);
        next.floating_travel = add(next.floating_travel, incoming.floating_travel);
        next.positions.insert(GroupKey::Floating, p);
        delta.retargeted_groups.push((GroupKey::Floating, p));
    }

    // 2. Rigid members of the pass: votes for their group's new position, or a join. Joins wait
    // for step 3: a joiner picks its container by remaining travel, which the votes may change.
    let mut votes: Vec<(GroupKey, WindowId, CGPoint)> = Vec::new();
    let mut joins: Vec<(GroupMember, CGPoint)> = Vec::new();
    for group in &incoming.groups {
        // Under a pan, only a group not travelling with it votes: a column a move swapped, heading
        // for a slot of its own. Its destination is a place on the strip, never a park, so it
        // votes wherever that is.
        let swapped = pan.is_some_and(|d| !same_vector(group.travel, d));
        for m in &group.members {
            let to = overlay_of(m.rel, group.travel);
            match next.locate(m.window) {
                Located::Group(key, rel) => {
                    if swapped || (pan.is_none() && !rides_out(&next, key, to, viewport)) {
                        votes.push((key, m.window, sub(to.origin, rel.origin)));
                    }
                }
                // Mid-resize: it stays loose for this flight and bends to the new destination.
                Located::Changing(i) => retarget_loose(&mut next.changing, i, to, &mut delta),
                Located::Entrance(i) => retarget_loose(&mut next.entrances, i, to, &mut delta),
                Located::Floating(i) => {
                    let to_rel = group_relative(to, floating_before);
                    retarget_loose(&mut next.floating, i, to_rel, &mut delta);
                }
                Located::Absent => {
                    let member = GroupMember {
                        window: m.window,
                        rel: m.rel,
                        companion: m.companion,
                    };
                    joins.push((member, group.travel));
                }
            }
        }
    }

    // 3. Per voted group: the largest cluster keeps the container; the rest are reparented. Under
    // a pan, the members of a voted group that did not vote hold the place the pan gave them.
    let mut keys: Vec<GroupKey> = Vec::new();
    for (key, _, _) in &votes {
        if !keys.contains(key) {
            keys.push(*key);
        }
    }
    if pan.is_some() {
        for &key in &keys {
            let hold = next.position(key);
            let holders: Vec<WindowId> = next
                .groups
                .iter()
                .filter(|g| g.key == key)
                .flat_map(|g| g.members.iter().map(|m| m.window))
                .filter(|window| !votes.iter().any(|(_, w, _)| w == window))
                .collect();
            votes.extend(holders.into_iter().map(|window| (key, window, hold)));
        }
    }
    for key in keys {
        let mut clusters: Vec<(CGPoint, Vec<(WindowId, CGPoint)>)> = Vec::new();
        for (k, window, p) in votes.iter().copied().filter(|(k, _, _)| *k == key) {
            debug_assert_eq!(k, key);
            match clusters.iter_mut().find(|(c, _)| same_vector(*c, p)) {
                Some((_, members)) => members.push((window, p)),
                None => clusters.push((p, vec![(window, p)])),
            }
        }
        let winner = clusters
            .iter()
            .enumerate()
            .max_by_key(|(i, (_, members))| {
                let holds_focus = focus.is_some_and(|f| members.iter().any(|(w, _)| *w == f));
                (members.len(), holds_focus, std::cmp::Reverse(*i))
            })
            .map(|(i, _)| i)
            .expect("a voted group has a cluster");
        let p = clusters[winner].0;
        next.move_group(key, p, &mut delta);
        let from_drawn = position_in(&presented.drawn, key);
        let from_along = position_in(&presented.along, key);
        for (i, (_, losers)) in clusters.iter().enumerate() {
            if i == winner {
                continue;
            }
            for &(window, p) in losers {
                let member = next.take_member(key, window);
                // What the member still has to travel: a group with the same remaining travel
                // carries it there. Else it leaves in a container of its own from where it is
                // drawn.
                let remaining = sub(p, from_along);
                let to_key = match landing_for(
                    &next,
                    &presented.along,
                    remaining,
                    Some(key),
                    member.companion,
                ) {
                    Some(to_key) => {
                        let shift = sub(from_along, position_in(&presented.along, to_key));
                        let rel = CGRect::new(add(member.rel.origin, shift), member.rel.size);
                        next.group_mut(to_key).members.push(GroupMember { rel, ..member });
                        to_key
                    }
                    None => next.open_group(
                        from_drawn,
                        p,
                        member,
                        Some(key),
                        &mut presented,
                        &mut delta,
                    ),
                };
                delta.reparented.push((window, key, to_key));
            }
        }
    }

    for (member, vector) in joins {
        join(&mut next, member, vector, &mut presented, &mut delta);
    }

    // 4. Loose members of the pass.
    for (list_is_entrance, entries) in [(false, &incoming.changing), (true, &incoming.entrances)] {
        for &(window, from, to) in entries {
            match next.locate(window) {
                Located::Group(key, rel) => {
                    // Rigid until now: it leaves its container at the frame it is drawn at.
                    next.take_member(key, window);
                    let at = overlay_of(rel, position_in(&presented.drawn, key));
                    next.changing.push((window, at, to));
                    delta.reparented.push((window, key, GroupKey::Loose));
                    delta.retargeted_tiles.push((window, to));
                }
                Located::Changing(i) => retarget_loose(&mut next.changing, i, to, &mut delta),
                Located::Entrance(i) => retarget_loose(&mut next.entrances, i, to, &mut delta),
                Located::Floating(i) => {
                    let to_rel = group_relative(to, floating_before);
                    retarget_loose(&mut next.floating, i, to_rel, &mut delta);
                }
                Located::Absent => {
                    if list_is_entrance {
                        next.entrances.push((window, from, to));
                    } else {
                        next.changing.push((window, from, to));
                    }
                    delta.joined_tiles.push((window, GroupKey::Loose));
                }
            }
        }
    }
    for &(window, from, to) in &incoming.floating {
        let to_rel = group_relative(to, floating_before);
        match next.locate(window) {
            Located::Floating(i) => retarget_loose(&mut next.floating, i, to_rel, &mut delta),
            Located::Group(key, rel) => {
                next.take_member(key, window);
                let at = overlay_of(rel, position_in(&presented.drawn, key));
                let from_rel =
                    group_relative(at, position_in(&presented.drawn, GroupKey::Floating));
                next.floating.push((window, from_rel, to_rel));
                delta.reparented.push((window, key, GroupKey::Floating));
                delta.retargeted_tiles.push((window, to_rel));
            }
            Located::Changing(i) => retarget_loose(&mut next.changing, i, to, &mut delta),
            Located::Entrance(i) => retarget_loose(&mut next.entrances, i, to, &mut delta),
            Located::Absent => {
                let from_rel =
                    group_relative(from, position_in(&presented.along, GroupKey::Floating));
                next.floating.push((window, from_rel, to_rel));
                delta.joined_tiles.push((window, GroupKey::Floating));
            }
        }
    }

    // A group the pan moved and a vote brought back is still going where it was.
    delta
        .retargeted_groups
        .retain(|(key, p)| current.positions.get(key).is_none_or(|was| !was.same_as(*p)));
    (next, delta)
}

/// A rigid member the pass sends off the viewport while its container is moving keeps riding
/// the container. It has no visible destination, and voting its own vector opened a group that
/// swept it sideways to the park while the row it sat in travelled vertically (the zig-zag seen
/// on a switch whose layout pass parked the departing row). A still container has no motion to
/// lend, so its member votes and exits on its own.
fn rides_out(next: &FlightPlan, key: GroupKey, to: CGRect, viewport: CGRect) -> bool {
    let moving = next.groups.iter().any(|g| g.key == key && !g.is_still());
    moving && rini_geometry::is_off_screen(viewport, to)
}

/// Bends one loose tile toward `to` (its container's space) unless it is already going there.
fn retarget_loose(
    list: &mut [(WindowId, CGRect, CGRect)],
    index: usize,
    to: CGRect,
    delta: &mut PlanDelta,
) {
    if list[index].2.same_as(to) {
        return;
    }
    list[index].2 = to;
    delta.retargeted_tiles.push((list[index].0, to));
}

/// The group whose remaining travel (destination less where its leg has it, `along`) matches
/// `remaining`, for a member arriving from elsewhere; a border prefers the group of the window it
/// traces.
fn landing_for(
    next: &FlightPlan,
    along: &HashMap<GroupKey, CGPoint>,
    remaining: CGPoint,
    exclude: Option<GroupKey>,
    companion: Option<WindowId>,
) -> Option<GroupKey> {
    let holds = |g: &RigidGroup, window: WindowId| g.members.iter().any(|m| m.window == window);
    let fits = |g: &&RigidGroup| {
        let p = along.get(&g.key).copied().unwrap_or(next.position(g.key));
        Some(g.key) != exclude && same_vector(sub(next.position(g.key), p), remaining)
    };
    let traced = next
        .groups
        .iter()
        .filter(fits)
        .find(|g| companion.is_some_and(|anchor| holds(g, anchor)));
    traced.or_else(|| next.groups.iter().find(fits)).map(|g| g.key)
}

/// A rigid newcomer: rides a group whose remaining travel matches its vector, else opens one. Its
/// frame is a place on the strip, as the group's leg is, so it is taken against that and not where
/// the group is drawn: a bounce or nudge riding the group carries the newcomer with it.
fn join(
    next: &mut FlightPlan,
    member: GroupMember,
    vector: CGPoint,
    presented: &mut Presented,
    delta: &mut PlanDelta,
) {
    match landing_for(next, &presented.along, vector, None, member.companion) {
        Some(key) => {
            let p = presented.along.get(&key).copied().unwrap_or(next.position(key));
            let rel = group_relative(member.rel, p);
            next.group_mut(key).members.push(GroupMember { rel, ..member });
            delta.joined_tiles.push((member.window, key));
        }
        None => {
            next.open_group(CGPoint::new(0.0, 0.0), vector, member, None, presented, delta);
        }
    }
}

/// Two translation vectors within `GROUP_TOLERANCE` on both axes.
pub fn same_vector(a: CGPoint, b: CGPoint) -> bool {
    (a.x - b.x).abs() <= GROUP_TOLERANCE && (a.y - b.y).abs() <= GROUP_TOLERANCE
}

/// `to.origin - from.origin`.
fn vector_of(from: CGRect, to: CGRect) -> CGPoint {
    CGPoint::new(to.origin.x - from.origin.x, to.origin.y - from.origin.y)
}

/// A frame in its container's space: `frame` less the container's position.
pub fn group_relative(frame: CGRect, position: CGPoint) -> CGRect {
    CGRect::new(
        CGPoint::new(frame.origin.x - position.x, frame.origin.y - position.y),
        frame.size,
    )
}

/// The inverse of [`group_relative`]: a group-relative frame back in overlay space.
pub fn overlay_of(rel: CGRect, position: CGPoint) -> CGRect {
    CGRect::new(
        CGPoint::new(rel.origin.x + position.x, rel.origin.y + position.y),
        rel.size,
    )
}

/// A layout pass as rigid pieces. `requests` are display-space `(window, start, end, floating)`
/// after `start` has resolved parks and dropped what is not worth animating.
pub fn reflow_plan(requests: &[(WindowId, CGRect, CGRect, bool)], display: CGRect) -> ReflowPlan {
    let mut plan = ReflowPlan::empty();
    for &(window, start, end, floating) in requests {
        let from = to_overlay_space(start, display);
        let to = to_overlay_space(end, display);
        if floating {
            plan.floating.push((window, from, to));
        } else if is_a_resize(from.size, to.size) {
            plan.changing.push((window, from, to));
        } else {
            plan.place(window, from, vector_of(from, to));
        }
    }
    plan
}

/// Frame zero again from a flight's merged tiles (overlay space): every tile adopted by the
/// `reflow_plan` rules. Used while a flight is still collecting passes.
pub fn plan_from_tiles<T: TileGeometry>(tiles: &[T]) -> ReflowPlan {
    let mut plan = ReflowPlan::empty();
    for tile in tiles {
        plan.adopt(tile);
    }
    plan
}

/// A strip movement as rigid pieces: the strip is one piece, and each column a move swapped that
/// crosses it is a piece of its own, grouped by the vector it travels. The surface is already
/// overlay space. Pinned windows stand in the floating container; unpinned floating windows ride
/// it by the strip's travel.
pub fn surface_plan<W: SurfaceGeometry>(
    windows: &[W],
    from_offset: CGPoint,
    to_offset: CGPoint,
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
            plan.floating.push((window.window(), from, from));
        } else if window.floating() {
            if mixed {
                plan.floating.push((window.window(), from, to));
            } else {
                plan.floating.push((window.window(), from, from));
                plan.floating_travel = travel;
            }
        } else if window.from().is_some() {
            plan.place(window.window(), from, vector_of(from, to));
        } else {
            plan.place(window.window(), from, travel);
        }
    }
    plan
}

/// Whether the container `key` names takes part in an out-and-back by `offset`, an edge bounce or
/// a move's nudge. Strip containers always; the floating container only when it is vertical, the
/// same rule a pan (floating pinned) and a switch (floating carried) follow. A nudge is always
/// sideways, so it steps the whole strip and never a floating window.
pub fn carries_out_and_back(key: GroupKey, offset: CGPoint) -> bool {
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
pub fn edge_bounce_overshoot(direction: Direction) -> CGPoint {
    match direction {
        Direction::Left => CGPoint::new(EDGE_BOUNCE_OVERSHOOT, 0.0),
        Direction::Right => CGPoint::new(-EDGE_BOUNCE_OVERSHOOT, 0.0),
        Direction::Up => CGPoint::new(0.0, EDGE_BOUNCE_OVERSHOOT),
        Direction::Down => CGPoint::new(0.0, -EDGE_BOUNCE_OVERSHOOT),
    }
}
/// One movement `fly` installs: a container's translation, or one loose tile's own animation.
/// Pure output of [`animation_targets`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnimationTarget {
    Container {
        key: GroupKey,
        from: CGPoint,
        to: CGPoint,
    },
    Tile {
        window: WindowId,
        from: CGRect,
        to: CGRect,
    },
}

/// What a flight animates, and nothing else: one `Container` per strip group with travel, the
/// floating container when it travels, and a `Tile` per changing, entrance and moving floating
/// member. Rigid members are never named. See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
pub fn animation_targets(plan: &FlightPlan) -> Vec<AnimationTarget> {
    let mut out = Vec::new();
    let position =
        |key: GroupKey, travel: CGPoint| plan.positions.get(&key).copied().unwrap_or(travel);
    for group in plan.groups.iter().filter(|g| !g.is_still()) {
        let to = position(group.key, group.travel);
        let from = CGPoint::new(to.x - group.travel.x, to.y - group.travel.y);
        out.push(AnimationTarget::Container { key: group.key, from, to });
    }
    let ft = plan.floating_travel;
    if ft.x != 0.0 || ft.y != 0.0 {
        let to = position(GroupKey::Floating, ft);
        let from = CGPoint::new(to.x - ft.x, to.y - ft.y);
        out.push(AnimationTarget::Container {
            key: GroupKey::Floating,
            from,
            to,
        });
    }
    for &(window, from, to) in plan.changing.iter().chain(&plan.entrances) {
        out.push(AnimationTarget::Tile { window, from, to });
    }
    for &(window, from, to) in &plan.floating {
        if !from.same_as(to) {
            out.push(AnimationTarget::Tile { window, from, to });
        }
    }
    out
}

/// What an overlay pass should stop drawing: the tiles no longer in the plan, and the containers
/// left with nothing in them.
///
/// Two stages, and the order between them is the point. A container is judged empty by the tiles
/// that REMAIN, so the tiles have to go first. Judging containers against the original set keeps a
/// container whose only tile just left, and an empty container is a layer the compositor keeps
/// compositing.
pub fn stale_overlay_layers<K: Copy + PartialEq>(
    tiles: &[(WindowId, Option<K>)],
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
        .filter_map(|(_, key)| *key)
        .collect();
    let empty: Vec<K> = containers.iter().copied().filter(|key| !occupied.contains(key)).collect();
    (stale_tiles, empty)
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    /// `animation_targets`: what `fly` installs and nothing else (Requirement 11.4 of
    /// `rigid-strip-groups`). Plans built with `reflow_plan` on a 1728x1117 display at the origin,
    /// so overlay space equals display space.
    mod targets {
        use super::*;

        const DISPLAY: CGRect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width: 1728.0, height: 1117.0 },
        };

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
        }

        fn column(i: f64) -> CGRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        fn shifted(frame: CGRect, dx: f64) -> CGRect {
            CGRect::new(CGPoint::new(frame.origin.x + dx, frame.origin.y), frame.size)
        }

        fn flight(requests: &[(WindowId, CGRect, CGRect, bool)]) -> FlightPlan {
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
                (wid(1), a, shifted(a, -859.0), false),
                (wid(2), b, shifted(b, -859.0), false),
            ]);
            let targets = animation_targets(&plan);
            assert_eq!(
                targets,
                vec![AnimationTarget::Container {
                    key: GroupKey::Rigid(1),
                    from: CGPoint::new(0.0, 0.0),
                    to: CGPoint::new(-859.0, 0.0),
                }]
            );
        }

        #[test]
        fn a_still_group_and_two_moving_groups_are_two_container_targets() {
            let (a, b, c) = (column(0.0), column(1.0), column(2.0));
            let plan = flight(&[
                (wid(1), a, a, false),
                (wid(2), b, shifted(b, 300.0), false),
                (wid(3), c, shifted(c, -300.0), false),
            ]);
            let targets = animation_targets(&plan);
            assert_eq!(
                containers(&targets),
                vec![GroupKey::Rigid(1), GroupKey::Rigid(2)]
            );
            assert!(tiles(&targets).is_empty(), "no rigid member is a tile target");
        }

        #[test]
        fn a_changing_member_is_a_tile_target_and_rigid_members_are_not() {
            let (a, b) = (column(0.0), column(1.0));
            let grown = rect(a.origin.x, a.origin.y, a.size.width + 400.0, a.size.height);
            let plan = flight(&[
                (wid(1), a, grown, false),
                (wid(2), b, shifted(b, 400.0), false),
            ]);
            let targets = animation_targets(&plan);
            assert_eq!(tiles(&targets), vec![wid(1)]);
            assert_eq!(containers(&targets), vec![GroupKey::Rigid(1)]);
            assert!(targets.contains(&AnimationTarget::Tile {
                window: wid(1),
                from: a,
                to: grown
            }));
        }

        #[test]
        fn the_floating_container_is_a_target_iff_it_travels() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let mut plan = flight(&[(wid(1), settings, settings, true)]);
            assert!(
                animation_targets(&plan).is_empty(),
                "a standing floating window: nothing flies"
            );

            plan.floating_travel = CGPoint::new(0.0, -1117.0);
            plan.positions.insert(GroupKey::Floating, plan.floating_travel);
            assert_eq!(
                animation_targets(&plan),
                vec![AnimationTarget::Container {
                    key: GroupKey::Floating,
                    from: CGPoint::new(0.0, 0.0),
                    to: CGPoint::new(0.0, -1117.0),
                }]
            );

            let moved = shifted(settings, -100.0);
            let plan = flight(&[(wid(1), settings, moved, true)]);
            assert_eq!(
                animation_targets(&plan),
                vec![AnimationTarget::Tile {
                    window: wid(1),
                    from: settings,
                    to: moved
                }],
                "a floating window moving on its own is a tile target, not a container"
            );
        }

        #[test]
        fn zero_travel_groups_are_never_targets() {
            let (a, b) = (column(0.0), column(1.0));
            let plan = flight(&[(wid(1), a, a, false), (wid(2), b, b, false)]);
            assert!(animation_targets(&plan).is_empty());
            assert_eq!(plan.groups.len(), 1, "everything standing is the still group");
        }
    }

    /// Strip containers always bounce and step; the floating container only with the stack
    /// (vertical), the way a pan pins floating windows and a switch carries them. A move's step is
    /// sideways, so it never carries a floating window.
    #[test]
    fn the_floating_container_bounces_only_vertically_and_never_steps() {
        let sideways = CGPoint::new(-EDGE_BOUNCE_OVERSHOOT, 0.0);
        let upward = CGPoint::new(0.0, -EDGE_BOUNCE_OVERSHOOT);
        let share = 1728.0 * crate::animation::domain::motion::nudge::NUDGE_SHARE;
        let steps = [CGPoint::new(-share, 0.0), CGPoint::new(share, 0.0)];
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
        assert_eq!(edge_bounce_overshoot(Direction::Right), CGPoint::new(-o, 0.0));
        assert_eq!(edge_bounce_overshoot(Direction::Left), CGPoint::new(o, 0.0));
        assert_eq!(edge_bounce_overshoot(Direction::Down), CGPoint::new(0.0, -o));
        assert_eq!(edge_bounce_overshoot(Direction::Up), CGPoint::new(0.0, o));
        assert!(o < 100.0, "a nudge, not a scroll");
    }

    #[test]
    fn a_tile_the_plan_still_names_is_kept() {
        let tiles = [
            (WindowId::new(1, 1), Some(7u8)),
            (WindowId::new(1, 2), Some(7)),
        ];
        let keep = [WindowId::new(1, 1), WindowId::new(1, 2)];
        let (stale, empty) = stale_overlay_layers(&tiles, &keep, &[7]);
        assert!(stale.is_empty());
        assert!(empty.is_empty(), "the container still has both tiles");
    }

    #[test]
    fn a_tile_the_plan_dropped_is_stale() {
        let tiles = [
            (WindowId::new(1, 1), Some(7u8)),
            (WindowId::new(1, 2), Some(7)),
        ];
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
        let tiles = [
            (WindowId::new(1, 1), Some(7u8)),
            (WindowId::new(1, 2), Some(8)),
        ];
        let keep = [WindowId::new(1, 1)];
        let (stale, empty) = stale_overlay_layers(&tiles, &keep, &[7, 8]);
        assert_eq!(stale, [WindowId::new(1, 2)]);
        assert_eq!(empty, [8], "container 8 lost its only tile");
    }

    #[test]
    fn a_container_no_tile_ever_claimed_is_empty() {
        let tiles = [(WindowId::new(1, 1), Some(7u8))];
        let keep = [WindowId::new(1, 1)];
        let (_, empty) = stale_overlay_layers(&tiles, &keep, &[7, 9]);
        assert_eq!(empty, [9]);
    }

    /// A tile with no container of its own keeps none alive.
    #[test]
    fn a_tile_outside_every_container_holds_nothing_open() {
        let tiles = [(WindowId::new(1, 1), None::<u8>)];
        let keep = [WindowId::new(1, 1)];
        let (stale, empty) = stale_overlay_layers(&tiles, &keep, &[7]);
        assert!(stale.is_empty());
        assert_eq!(empty, [7]);
    }

    /// A rigid member rides its container, so it is not moving on its own and `rel` is both its
    /// start and its end. Reading `to` for a reparented one would install the layer at its
    /// destination and animate nowhere.
    #[test]
    fn a_rigid_member_starts_where_it_sits() {
        let rel = CGRect::new(CGPoint::new(10.0, 20.0), CGSize::new(100.0, 50.0));
        let member = Member::Rigid { key: GroupKey::STILL, rel };
        assert_eq!(member.start_frame(), rel);
    }

    #[test]
    fn every_animated_member_starts_at_its_from() {
        let from = CGRect::new(CGPoint::new(1.0, 2.0), CGSize::new(3.0, 4.0));
        let to = CGRect::new(CGPoint::new(90.0, 90.0), CGSize::new(3.0, 4.0));
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
        let to = rect(300., 50., 400., 600.);
        let from = entrance_from(to);
        assert_eq!(from.origin, to.origin);
        assert_eq!(from.size.width, 0.0);
        assert_eq!(from.size.height, to.size.height, "full height from the start");
    }

    /// A pass merging into a flight in progress (`merge_plans`). The 3:27:20 tear is the case it
    /// exists for. See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
    mod merges {
        use rini_geometry::{SameAs, to_overlay_space};

        use super::pieces::random_requests;
        use super::*;
        use crate::animation::domain::motion::testing::{DISPLAY, Gen, OnSurface, RUNS, StagedTile};

        /// An external display at a non-zero origin, so a retarget that forgot the conversion shows.
        const EXTERNAL: CGRect = CGRect {
            origin: CGPoint { x: 1728.0, y: -300.0 },
            size: CGSize { width: 3008.0, height: 1692.0 },
        };

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
        }

        fn shifted(frame: CGRect, delta: CGPoint) -> CGRect {
            CGRect::new(
                CGPoint::new(frame.origin.x + delta.x, frame.origin.y + delta.y),
                frame.size,
            )
        }

        fn column(i: f64) -> CGRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        fn flight_of(requests: &[(WindowId, CGRect, CGRect, bool)]) -> FlightPlan {
            FlightPlan::from(reflow_plan(requests, DISPLAY))
        }

        /// A window's destination in overlay space, whatever it rides.
        fn dest(plan: &FlightPlan, window: WindowId) -> Option<CGRect> {
            Some(match plan.member(window)? {
                Member::Rigid { key, rel } => overlay_of(rel, plan.position_of(key)),
                Member::Changing { to, .. } | Member::Entrance { to, .. } => to,
                Member::Floating { to, .. } => overlay_of(to, plan.position_of(GroupKey::Floating)),
            })
        }

        fn key_of(plan: &FlightPlan, window: WindowId) -> Option<GroupKey> {
            match plan.member(window)? {
                Member::Rigid { key, .. } => Some(key),
                Member::Changing { .. } | Member::Entrance { .. } => Some(GroupKey::Loose),
                Member::Floating { .. } => Some(GroupKey::Floating),
            }
        }

        /// Model positions as the presented ones: a flight that has not started drawing.
        fn at_model(plan: &FlightPlan) -> HashMap<GroupKey, CGPoint> {
            plan.positions.clone()
        }

        /// Halfway between install (`p - travel`) and destination, per container.
        fn midway(plan: &FlightPlan) -> HashMap<GroupKey, CGPoint> {
            plan.positions
                .iter()
                .map(|(key, p)| {
                    let travel = match key {
                        GroupKey::Floating => plan.floating_travel,
                        GroupKey::Loose => CGPoint::new(0.0, 0.0),
                        key => plan
                            .groups
                            .iter()
                            .find(|g| g.key == *key)
                            .map(|g| g.travel)
                            .unwrap_or(CGPoint::new(0.0, 0.0)),
                    };
                    (*key, CGPoint::new(p.x - travel.x / 2.0, p.y - travel.y / 2.0))
                })
                .collect()
        }

        #[test]
        fn a_pan_merging_into_an_open_shifts_every_group_and_reparents_nothing() {
            let (a, b, c) = (column(0.0), column(1.0), column(2.0));
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let mut current = flight_of(&[
                (wid(1), a, a, false),
                (wid(2), b, shifted(b, CGPoint::new(859.0, 0.0)), false),
                (wid(3), c, shifted(c, CGPoint::new(859.0, 0.0)), false),
            ]);
            current.entrances.push((wid(9), entrance_from(slot), slot));
            let before = current.clone();

            let d = CGPoint::new(-574.0, 0.0);
            let pan = surface_plan(
                &[
                    OnSurface {
                        window: wid(1),
                        frame: a,
                        pinned: false,
                        floating: false,
                        from: None,
                    },
                    OnSurface {
                        window: wid(2),
                        frame: shifted(b, CGPoint::new(859.0, 0.0)),
                        pinned: false,
                        floating: false,
                        from: None,
                    },
                ],
                CGPoint::new(-574.0, 0.0),
                CGPoint::new(0.0, 0.0),
            );
            let (merged, delta) = merge_plans(
                &current,
                &pan,
                Some(d),
                &at_model(&current).into(),
                None,
                DISPLAY,
            );

            for group in before.groups.iter().filter(|g| !g.members.is_empty()) {
                let p = merged.position_of(group.key);
                assert_eq!(
                    p,
                    CGPoint::new(
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
            assert_eq!(merged.entrances[0].2, shifted(slot, d));
            assert_eq!(delta.retargeted_tiles, vec![(wid(9), shifted(slot, d))]);
            assert_eq!(
                dest(&merged, wid(3)),
                Some(shifted(c, CGPoint::new(859.0 + d.x, 0.0))),
                "a member the pan did not compose rides its group"
            );
        }

        #[test]
        fn a_redundant_pass_is_an_empty_delta() {
            let (a, b) = (column(0.0), column(1.0));
            let requests = [
                (wid(1), a, a, false),
                (wid(2), b, shifted(b, CGPoint::new(-300.0, 0.0)), false),
            ];
            let current = flight_of(&requests);
            let again = reflow_plan(&requests, DISPLAY);
            let (merged, delta) =
                merge_plans(&current, &again, None, &midway(&current).into(), None, DISPLAY);
            assert!(delta.is_empty(), "{delta:?}");
            assert_eq!(merged, current);
        }

        #[test]
        fn a_pass_moving_one_member_elsewhere_reparents_it_and_keeps_the_other() {
            let (a, b) = (column(0.0), column(1.0));
            let v = CGPoint::new(-300.0, 0.0);
            let current = flight_of(&[
                (wid(1), a, shifted(a, v), false),
                (wid(2), b, shifted(b, v), false),
            ]);
            let group = current.groups[1].key;
            let presented = midway(&current);
            let elsewhere = shifted(b, CGPoint::new(500.0, 0.0));
            let pass = reflow_plan(
                &[
                    (wid(1), a, shifted(a, v), false),
                    (wid(2), b, elsewhere, false),
                ],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &presented.clone().into(), None, DISPLAY);
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
                dest(&merged, wid(2)).unwrap().same_as(elsewhere),
                "{:?}",
                dest(&merged, wid(2))
            );
            assert!(dest(&merged, wid(1)).unwrap().same_as(shifted(a, v)));
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
            let up = CGPoint::new(0.0, -DISPLAY.size.height);
            let current = flight_of(&[
                (wid(1), a, shifted(a, up), false),
                (wid(2), b, shifted(b, up), false),
                (wid(3), c, shifted(c, up), false),
            ]);
            let group = key_of(&current, wid(1)).unwrap();
            let past_right =
                rect(DISPLAY.size.width + 1.0, b.origin.y, b.size.width, b.size.height);
            let past_left = rect(-c.size.width - 1.0, c.origin.y, c.size.width, c.size.height);
            let pass = reflow_plan(
                &[
                    (wid(1), a, shifted(a, up), false),
                    (wid(2), b, past_right, false),
                    (wid(3), c, past_left, false),
                ],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, DISPLAY);
            assert!(delta.is_empty(), "{delta:?}");
            assert_eq!(merged, current);
            for w in [wid(1), wid(2), wid(3)] {
                assert_eq!(key_of(&merged, w), Some(group));
            }

            // A still container lends no motion: the member leaves on its own vector.
            let still = flight_of(&[(wid(1), a, a, false), (wid(2), b, b, false)]);
            let pass =
                reflow_plan(&[(wid(1), a, a, false), (wid(2), b, past_right, false)], DISPLAY);
            let (merged, delta) =
                merge_plans(&still, &pass, None, &midway(&still).into(), None, DISPLAY);
            assert_eq!(delta.reparented.len(), 1, "{delta:?}");
            assert!(dest(&merged, wid(2)).unwrap().same_as(past_right));
        }

        #[test]
        fn a_rigid_member_turning_into_a_resize_goes_loose() {
            let (a, b) = (column(0.0), column(1.0));
            let v = CGPoint::new(-300.0, 0.0);
            let current = flight_of(&[
                (wid(1), a, shifted(a, v), false),
                (wid(2), b, shifted(b, v), false),
            ]);
            let group = current.groups[1].key;
            let presented = midway(&current);
            let grown = rect(b.origin.x + v.x, b.origin.y, b.size.width + 400.0, b.size.height);
            let pass = reflow_plan(&[(wid(2), shifted(b, v), grown, false)], DISPLAY);
            let (merged, delta) =
                merge_plans(&current, &pass, None, &presented.clone().into(), None, DISPLAY);
            assert_eq!(delta.reparented, vec![(wid(2), group, GroupKey::Loose)]);
            assert_eq!(delta.retargeted_tiles, vec![(wid(2), grown)]);
            let Some(Member::Changing { from, to }) = merged.member(wid(2)) else {
                panic!("loose")
            };
            assert_eq!(
                from,
                overlay_of(b, presented[&group]),
                "leaves at the presented frame"
            );
            assert_eq!(to, grown);
            assert_eq!(key_of(&merged, wid(1)), Some(group));
        }

        /// `rel` is taken from the container's presented position.
        #[test]
        fn a_join_matching_a_groups_remaining_travel_joins_it() {
            let (a, b) = (column(0.0), column(1.0));
            let v = CGPoint::new(-400.0, 0.0);
            let current = flight_of(&[(wid(1), a, shifted(a, v), false)]);
            let group = current.groups[1].key;
            let presented = midway(&current);
            let remaining = CGPoint::new(v.x - presented[&group].x, 0.0);
            let pass = reflow_plan(&[(wid(2), b, shifted(b, remaining), false)], DISPLAY);
            let (merged, delta) =
                merge_plans(&current, &pass, None, &presented.clone().into(), None, DISPLAY);
            assert_eq!(delta.joined_tiles, vec![(wid(2), group)]);
            assert!(delta.new_groups.is_empty());
            let Some(Member::Rigid { key, rel }) = merged.member(wid(2)) else {
                panic!("rigid")
            };
            assert_eq!(key, group);
            assert_eq!(rel, group_relative(b, presented[&group]));
            assert!(dest(&merged, wid(2)).unwrap().same_as(shifted(b, remaining)));
        }

        #[test]
        fn a_join_with_a_new_vector_opens_a_group() {
            let (a, b) = (column(0.0), column(1.0));
            let current = flight_of(&[(wid(1), a, shifted(a, CGPoint::new(-400.0, 0.0)), false)]);
            let pass = reflow_plan(
                &[(wid(2), b, shifted(b, CGPoint::new(120.0, 0.0)), false)],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, DISPLAY);
            assert!(delta.joined_tiles.is_empty());
            assert_eq!(delta.new_groups.len(), 1);
            let NewGroup { key, install, .. } = delta.new_groups[0];
            assert_eq!(install, CGPoint::new(0.0, 0.0));
            assert_eq!(key_of(&merged, wid(2)), Some(key));
            assert_eq!(merged.position_of(key), CGPoint::new(120.0, 0.0));
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
            let push = CGPoint::new(859.0, 0.0);
            // The open: columns from index 1 shift right by the newcomer's width.
            let requests: Vec<(WindowId, CGRect, CGRect, bool)> = (0..22)
                .map(|i| {
                    let f = col(i as f64);
                    let to = if i == 0 { f } else { shifted(f, push) };
                    (wid(i + 1), f, to, false)
                })
                .collect();
            let slot = col(1.0);
            let mut current = FlightPlan::from(reflow_plan(&requests, EXTERNAL));
            let slot_o = to_overlay_space(slot, EXTERNAL);
            current.entrances.push((wid(100), entrance_from(slot_o), slot_o));

            let d = CGPoint::new(-574.0, 0.0);
            let windows: Vec<OnSurface> = requests
                .iter()
                .map(|(w, _, to, _)| OnSurface {
                    window: *w,
                    frame: to_overlay_space(*to, EXTERNAL),
                    pinned: false,
                    floating: false,
                    from: None,
                })
                .collect();
            let pan =
                surface_plan(&windows, CGPoint::new(-574.0, 0.0), CGPoint::new(0.0, 0.0));
            let (merged, delta) =
                merge_plans(&current, &pan, Some(d), &midway(&current).into(), None, DISPLAY);

            for (w, _, to, _) in &requests {
                let expected = shifted(to_overlay_space(*to, EXTERNAL), d);
                assert!(
                    dest(&merged, *w).unwrap().same_as(expected),
                    "{w:?}: {:?} vs {expected:?}",
                    dest(&merged, *w)
                );
            }
            assert_eq!(merged.entrances[0].2, shifted(slot_o, d));
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
            let v = CGPoint::new(-300.0, 0.0);
            let current = flight_of(&[
                (wid(1), a, shifted(a, v), false),
                (wid(2), b, shifted(b, v), false),
                (wid(3), c, shifted(c, v), false),
            ]);
            let group = current.groups[1].key;
            // The pass composes 1 and 3 with a further 100pt; 2 has no picture.
            let further = CGPoint::new(-400.0, 0.0);
            let pass = reflow_plan(
                &[
                    (wid(1), a, shifted(a, further), false),
                    (wid(3), c, shifted(c, further), false),
                ],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, DISPLAY);
            assert_eq!(delta.retargeted_groups, vec![(group, further)]);
            assert!(delta.reparented.is_empty() && delta.joined_tiles.is_empty());
            assert!(
                dest(&merged, wid(2)).unwrap().same_as(shifted(b, further)),
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
                    let mut pan: Option<CGPoint> = None;
                    let incoming: ReflowPlan = match rng.below(4) {
                        // A pan.
                        0 => {
                            let mut d = CGPoint::new(rng.pt(-9.0, 9.0) * 100.0, 0.0);
                            if is_zero(d) {
                                d.x = 100.0;
                            }
                            pan = Some(d);
                            let mut p = ReflowPlan::empty();
                            for w in &windows {
                                let Some(from) = dest(&plan, *w) else { continue };
                                match plan.member(*w) {
                                    Some(Member::Rigid { .. }) => {
                                        p.adopt(&StagedTile {
                                            window: *w,
                                            from,
                                            to: shifted(from, d),
                                            floating: false,
                                            companion: None,
                                        });
                                    }
                                    _ => {}
                                }
                            }
                            p
                        }
                        // A layout pass over a subset with a fresh vector or two.
                        1 | 2 => {
                            let v1 = CGPoint::new(rng.pt(-9.0, 9.0) * 50.0, 0.0);
                            let v2 = CGPoint::new(rng.pt(-9.0, 9.0) * 50.0, 0.0);
                            let mut reqs: Vec<(WindowId, CGRect, CGRect, bool)> = Vec::new();
                            for w in &windows {
                                if rng.coin() {
                                    continue;
                                }
                                let Some(Member::Rigid { key, rel }) = plan.member(*w) else {
                                    continue;
                                };
                                let from = overlay_of(rel, presented[&key]);
                                let v = if rng.coin() { v1 } else { v2 };
                                reqs.push((*w, from, shifted(from, v), false));
                            }
                            // Sometimes a newcomer.
                            if rng.coin() {
                                let from = rng.on_screen();
                                reqs.push((wid(500 + step as u32), from, shifted(from, v1), false));
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
                            let mut reqs: Vec<(WindowId, CGRect, CGRect, bool)> = Vec::new();
                            if let Some(w) = rigid.first() {
                                let Some(Member::Rigid { key, rel }) = plan.member(*w) else {
                                    unreachable!()
                                };
                                let from = overlay_of(rel, presented[&key]);
                                let to = rect(
                                    from.origin.x,
                                    from.origin.y,
                                    from.size.width + 300.0,
                                    from.size.height,
                                );
                                reqs.push((*w, from, to, false));
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
                        DISPLAY,
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
                            Member::Rigid { key, rel } => overlay_of(
                                rel,
                                incoming.groups.iter().find(|g| g.key == key).unwrap().travel,
                            ),
                            Member::Changing { to, .. }
                            | Member::Entrance { to, .. }
                            | Member::Floating { to, .. } => to,
                        };
                        let got = dest(&merged, w)
                            .unwrap_or_else(|| panic!("{tag}: {w:?} unnamed after merge"));
                        // The one exception: a member sent off the viewport rides its moving container (`rides_out`).
                        let rode_out = pan.is_none()
                            && rini_geometry::is_off_screen(DISPLAY, want)
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
                            is_zero(d) || before.groups.iter().all(|g| g.members.is_empty()),
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
                let mut reqs: Vec<(WindowId, CGRect, CGRect, bool)> = Vec::new();
                for &(w, _, _, floating) in &requests {
                    let (from, to) = match current.member(w).unwrap() {
                        Member::Rigid { key, rel } => (
                            overlay_of(rel, presented[&key]),
                            overlay_of(rel, current.position_of(key)),
                        ),
                        Member::Changing { from, to } | Member::Entrance { from, to } => (from, to),
                        Member::Floating { from, to } => (
                            overlay_of(from, presented[&GroupKey::Floating]),
                            overlay_of(to, current.position_of(GroupKey::Floating)),
                        ),
                    };
                    reqs.push((w, from, to, floating));
                }
                let same = reflow_plan(&reqs, DISPLAY);
                let (merged, delta) =
                    merge_plans(&current, &same, None, &presented.clone().into(), None, DISPLAY);
                assert!(delta.is_empty(), "seed 151 run {run}: {delta:?}");
                assert_eq!(merged, current, "seed 151 run {run}");
            }
        }

        /// Where a container is drawn includes the nudge riding it, and a member leaving it mid-nudge
        /// leaves from there, carrying on its container's motion: continuous, and still landing on
        /// its destination.
        #[test]
        fn a_member_leaving_a_nudged_container_leaves_from_where_it_is_drawn() {
            let (a, b) = (rect(4.0, 32.0, 860.0, 1081.0), rect(868.0, 32.0, 860.0, 1081.0));
            let v = CGPoint::new(-300.0, 0.0);
            let current = FlightPlan::from(reflow_plan(
                &[
                    (wid(1), a, rect(a.origin.x + v.x, 32.0, 860.0, 1081.0), false),
                    (wid(2), b, rect(b.origin.x + v.x, 32.0, 860.0, 1081.0), false),
                ],
                DISPLAY,
            ));
            let group = key_of(&current, wid(2)).unwrap();
            let nudge = CGPoint::new(576.0, 0.0);
            let along = CGPoint::new(v.x / 2.0, 0.0);
            let drawn = CGPoint::new(along.x + nudge.x, 0.0);
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
                    (wid(1), a, rect(a.origin.x + v.x, 32.0, 860.0, 1081.0), false),
                    (wid(2), b, elsewhere, false),
                ],
                DISPLAY,
            );
            let (merged, delta) = merge_plans(&current, &pass, None, &presented, None, DISPLAY);
            let NewGroup { install, leaving, .. } = delta.new_groups[0];
            assert_eq!(leaving, Some(group));
            let key = key_of(&merged, wid(2)).unwrap();
            assert_ne!(key, group);
            assert_eq!(overlay_of(rel(&merged, wid(2)), install), before, "no jump");
            assert!(dest(&merged, wid(2)).unwrap().same_as(elsewhere));
        }
    }

    /// A pass as rigid pieces: `reflow_plan`, `surface_plan`, `plan_from_tiles` and what
    /// `animation_targets` names. See "Layout changes" and "Strip movements" in
    /// `src/animation/docs/animation-smoothness.md`.
    mod pieces {
        use rini_geometry::{SameAs, to_overlay_space};

        use super::*;
        use crate::animation::domain::motion::surface::{pan_travel, surface_travel};
        use crate::animation::domain::motion::testing::{DISPLAY, Gen, OnSurface, RUNS, StagedTile};

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
        }

        fn shifted(frame: CGRect, dx: f64, dy: f64) -> CGRect {
            CGRect::new(
                CGPoint::new(frame.origin.x + dx, frame.origin.y + dy),
                frame.size,
            )
        }

        fn column(i: f64) -> CGRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        fn moving(plan: &ReflowPlan) -> Vec<&RigidGroup> {
            plan.groups
                .iter()
                .filter(|g| !g.members.is_empty() && g.key != GroupKey::STILL)
                .collect()
        }

        fn members(group: &RigidGroup) -> Vec<WindowId> {
            group.members.iter().map(|m| m.window).collect()
        }

        fn strip_window(idx: u32, frame: CGRect, pinned: bool, floating: bool) -> OnSurface {
            OnSurface {
                window: wid(idx),
                frame,
                pinned,
                floating,
                from: None,
            }
        }

        /// A random pass: 1-12 windows over 1-4 distinct vectors (4pt apart on some axis, and from zero)
        /// plus ±1pt jitter; some still, resizing, or floating.
        pub(super) fn random_requests(rng: &mut Gen) -> Vec<(WindowId, CGRect, CGRect, bool)> {
            let mut palette: Vec<CGPoint> = Vec::new();
            let wanted = 1 + rng.below(4) as usize;
            while palette.len() < wanted {
                let v = CGPoint::new(
                    rng.pt(-12.0, 12.0) * 50.0,
                    if rng.coin() {
                        0.0
                    } else {
                        rng.pt(-6.0, 6.0) * 50.0
                    },
                );
                if (v.x == 0.0 && v.y == 0.0) || palette.contains(&v) {
                    continue;
                }
                palette.push(v);
            }
            let count = 1 + rng.below(12) as usize;
            (0..count)
                .map(|i| {
                    let from = rng.on_screen();
                    let window = wid(i as u32 + 1);
                    match rng.below(10) {
                        0 => (window, from, from, false),
                        1 => {
                            let to = rect(
                                from.origin.x,
                                from.origin.y,
                                from.size.width + rng.pt(-300.0, 300.0),
                                from.size.height,
                            );
                            let to = if is_a_resize(from.size, to.size) {
                                to
                            } else {
                                rect(
                                    to.origin.x,
                                    to.origin.y,
                                    from.size.width + 200.0,
                                    to.size.height,
                                )
                            };
                            (window, from, to, false)
                        }
                        2 => {
                            let v = palette[rng.below(palette.len() as u64) as usize];
                            (window, from, shifted(from, v.x, v.y), true)
                        }
                        _ => {
                            let v = palette[rng.below(palette.len() as u64) as usize];
                            let (jx, jy) = (rng.pt(-1.0, 1.0), rng.pt(-1.0, 1.0));
                            (window, from, shifted(from, v.x + jx, v.y + jy), false)
                        }
                    }
                })
                .collect()
        }

        #[test]
        fn two_columns_shifting_by_the_same_vector_are_one_group() {
            let a = column(0.0);
            let b = column(1.0);
            let plan = reflow_plan(
                &[
                    (wid(1), a, shifted(a, -859.0, 0.0), false),
                    (wid(2), b, shifted(b, -859.0, 0.0), false),
                ],
                DISPLAY,
            );
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].travel, CGPoint::new(-859.0, 0.0));
            assert_eq!(members(groups[0]), vec![wid(1), wid(2)]);
            assert_eq!(
                groups[0].members[0].rel, a,
                "a container installs at (0,0), so rel is from"
            );
            assert!(plan.groups[0].members.is_empty(), "nothing stands still");
            assert!(
                plan.changing.is_empty() && plan.floating.is_empty() && plan.entrances.is_empty()
            );
        }

        #[test]
        fn a_column_two_points_off_shares_the_group_and_three_points_off_opens_one() {
            let a = column(0.0);
            let b = column(1.0);
            let same = reflow_plan(
                &[
                    (wid(1), a, shifted(a, 300.0, 0.0), false),
                    (wid(2), b, shifted(b, 302.0, 0.0), false),
                ],
                DISPLAY,
            );
            assert_eq!(moving(&same).len(), 1, "2pt apart: one group");
            assert_eq!(members(moving(&same)[0]), vec![wid(1), wid(2)]);

            let split = reflow_plan(
                &[
                    (wid(1), a, shifted(a, 300.0, 0.0), false),
                    (wid(2), b, shifted(b, 303.0, 0.0), false),
                ],
                DISPLAY,
            );
            let groups = moving(&split);
            assert_eq!(groups.len(), 2, "3pt apart: two groups");
            assert_eq!(members(groups[0]), vec![wid(1)]);
            assert_eq!(members(groups[1]), vec![wid(2)]);
            assert_eq!(groups[1].travel, CGPoint::new(303.0, 0.0));
            assert_eq!(groups[0].key, GroupKey::Rigid(1));
            assert_eq!(groups[1].key, GroupKey::Rigid(2));
        }

        #[test]
        fn a_resizing_column_is_changing_and_in_no_group() {
            let a = column(0.0);
            let grown = rect(a.origin.x, a.origin.y, a.size.width + 400.0, a.size.height);
            let plan = reflow_plan(&[(wid(1), a, grown, false)], DISPLAY);
            assert_eq!(plan.changing, vec![(wid(1), a, grown)]);
            assert!(plan.group_of(wid(1)).is_none());
            assert!(moving(&plan).is_empty());
            assert_eq!(
                plan.member(wid(1)),
                Some(Member::Changing { from: a, to: grown })
            );
        }

        #[test]
        fn a_floating_window_is_floating_and_never_grouped() {
            let a = column(0.0);
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let plan = reflow_plan(
                &[
                    (wid(1), a, shifted(a, -100.0, 0.0), false),
                    (wid(2), settings, shifted(settings, -100.0, 0.0), true),
                    (wid(3), settings, settings, true),
                ],
                DISPLAY,
            );
            assert_eq!(
                members(moving(&plan)[0]),
                vec![wid(1)],
                "the same vector does not pull it in"
            );
            assert_eq!(
                plan.floating,
                vec![
                    (wid(2), settings, shifted(settings, -100.0, 0.0)),
                    (wid(3), settings, settings)
                ]
            );
            assert!(plan.group_of(wid(2)).is_none() && plan.group_of(wid(3)).is_none());
            assert_eq!(
                plan.floating_travel,
                CGPoint::new(0.0, 0.0),
                "a layout pass never moves the container"
            );
        }

        #[test]
        fn the_still_window_is_in_the_still_group() {
            let a = column(0.0);
            let b = column(1.0);
            let plan = reflow_plan(
                &[
                    (wid(1), a, a, false),
                    (wid(2), b, shifted(b, 40.0, 0.0), false),
                ],
                DISPLAY,
            );
            assert_eq!(plan.groups[0].key, GroupKey::STILL);
            assert_eq!(plan.groups[0].travel, CGPoint::new(0.0, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![wid(1)]);
            assert_eq!(
                plan.member(wid(1)),
                Some(Member::Rigid { key: GroupKey::STILL, rel: a })
            );
            assert_eq!(members(moving(&plan)[0]), vec![wid(2)]);
        }

        /// The 3:27:20 pan: 22 survivors scrolled 574pt. One group, 22 members, one travel.
        #[test]
        fn the_3_27_20_pan_is_one_group_of_twenty_two() {
            let windows: Vec<OnSurface> =
                (0..22).map(|i| strip_window(i + 1, column(i as f64), false, false)).collect();
            let from_offset = CGPoint::new(-574.0, 0.0);
            let to_offset = CGPoint::new(0.0, 0.0);
            let plan = surface_plan(&windows, from_offset, to_offset);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].members.len(), 22);
            assert_eq!(groups[0].travel, CGPoint::new(-574.0, 0.0));
            assert_eq!(groups[0].travel, pan_travel(from_offset, to_offset));
            for (window, member) in windows.iter().zip(&groups[0].members) {
                let (from, to) = surface_travel(window.frame, from_offset, to_offset, false);
                assert_eq!(member.window, window.window);
                assert_eq!(member.rel, from);
                assert_eq!(
                    overlay_of(member.rel, groups[0].travel),
                    to,
                    "rel plus travel is the destination"
                );
            }
            assert!(plan.groups[0].members.is_empty());
            assert!(plan.floating.is_empty() && plan.changing.is_empty());
            assert_eq!(plan.floating_travel, CGPoint::new(0.0, 0.0));
        }

        #[test]
        fn pinned_windows_are_floating_with_zero_travel() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let windows = vec![
                strip_window(1, column(0.0), false, false),
                strip_window(2, settings, true, true),
            ];
            let plan =
                surface_plan(&windows, CGPoint::new(-574.0, 0.0), CGPoint::new(0.0, 0.0));
            assert_eq!(plan.floating, vec![(wid(2), settings, settings)]);
            assert_eq!(plan.floating_travel, CGPoint::new(0.0, 0.0));
            assert_eq!(
                plan.member(wid(2)),
                Some(Member::Floating { from: settings, to: settings })
            );
            assert_eq!(members(moving(&plan)[0]), vec![wid(1)]);
        }

        #[test]
        fn a_switch_moves_the_floating_container_by_the_surface_travel() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let windows = vec![
                strip_window(1, column(0.0), false, false),
                strip_window(2, settings, false, true),
            ];
            let from_offset = CGPoint::new(0.0, 0.0);
            let to_offset = CGPoint::new(0.0, 1117.0);
            let plan = surface_plan(&windows, from_offset, to_offset);
            let travel = pan_travel(from_offset, to_offset);
            assert_eq!(travel, CGPoint::new(0.0, -1117.0));
            assert_eq!(moving(&plan)[0].travel, travel);
            assert_eq!(plan.floating_travel, travel);
            assert_eq!(
                plan.floating,
                vec![(wid(2), settings, settings)],
                "the tile itself stands in its container"
            );
            let (_, to) = surface_travel(settings, from_offset, to_offset, false);
            assert_eq!(overlay_of(settings, plan.floating_travel), to);
        }

        #[test]
        fn group_relative_and_overlay_of_are_inverses() {
            let f = rect(867.0, 32.0, 859.0, 1081.0);
            let p = CGPoint::new(-574.0, 12.0);
            assert_eq!(overlay_of(group_relative(f, p), p), f);
            assert_eq!(group_relative(f, p), rect(1441.0, 20.0, 859.0, 1081.0));
            assert_eq!(
                group_relative(f, CGPoint::new(0.0, 0.0)),
                f,
                "at install rel is from"
            );
            let mut rng = Gen(3);
            for _ in 0..RUNS {
                let f = rng.on_screen();
                let p = CGPoint::new(rng.pt(-2000.0, 2000.0), rng.pt(-2000.0, 2000.0));
                assert_eq!(overlay_of(group_relative(f, p), p), f);
                assert_eq!(group_relative(f, p).size, f.size);
            }
        }

        #[test]
        fn a_fresh_flight_plan_positions_every_container_at_its_travel() {
            let a = column(0.0);
            let plan = reflow_plan(
                &[
                    (wid(1), a, shifted(a, -100.0, 0.0), false),
                    (wid(2), a, a, false),
                ],
                DISPLAY,
            );
            let flight = FlightPlan::from(plan.clone());
            assert_eq!(flight.groups, plan.groups);
            assert_eq!(flight.positions[&GroupKey::STILL], CGPoint::new(0.0, 0.0));
            assert_eq!(flight.positions[&GroupKey::Rigid(1)], CGPoint::new(-100.0, 0.0));
            assert_eq!(flight.positions[&GroupKey::Loose], CGPoint::new(0.0, 0.0));
            assert_eq!(flight.positions[&GroupKey::Floating], CGPoint::new(0.0, 0.0));
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
                let mut asked: Vec<WindowId> = requests.iter().map(|r| r.0).collect();
                asked.sort();
                assert_eq!(named, asked, "{tag}: partition");
                assert!(plan.entrances.is_empty(), "{tag}");

                assert_eq!(plan.groups[0].key, GroupKey::STILL, "{tag}");
                assert_eq!(plan.groups[0].travel, CGPoint::new(0.0, 0.0), "{tag}");
                for (i, group) in plan.groups.iter().enumerate() {
                    assert_eq!(group.key, GroupKey::Rigid(i as u16), "{tag}: keys in plan order");
                    assert!(
                        i == 0 || !group.members.is_empty(),
                        "{tag}: an empty moving group"
                    );
                }

                for &(window, start, end, floating) in &requests {
                    let from = to_overlay_space(start, display);
                    let to = to_overlay_space(end, display);
                    let v = CGPoint::new(to.origin.x - from.origin.x, to.origin.y - from.origin.y);
                    match plan.member(window).unwrap_or_else(|| panic!("{tag}: {window:?} unnamed"))
                    {
                        Member::Rigid { key, rel } => {
                            assert!(!floating, "{tag}: a floating window grouped");
                            assert!(!is_a_resize(from.size, to.size), "{tag}: a resize grouped");
                            assert_eq!(rel, from, "{tag}");
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
                            assert_eq!((f, t), (from, to), "{tag}");
                        }
                        Member::Floating { from: f, to: t } => {
                            assert!(floating, "{tag}");
                            assert_eq!((f, t), (from, to), "{tag}");
                        }
                        Member::Entrance { .. } => panic!("{tag}: no entrance was asked for"),
                    }
                }

                let vector_of = |window: WindowId| {
                    let r = requests.iter().find(|r| r.0 == window).unwrap();
                    CGPoint::new(r.2.origin.x - r.1.origin.x, r.2.origin.y - r.1.origin.y)
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
                    (wid(1), left, left, false),
                    (wid(2), neighbour, shifted(neighbour, 859.0, 0.0), false),
                ],
                DISPLAY,
            );
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![wid(2)]);
            assert_eq!(groups[0].travel, CGPoint::new(859.0, 0.0));
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
                    (wid(1), left, left, false),
                    (wid(2), middle, grown, false),
                    (wid(3), right, shifted(right, dw, 0.0), false),
                ],
                DISPLAY,
            );
            assert_eq!(plan.changing, vec![(wid(2), middle, grown)]);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![wid(3)]);
            assert_eq!(groups[0].travel, CGPoint::new(dw, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![wid(1)]);
        }

        #[test]
        fn a_close_shifts_every_survivor_as_one_group() {
            let w = 863.0;
            let survivors: Vec<(WindowId, CGRect, CGRect, bool)> = (1..=4)
                .map(|i| {
                    let f = column(i as f64);
                    (wid(i as u32), f, shifted(f, -w, 0.0), false)
                })
                .collect();
            let plan = reflow_plan(&survivors, DISPLAY);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), (1..=4).map(wid).collect::<Vec<_>>());
            assert_eq!(groups[0].travel, CGPoint::new(-w, 0.0));
            assert!(plan.groups[0].members.is_empty());
        }

        #[test]
        fn a_floating_only_pass_has_no_groups_and_one_floating_member() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let plan = reflow_plan(
                &[(wid(1), settings, shifted(settings, 40.0, 20.0), true)],
                DISPLAY,
            );
            assert!(moving(&plan).is_empty());
            assert!(plan.groups[0].members.is_empty());
            assert_eq!(
                plan.floating,
                vec![(wid(1), settings, shifted(settings, 40.0, 20.0))]
            );
            let flight = FlightPlan::from(plan);
            let targets = animation_targets(&flight);
            assert_eq!(targets.len(), 1, "the floating tile flies on its own");
        }

        #[test]
        fn a_plan_rebuilt_from_tiles_matches_the_plan_from_requests() {
            let mut rng = Gen(139);
            for run in 0..RUNS {
                let requests = random_requests(&mut rng);
                let expected = reflow_plan(&requests, DISPLAY);
                let tiles: Vec<StagedTile> = requests
                    .iter()
                    .map(|&(window, start, end, floating)| StagedTile {
                        window,
                        from: to_overlay_space(start, DISPLAY),
                        to: to_overlay_space(end, DISPLAY),
                        floating,
                        companion: None,
                    })
                    .collect();
                let rebuilt = plan_from_tiles(&tiles);
                assert_eq!(rebuilt, expected, "seed 139 run {run}");
            }
        }

        /// `fly` installs what `animation_targets` names and nothing else.
        #[test]
        fn animation_targets_name_every_moving_piece_once_and_no_rigid_member() {
            let mut rng = Gen(137);
            for run in 0..RUNS {
                let requests = random_requests(&mut rng);
                let mut plan = reflow_plan(&requests, DISPLAY);
                // A switch now and then: the floating container itself travels.
                let switch = run % 5 == 0 && !plan.floating.is_empty();
                if switch {
                    plan.floating_travel = CGPoint::new(0.0, -DISPLAY.size.height);
                    for (_, from, to) in plan.floating.iter_mut() {
                        *to = *from;
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
                                CGPoint::new(0.0, 0.0),
                                "{tag}: a fresh flight installs at the origin"
                            );
                            assert_eq!(to, travel, "{tag}");
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
                    .map(|(w, _, _)| *w)
                    .chain(
                        plan.floating.iter().filter(|(_, f, t)| !f.same_as(*t)).map(|(w, _, _)| *w),
                    )
                    .collect();
                expected.sort();
                tiles.sort();
                assert_eq!(tiles, expected, "{tag}: tile targets");
            }
        }
    }
}
