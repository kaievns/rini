//! A later pass folded into a flight in progress.
//! See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.

use std::collections::HashMap;

use rini_core::ids::WindowId;
use rini_geometry::SameAs;
use rini_geometry::space::{GroupRect, OverlayPoint, OverlayRect, OverlayVector, Respace};

use super::{
    FlightPlan, GroupKey, GroupMember, Located, PlanDelta, Presented, ReflowPlan, RigidGroup,
    group_relative, on_overlay, overlay_of, same_vector,
};

/// A rigid member's say in where its container goes: the container, the member, the place.
type Vote = (GroupKey, WindowId, OverlayPoint);

/// Folds a later pass into a flight in progress: containers are retargeted, membership changes
/// are reparented at the frames they are drawn at, a pan adds its travel to every group and the
/// columns a move swapped still vote under it, and a window the pass brings rides beside the windows
/// it sits beside as the last pass left them. `presented` is where each container is at the merge.
pub fn merge_plans(
    current: &FlightPlan,
    incoming: &ReflowPlan,
    pan: Option<OverlayVector>,
    presented: &Presented,
    focus: Option<WindowId>,
    viewport: OverlayRect,
) -> (FlightPlan, PlanDelta) {
    let mut merge = Merge::new(current, presented);
    // 1. A strip pan: every group and every loose strip tile moves by `d`; nothing changes hands.
    let pan = pan.filter(|d| *d != OverlayVector::zero());
    if let Some(d) = pan {
        merge.pan(d);
    }
    merge.floating_travel(incoming.floating_travel);
    // 2. Rigid members of the pass: votes for their group's new position, or a join. Joins wait
    // for step 3: a joiner picks its container by how far it travels, which the votes may change.
    let (votes, joins) = merge.vote(incoming, pan, viewport);
    // 3. Per voted group: the largest cluster keeps the container; the rest are reparented.
    merge.settle(votes, pan.is_some(), focus);
    for (member, vector) in joins {
        let Merge {
            next, sent, presented, delta, ..
        } = &mut merge;
        join(next, member, vector, sent, presented, delta);
    }
    // 4. Loose members of the pass.
    merge.loose_members(incoming);
    merge.floating_members(incoming);
    let Merge { next, mut delta, .. } = merge;
    // A group the pan moved and a vote brought back is still going where it was.
    delta
        .retargeted_groups
        .retain(|(key, p)| current.positions.get(key).is_none_or(|was| !was.same_as(*p)));
    (next, delta)
}

/// A merge under way: the plan it makes, what it changed, and where each container is drawn and
/// was last sent.
struct Merge<'a> {
    current: &'a FlightPlan,
    next: FlightPlan,
    delta: PlanDelta,
    presented: Presented,
    /// Where the last pass sent each container: every frame a pass carries is a place on the strip
    /// as that pass left it, so a window the pass brings is placed against these.
    sent: HashMap<GroupKey, OverlayPoint>,
    loose: OverlayPoint,
    /// Where the floating container is before this pass's switch moves it. Floating frames stay
    /// container-relative.
    floating_before: OverlayPoint,
}

impl<'a> Merge<'a> {
    fn new(current: &'a FlightPlan, presented: &Presented) -> Self {
        let mut presented = presented.clone();
        for (key, p) in &current.positions {
            presented.drawn.entry(*key).or_insert(*p);
            presented.along.entry(*key).or_insert(*p);
        }
        Merge {
            current,
            next: current.clone(),
            delta: PlanDelta::default(),
            presented,
            sent: current.positions.clone(),
            loose: current.position(GroupKey::Loose),
            floating_before: current.position(GroupKey::Floating),
        }
    }

    fn pan(&mut self, d: OverlayVector) {
        let next = &mut self.next;
        let delta = &mut self.delta;
        let keys: Vec<GroupKey> =
            next.groups.iter().filter(|g| !g.members.is_empty()).map(|g| g.key).collect();
        for key in keys {
            let p = next.position(key) + d;
            next.move_group(key, p, delta);
        }
        for i in 0..next.changing.len() {
            next.changing[i].2 = next.changing[i].2.translate(d.respace());
            delta.retargeted_tiles.push((next.changing[i].0, next.changing[i].2));
        }
        for i in 0..next.entrances.len() {
            next.entrances[i].2 = next.entrances[i].2.translate(d.respace());
            delta.retargeted_tiles.push((next.entrances[i].0, next.entrances[i].2));
        }
    }

    /// A switch: the floating container travels too.
    fn floating_travel(&mut self, travel: OverlayVector) {
        self.floating_before = self.next.position(GroupKey::Floating);
        if travel != OverlayVector::zero() {
            let p = self.floating_before + travel;
            self.next.floating_travel += travel;
            self.next.positions.insert(GroupKey::Floating, p);
            self.delta.retargeted_groups.push((GroupKey::Floating, p));
        }
    }

    fn vote(
        &mut self,
        incoming: &ReflowPlan,
        pan: Option<OverlayVector>,
        viewport: OverlayRect,
    ) -> (Vec<Vote>, Vec<(GroupMember, OverlayVector)>) {
        let (next, delta, loose) = (&mut self.next, &mut self.delta, self.loose);
        let mut votes: Vec<Vote> = Vec::new();
        let mut joins: Vec<(GroupMember, OverlayVector)> = Vec::new();
        for group in &incoming.groups {
            // Under a pan, only a group not travelling with it votes: a column a move swapped,
            // heading for a slot of its own. Its destination is a place on the strip, never a
            // park, so it votes wherever that is.
            let swapped = pan.is_some_and(|d| !same_vector(group.travel, d));
            for m in &group.members {
                let to = overlay_of(m.rel, group.travel.to_point());
                match next.locate(m.window) {
                    Located::Group(key, rel) => {
                        if swapped || (pan.is_none() && !rides_out(next, key, to, viewport)) {
                            votes.push((
                                key,
                                m.window,
                                to.origin - rel.origin.to_vector().respace(),
                            ));
                        }
                    }
                    // Mid-resize: it stays loose for this flight and bends to the new destination.
                    Located::Changing(i) => {
                        retarget_loose(&mut next.changing, i, group_relative(to, loose), delta)
                    }
                    Located::Entrance(i) => {
                        retarget_loose(&mut next.entrances, i, group_relative(to, loose), delta)
                    }
                    Located::Floating(i) => {
                        let to_rel = group_relative(to, self.floating_before);
                        retarget_loose(&mut next.floating, i, to_rel, delta);
                    }
                    Located::Absent => {
                        let member = GroupMember { window: m.window, rel: m.rel };
                        joins.push((member, group.travel));
                    }
                }
            }
        }
        (votes, joins)
    }

    /// Under a pan, the members of a voted group that did not vote hold the place the pan gave
    /// them.
    fn settle(&mut self, mut votes: Vec<Vote>, panned: bool, focus: Option<WindowId>) {
        let mut keys: Vec<GroupKey> = Vec::new();
        for (key, _, _) in &votes {
            if !keys.contains(key) {
                keys.push(*key);
            }
        }
        if panned {
            for &key in &keys {
                let hold = self.next.position(key);
                let holders: Vec<WindowId> = self
                    .next
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
            self.settle_group(key, &votes, focus);
        }
    }

    fn settle_group(&mut self, key: GroupKey, votes: &[Vote], focus: Option<WindowId>) {
        let mut clusters: Vec<(OverlayPoint, Vec<(WindowId, OverlayPoint)>)> = Vec::new();
        for (k, window, p) in votes.iter().copied().filter(|(k, _, _)| *k == key) {
            debug_assert_eq!(k, key);
            match clusters.iter_mut().find(|(c, _)| same_vector(c.to_vector(), p.to_vector())) {
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
        self.next.move_group(key, p, &mut self.delta);
        for (i, (_, losers)) in clusters.iter().enumerate() {
            if i == winner {
                continue;
            }
            for &(window, p) in losers {
                self.reparent(key, window, p);
            }
        }
    }

    /// What the member still has to travel: a group with the same remaining travel carries it
    /// there. Else it leaves in a container of its own from where it is drawn.
    fn reparent(&mut self, key: GroupKey, window: WindowId, p: OverlayPoint) {
        let from_drawn = position_in(&self.presented.drawn, key);
        let from_along = position_in(&self.presented.along, key);
        let member = self.next.take_member(key, window);
        let remaining = p - from_along;
        let to_key = match landing_for(&self.next, &self.presented.along, remaining, Some(key)) {
            Some(to_key) => {
                let shift = from_along - position_in(&self.presented.along, to_key);
                let rel = member.rel.translate(shift.respace());
                self.next.group_mut(to_key).members.push(GroupMember { rel, ..member });
                to_key
            }
            None => {
                let opened = self.next.open_group(
                    from_drawn,
                    p,
                    member,
                    Some(key),
                    &mut self.presented,
                    &mut self.delta,
                );
                self.sent.insert(opened, position_in(&self.current.positions, key));
                opened
            }
        };
        self.delta.reparented.push((window, key, to_key));
    }

    fn loose_members(&mut self, incoming: &ReflowPlan) {
        let (next, delta, loose) = (&mut self.next, &mut self.delta, self.loose);
        for (list_is_entrance, entries) in
            [(false, &incoming.changing), (true, &incoming.entrances)]
        {
            for &(window, from, to) in entries {
                match next.locate(window) {
                    Located::Group(key, rel) => {
                        // Rigid until now: it leaves its container at the frame it is drawn at.
                        next.take_member(key, window);
                        let at = overlay_of(rel, position_in(&self.presented.drawn, key));
                        next.changing.push((window, group_relative(at, loose), to));
                        delta.reparented.push((window, key, GroupKey::Loose));
                        delta.retargeted_tiles.push((window, to));
                    }
                    Located::Changing(i) => retarget_loose(&mut next.changing, i, to, delta),
                    Located::Entrance(i) => retarget_loose(&mut next.entrances, i, to, delta),
                    Located::Floating(i) => {
                        let to_rel = group_relative(on_overlay(to), self.floating_before);
                        retarget_loose(&mut next.floating, i, to_rel, delta);
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
    }

    fn floating_members(&mut self, incoming: &ReflowPlan) {
        let (next, delta, floating_before) =
            (&mut self.next, &mut self.delta, self.floating_before);
        let drawn = &self.presented.drawn;
        for &(window, from, to) in &incoming.floating {
            let to_rel = group_relative(on_overlay(to), floating_before);
            match next.locate(window) {
                Located::Floating(i) => retarget_loose(&mut next.floating, i, to_rel, delta),
                Located::Group(key, rel) => {
                    next.take_member(key, window);
                    let at = overlay_of(rel, position_in(drawn, key));
                    let from_rel = group_relative(at, position_in(drawn, GroupKey::Floating));
                    next.floating.push((window, from_rel, to_rel));
                    delta.reparented.push((window, key, GroupKey::Floating));
                    delta.retargeted_tiles.push((window, to_rel));
                }
                Located::Changing(i) => retarget_loose(&mut next.changing, i, to, delta),
                Located::Entrance(i) => retarget_loose(&mut next.entrances, i, to, delta),
                Located::Absent => {
                    let from_rel = group_relative(on_overlay(from), floating_before);
                    next.floating.push((window, from_rel, to_rel));
                    delta.joined_tiles.push((window, GroupKey::Floating));
                }
            }
        }
    }
}

fn position_in(positions: &HashMap<GroupKey, OverlayPoint>, key: GroupKey) -> OverlayPoint {
    positions.get(&key).copied().unwrap_or(OverlayPoint::origin())
}

/// A rigid member the pass sends off the viewport while its container is moving keeps riding
/// the container. It has no visible destination, and voting its own vector opened a group that
/// swept it sideways to the park while the row it sat in travelled vertically (the zig-zag seen
/// on a switch whose layout pass parked the departing row). A still container has no motion to
/// lend, so its member votes and exits on its own.
fn rides_out(next: &FlightPlan, key: GroupKey, to: OverlayRect, viewport: OverlayRect) -> bool {
    let moving = next.groups.iter().any(|g| g.key == key && !g.is_still());
    moving && rini_geometry::is_off_screen(viewport, to)
}

/// Bends one loose tile toward `to` unless it is already going there.
fn retarget_loose(
    list: &mut [(WindowId, GroupRect, GroupRect)],
    index: usize,
    to: GroupRect,
    delta: &mut PlanDelta,
) {
    if list[index].2.same_as(to) {
        return;
    }
    list[index].2 = to;
    delta.retargeted_tiles.push((list[index].0, to));
}

/// The group whose remaining travel (destination less where its leg has it, `along`) matches
/// `remaining`, for a member leaving another.
fn landing_for(
    next: &FlightPlan,
    along: &HashMap<GroupKey, OverlayPoint>,
    remaining: OverlayVector,
    exclude: Option<GroupKey>,
) -> Option<GroupKey> {
    travelling(next, along, remaining, exclude).next().map(|g| g.key)
}

/// The groups other than `exclude` whose travel from `start` to where the merge sends them matches
/// `travel`.
fn travelling<'a>(
    next: &'a FlightPlan,
    start: &'a HashMap<GroupKey, OverlayPoint>,
    travel: OverlayVector,
    exclude: Option<GroupKey>,
) -> impl Iterator<Item = &'a RigidGroup> {
    next.groups.iter().filter(move |g| {
        let p = start.get(&g.key).copied().unwrap_or(next.position(g.key));
        Some(g.key) != exclude && same_vector(next.position(g.key) - p, travel)
    })
}

/// A rigid newcomer. Its frame is a place on the strip as the last pass left it (`sent`), so it rides
/// a group that travels as it does from there, placed in it where that pass left it: it starts
/// beside the windows it sits beside, wherever they are drawn, and a bounce or nudge riding the
/// group carries it too. Of several, it takes the group holding the window nearest it. A newcomer
/// travelling as no group does opens one, from its frame.
fn join(
    next: &mut FlightPlan,
    member: GroupMember,
    vector: OverlayVector,
    sent: &mut HashMap<GroupKey, OverlayPoint>,
    presented: &mut Presented,
    delta: &mut PlanDelta,
) {
    let fitting = travelling(next, sent, vector, None);
    let frame = on_overlay(member.rel);
    let apart = |g: &RigidGroup| {
        let at = sent.get(&g.key).copied().unwrap_or(next.position(g.key));
        g.members
            .iter()
            .map(|m| distance(overlay_of(m.rel, at), frame))
            .fold(f64::INFINITY, f64::min)
    };
    let chosen = fitting.min_by(|a, b| apart(a).total_cmp(&apart(b))).map(|g| g.key);
    match chosen {
        Some(key) => {
            let p = sent.get(&key).copied().unwrap_or(next.position(key));
            let rel = group_relative(frame, p);
            next.group_mut(key).members.push(GroupMember { rel, ..member });
            delta.joined_tiles.push((member.window, key));
        }
        None => {
            let origin = OverlayPoint::origin();
            let key = next.open_group(origin, vector.to_point(), member, None, presented, delta);
            sent.insert(key, origin);
        }
    }
}

/// How far apart two frames' nearest edges are: 0 where they touch or overlap.
fn distance(a: OverlayRect, b: OverlayRect) -> f64 {
    let dx = (a.min().x - b.max().x).max(b.min().x - a.max().x).max(0.0);
    let dy = (a.min().y - b.max().y).max(b.min().y - a.max().y).max(0.0);
    dx.hypot(dy)
}
