//! A pass as rigid pieces: the pure half of the container model.
//! See "The overlay engine" and "Layout changes" in `docs/animation-smoothness.md`.


use std::collections::HashMap;

use objc2_core_foundation::{CGPoint, CGRect};

use super::{StripWindow, strip_pan_travel, strip_travel, to_overlay_space};
use rini_core::ids::WindowId;
use rini_core::geometry::SameAs;
use crate::ui::window_snapshot::is_a_resize;
use crate::ui::workspace_overlay::OverlayTile;

/// Two translation vectors this close on both axes ride one container.
pub(crate) const GROUP_TOLERANCE: f64 = 2.0;

/// Which container a member lives in. Stable for the flight; a reparent changes a member's key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum GroupKey {
    /// A rigid strip piece. `0` is the still group; others are allocated in plan order.
    Strip(u16),
    /// Changing members and entrances: strip band, per-tile animations.
    StripLoose,
    Floating,
}

impl GroupKey {
    pub(crate) const STILL: GroupKey = GroupKey::Strip(0);
}

/// One rigid piece of the strip.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StripGroup {
    pub(crate) key: GroupKey,
    /// Total displacement of the container from install to the current destination.
    pub(crate) travel: CGPoint,
    /// Members with their group-relative frames (constant for the flight unless reparented).
    pub(crate) members: Vec<GroupMember>,
}

impl StripGroup {
    fn new(key: GroupKey, travel: CGPoint) -> Self {
        StripGroup { key, travel, members: Vec::new() }
    }

    pub(crate) fn is_still(&self) -> bool {
        self.travel.x == 0.0 && self.travel.y == 0.0
    }
}

/// A tile riding a container. The picture itself stays in `RunningAnimation.tiles`, found by window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GroupMember {
    pub(crate) window: WindowId,
    /// Frame inside the container: overlay-space `from` minus the container position at install.
    pub(crate) rel: CGRect,
    /// A border window riding the window it traces; drawn a quarter step in front of it.
    pub(crate) companion: bool,
}

/// How one window takes part in a plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Member {
    /// Rigid: rides its container. No per-tile animation.
    Rigid { key: GroupKey, rel: CGRect },
    /// Its size changes: own layer under `StripLoose`, animated as a resize from `from` to `to`.
    Changing { from: CGRect, to: CGRect },
    /// New window: own layer under `StripLoose`, from spawn frame to slot, stretched until reveal.
    Entrance { from: CGRect, to: CGRect },
    /// In the floating container; animated per tile only when `from != to`.
    Floating { from: CGRect, to: CGRect },
}

/// A pass, described as rigid pieces. Pure output of [`reflow_plan`] / [`strip_plan`].
/// Every rect is in overlay space.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReflowPlan {
    /// `groups[0]` is the still group, always present, possibly empty.
    pub(crate) groups: Vec<StripGroup>,
    pub(crate) changing: Vec<(WindowId, CGRect, CGRect)>,
    pub(crate) entrances: Vec<(WindowId, CGRect, CGRect)>,
    pub(crate) floating: Vec<(WindowId, CGRect, CGRect)>,
    /// Travel the floating container itself takes (a switch); zero for a pan or a layout pass.
    pub(crate) floating_travel: CGPoint,
}

impl ReflowPlan {
    pub(crate) fn empty() -> Self {
        ReflowPlan {
            groups: vec![StripGroup::new(GroupKey::STILL, CGPoint::new(0.0, 0.0))],
            changing: Vec::new(),
            entrances: Vec::new(),
            floating: Vec::new(),
            floating_travel: CGPoint::new(0.0, 0.0),
        }
    }

    /// Puts a rigid member starting at `from` (overlay space) into the group travelling by `vector`,
    /// opening one when none is within `GROUP_TOLERANCE`. Containers install at `(0,0)`, so `rel == from`.
    fn place(&mut self, window: WindowId, from: CGRect, vector: CGPoint) -> GroupKey {
        let member = GroupMember { window, rel: from, companion: false };
        if let Some(group) = self.groups.iter_mut().find(|g| same_vector(g.travel, vector)) {
            group.members.push(member);
            return group.key;
        }
        let key = GroupKey::Strip(self.groups.len() as u16);
        let mut group = StripGroup::new(key, vector);
        group.members.push(member);
        self.groups.push(group);
        key
    }

    /// Adds a tile the plan did not derive itself (a border companion) by the same
    /// rules: floating stays loose, a resize is `changing`, anything else rides the group with its vector.
    pub(crate) fn adopt(&mut self, tile: &OverlayTile) {
        if tile.floating {
            self.floating.push((tile.window, tile.from, tile.to));
        } else if is_a_resize(tile.from.size, tile.to.size) {
            self.changing.push((tile.window, tile.from, tile.to));
        } else {
            let key = self.place(tile.window, tile.from, vector_of(tile.from, tile.to));
            if tile.companion
                && let Some(group) = self.groups.iter_mut().find(|g| g.key == key)
                && let Some(member) = group.members.iter_mut().find(|m| m.window == tile.window)
            {
                member.companion = true;
            }
        }
    }

    /// Moves the named windows from `changing` to `entrances`: a rebuilt plan sees only a resize.
    pub(crate) fn mark_entrances(&mut self, windows: &[WindowId]) {
        let (entrances, changing): (Vec<_>, Vec<_>) =
            self.changing.drain(..).partition(|(w, _, _)| windows.contains(w));
        self.changing = changing;
        self.entrances.extend(entrances);
    }

    /// The group `window` rides, if it is a rigid member.
    #[cfg(test)]
    pub(crate) fn group_of(&self, window: WindowId) -> Option<&StripGroup> {
        self.groups.iter().find(|g| g.members.iter().any(|m| m.window == window))
    }

    /// How `window` takes part, or `None` when the plan does not name it.
    pub(crate) fn member(&self, window: WindowId) -> Option<Member> {
        member_in(&self.groups, &self.changing, &self.entrances, &self.floating, window)
    }

    /// Every window the plan names, in plan order: groups, changing, entrances, floating.
    #[cfg(test)]
    pub(crate) fn windows(&self) -> Vec<WindowId> {
        windows_in(&self.groups, &self.changing, &self.entrances, &self.floating)
    }
}

/// How `window` takes part in a plan's lists, or `None` when none names it.
fn member_in(
    groups: &[StripGroup],
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
#[cfg(test)]
fn windows_in(
    groups: &[StripGroup],
    changing: &[(WindowId, CGRect, CGRect)],
    entrances: &[(WindowId, CGRect, CGRect)],
    floating: &[(WindowId, CGRect, CGRect)],
) -> Vec<WindowId> {
    let mut out: Vec<WindowId> = groups
        .iter()
        .flat_map(|g| g.members.iter().filter(|m| !m.companion).map(|m| m.window))
        .collect();
    out.extend(changing.iter().map(|(w, _, _)| *w));
    out.extend(entrances.iter().map(|(w, _, _)| *w));
    out.extend(floating.iter().map(|(w, _, _)| *w));
    out
}

/// The running flight's plan. Adds container state the overlay needs to retarget.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FlightPlan {
    pub(crate) groups: Vec<StripGroup>,
    /// Model position of each container: install position plus every retarget's travel.
    pub(crate) positions: HashMap<GroupKey, CGPoint>,
    pub(crate) changing: Vec<(WindowId, CGRect, CGRect)>,
    pub(crate) entrances: Vec<(WindowId, CGRect, CGRect)>,
    pub(crate) floating: Vec<(WindowId, CGRect, CGRect)>,
    pub(crate) floating_travel: CGPoint,
    pub(crate) next_key: u16,
}

impl FlightPlan {
    /// A flight with nothing in it: the still group only.
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        FlightPlan::from(ReflowPlan::empty())
    }

    /// How `window` takes part. Floating frames are in the floating container's space.
    pub(crate) fn member(&self, window: WindowId) -> Option<Member> {
        member_in(&self.groups, &self.changing, &self.entrances, &self.floating, window)
    }

    /// Every window the flight names, companions left out.
    #[cfg(test)]
    pub(crate) fn windows(&self) -> Vec<WindowId> {
        windows_in(&self.groups, &self.changing, &self.entrances, &self.floating)
    }
}

impl From<ReflowPlan> for FlightPlan {
    /// A fresh flight: every container installs at `(0,0)`, so each group's model position is its travel.
    fn from(plan: ReflowPlan) -> Self {
        let mut positions: HashMap<GroupKey, CGPoint> =
            plan.groups.iter().map(|g| (g.key, g.travel)).collect();
        positions.insert(GroupKey::StripLoose, CGPoint::new(0.0, 0.0));
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
/// container in front or behind as a whole, strip containers in `strip_order`, each tile at its
/// within-band depth. `container_z - within` is `-tile_depth` (`model/z_group.rs`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Banding {
    pub(crate) floating_in_front: bool,
    /// Depth inside its container per tile; a companion carries its window's.
    pub(crate) within: HashMap<WindowId, usize>,
    /// Strip containers front to back: the one holding focus first, then by shallowest member.
    pub(crate) strip_order: Vec<GroupKey>,
}

/// What one merge changed, for the overlay. Pure output of [`merge_plans`]; every frame the
/// overlay needs is in the merged plan, found by window.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PlanDelta {
    /// Containers to animate from their presented position to `to` (overlay space).
    pub(crate) retargeted_groups: Vec<(GroupKey, CGPoint)>,
    /// New containers with the position they install at; members and destination are in the plan.
    pub(crate) new_groups: Vec<(GroupKey, CGPoint)>,
    /// Members moving container: (window, from_key, to_key). The new frame is in the plan.
    pub(crate) reparented: Vec<(WindowId, GroupKey, GroupKey)>,
    /// Loose tiles to bend toward a new destination, in their container's space.
    pub(crate) retargeted_tiles: Vec<(WindowId, CGRect)>,
    /// Tiles joining the flight, with their container; frames are in the plan.
    pub(crate) joined_tiles: Vec<(WindowId, GroupKey)>,
    pub(crate) focus_changed: bool,
}

impl PlanDelta {
    /// A merge that changed nothing: the redundant pass of a rapid press.
    pub(crate) fn is_empty(&self) -> bool {
        !self.moves_anything() && !self.focus_changed
    }

    /// Whether any geometry changed: what restarts the flight's clock.
    pub(crate) fn moves_anything(&self) -> bool {
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
        let index = |list: &[(WindowId, CGRect, CGRect)]| list.iter().position(|(w, _, _)| *w == window);
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
    pub(crate) fn position_of(&self, key: GroupKey) -> CGPoint {
        self.position(key)
    }

    fn group_mut(&mut self, key: GroupKey) -> &mut StripGroup {
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
        delta.retargeted_groups.push((key, p));
    }

    /// Opens a group installing at `install` and landing at `destination`, with one member.
    fn open_group(
        &mut self,
        install: CGPoint,
        destination: CGPoint,
        member: GroupMember,
        presented: &mut HashMap<GroupKey, CGPoint>,
        delta: &mut PlanDelta,
    ) -> GroupKey {
        let key = GroupKey::Strip(self.next_key);
        self.next_key += 1;
        let travel = CGPoint::new(destination.x - install.x, destination.y - install.y);
        let mut group = StripGroup::new(key, travel);
        group.members.push(member);
        self.groups.push(group);
        self.positions.insert(key, destination);
        presented.insert(key, install);
        delta.new_groups.push((key, install));
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
/// are reparented at presented frames, a pan adds its travel to every group. `presented` is each
/// container's presented position, read by the overlay just before. See "Mid-flight passes" in
/// `docs/animation-smoothness.md`.
pub(crate) fn merge_plans(
    current: &FlightPlan,
    incoming: &ReflowPlan,
    pan: Option<CGPoint>,
    presented: &HashMap<GroupKey, CGPoint>,
    focus: Option<WindowId>,
    viewport: CGRect,
) -> (FlightPlan, PlanDelta) {
    let mut next = current.clone();
    let mut delta = PlanDelta::default();
    let mut presented: HashMap<GroupKey, CGPoint> = presented.clone();
    for (key, p) in &current.positions {
        presented.entry(*key).or_insert(*p);
    }
    let presented_of = |presented: &HashMap<GroupKey, CGPoint>, key: GroupKey| {
        presented.get(&key).copied().unwrap_or(CGPoint::new(0.0, 0.0))
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
        for m in &group.members {
            let to = overlay_of(m.rel, group.travel);
            match next.locate(m.window) {
                Located::Group(key, rel) => {
                    if pan.is_none() && !rides_out(&next, key, to, viewport) {
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
                    let member = GroupMember { window: m.window, rel: m.rel, companion: m.companion };
                    joins.push((member, group.travel));
                }
            }
        }
    }

    // 3. Per voted group: the largest cluster keeps the container; the rest are reparented.
    let mut keys: Vec<GroupKey> = Vec::new();
    for (key, _, _) in &votes {
        if !keys.contains(key) {
            keys.push(*key);
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
        let from_presented = presented_of(&presented, key);
        for (i, (_, losers)) in clusters.iter().enumerate() {
            if i == winner {
                continue;
            }
            for &(window, p) in losers {
                let member = next.take_member(key, window);
                // What the member still has to travel from where it is drawn: a group with the
                // same remaining travel carries it there.
                let remaining = sub(p, from_presented);
                let to_key = match landing_for(&next, &presented, remaining, Some(key)) {
                    Some(to_key) => {
                        let shift = sub(from_presented, presented_of(&presented, to_key));
                        let rel = CGRect::new(add(member.rel.origin, shift), member.rel.size);
                        next.group_mut(to_key).members.push(GroupMember { rel, ..member });
                        to_key
                    }
                    None => next.open_group(from_presented, p, member, &mut presented, &mut delta),
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
                    let at = overlay_of(rel, presented_of(&presented, key));
                    next.changing.push((window, at, to));
                    delta.reparented.push((window, key, GroupKey::StripLoose));
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
                    delta.joined_tiles.push((window, GroupKey::StripLoose));
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
                let at = overlay_of(rel, presented_of(&presented, key));
                let from_rel = group_relative(at, presented_of(&presented, GroupKey::Floating));
                next.floating.push((window, from_rel, to_rel));
                delta.reparented.push((window, key, GroupKey::Floating));
                delta.retargeted_tiles.push((window, to_rel));
            }
            Located::Changing(i) => retarget_loose(&mut next.changing, i, to, &mut delta),
            Located::Entrance(i) => retarget_loose(&mut next.entrances, i, to, &mut delta),
            Located::Absent => {
                let from_rel = group_relative(from, presented_of(&presented, GroupKey::Floating));
                next.floating.push((window, from_rel, to_rel));
                delta.joined_tiles.push((window, GroupKey::Floating));
            }
        }
    }

    (next, delta)
}

/// A rigid member the pass sends off the viewport while its container is moving keeps riding
/// the container. It has no visible destination, and voting its own vector opened a group that
/// swept it sideways to the park while the row it sat in travelled vertically (the zig-zag seen
/// on a switch whose layout pass parked the departing row). A still container has no motion to
/// lend, so its member votes and exits on its own.
fn rides_out(next: &FlightPlan, key: GroupKey, to: CGRect, viewport: CGRect) -> bool {
    use crate::model::HiddenWindowPlacement;
    let moving = next.groups.iter().any(|g| g.key == key && !g.is_still());
    moving && HiddenWindowPlacement::is_off_screen(viewport, to)
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

/// The group whose remaining travel (destination less presented position) matches `remaining`.
fn landing_for(
    next: &FlightPlan,
    presented: &HashMap<GroupKey, CGPoint>,
    remaining: CGPoint,
    exclude: Option<GroupKey>,
) -> Option<GroupKey> {
    next.groups
        .iter()
        .filter(|g| Some(g.key) != exclude)
        .find(|g| {
            let p = presented.get(&g.key).copied().unwrap_or(next.position(g.key));
            same_vector(sub(next.position(g.key), p), remaining)
        })
        .map(|g| g.key)
}

/// A rigid newcomer: rides a group whose remaining travel matches its vector, else opens one.
fn join(
    next: &mut FlightPlan,
    member: GroupMember,
    vector: CGPoint,
    presented: &mut HashMap<GroupKey, CGPoint>,
    delta: &mut PlanDelta,
) {
    match landing_for(next, presented, vector, None) {
        Some(key) => {
            let p = presented.get(&key).copied().unwrap_or(next.position(key));
            let rel = group_relative(member.rel, p);
            next.group_mut(key).members.push(GroupMember { rel, ..member });
            delta.joined_tiles.push((member.window, key));
        }
        None => {
            next.open_group(CGPoint::new(0.0, 0.0), vector, member, presented, delta);
        }
    }
}

/// Two translation vectors within `GROUP_TOLERANCE` on both axes.
pub(crate) fn same_vector(a: CGPoint, b: CGPoint) -> bool {
    (a.x - b.x).abs() <= GROUP_TOLERANCE && (a.y - b.y).abs() <= GROUP_TOLERANCE
}

/// `to.origin - from.origin`.
fn vector_of(from: CGRect, to: CGRect) -> CGPoint {
    CGPoint::new(to.origin.x - from.origin.x, to.origin.y - from.origin.y)
}

/// A frame in its container's space: `frame` less the container's position.
pub(crate) fn group_relative(frame: CGRect, position: CGPoint) -> CGRect {
    CGRect::new(
        CGPoint::new(frame.origin.x - position.x, frame.origin.y - position.y),
        frame.size,
    )
}

/// The inverse of [`group_relative`]: a group-relative frame back in overlay space.
pub(crate) fn overlay_of(rel: CGRect, position: CGPoint) -> CGRect {
    CGRect::new(CGPoint::new(rel.origin.x + position.x, rel.origin.y + position.y), rel.size)
}

/// A layout pass as rigid pieces. `requests` are display-space `(window, start, end, floating)`
/// after `start` has resolved parks and dropped what is not worth animating.
pub(crate) fn reflow_plan(requests: &[(WindowId, CGRect, CGRect, bool)], display: CGRect) -> ReflowPlan {
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
pub(crate) fn plan_from_tiles(tiles: &[OverlayTile]) -> ReflowPlan {
    let mut plan = ReflowPlan::empty();
    for tile in tiles {
        plan.adopt(tile);
    }
    plan
}

/// A strip movement as one rigid piece. `strip_travel` already yields overlay space.
/// Pinned windows stand in the floating container; unpinned floating windows ride it by the strip's travel.
pub(crate) fn strip_plan(windows: &[StripWindow], from_offset: CGPoint, to_offset: CGPoint) -> ReflowPlan {
    let travel = strip_pan_travel(from_offset, to_offset);
    let mut plan = ReflowPlan::empty();
    // A pan pins its floating windows; a switch pins none. Both in one pass would need the floating
    // container to move some tiles and not others, so that case falls back to per-tile floating moves.
    let mixed = windows.iter().any(|w| w.pinned) && windows.iter().any(|w| w.floating && !w.pinned);
    for window in windows {
        let (from, to) = strip_travel(window.frame, from_offset, to_offset, window.pinned);
        if window.pinned {
            plan.floating.push((window.window, from, from));
        } else if window.floating {
            if mixed {
                plan.floating.push((window.window, from, to));
            } else {
                plan.floating.push((window.window, from, from));
                plan.floating_travel = travel;
            }
        } else {
            plan.place(window.window, from, travel);
        }
    }
    plan
}
