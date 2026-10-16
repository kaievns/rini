//! The authoritative picture of displays and spaces the spaces actor hands the application:
//! screens with their current space, what changed, and which windows sit on which active space.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::{DisplayUuid, SpaceId};
use rini_geometry::CGRectExt;
use rini_ipc::protocol::{Direction, DisplaySelector, RelativeDisplay};
use rini_skylight_sys::{DisplayReconfigFlags, WindowServerId};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use crate::displays::domain::screen::ScreenInfo;
use crate::displays::domain::space_activation::{SpaceActivationConfig, SpaceActivationPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceEventKind {
    User,
    Fullscreen,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QuarantineStats {
    pub appeared_dropped: u64,
    pub destroyed_dropped: u64,
}
/// Forwarded read-only space/display snapshot consumed by the reactor.
#[derive(Debug, Default, Clone)]
pub struct ForwardedSpaceState {
    pub screens: Vec<ScreenInfo>,
    pub fullscreen_spaces: HashSet<SpaceId>,
    pub has_seen_display_set: bool,
    pub active_spaces: HashSet<SpaceId>,
    pub menu_bar_space: Option<SpaceId>,
    pub command_space: Option<SpaceId>,
    pub display_space_ids: HashMap<DisplayUuid, Vec<SpaceId>>,
    /// The window server did not list the spaces, so `display_space_ids` holds each screen's
    /// current space only.
    pub display_space_ids_from_screens: bool,
    pub last_user_space_by_display: HashMap<DisplayUuid, SpaceId>,
    pub space_remaps: Vec<(SpaceId, SpaceId)>,
    pub topology: Topology,
    pub releases_lifecycle_refresh_quarantine: bool,
    pub resized_spaces: Vec<(SpaceId, CGSize)>,
    pub topology_window_delta: Option<TopologyWindowDelta>,
    pub active_window_spaces: HashMap<WindowServerId, SpaceId>,
}
impl ForwardedSpaceState {
    pub fn screen_by_space(&self, space: SpaceId) -> Option<&ScreenInfo> {
        self.screens.iter().find(|screen| screen.space == Some(space))
    }

    pub fn iter_known_spaces(&self) -> impl Iterator<Item = SpaceId> + '_ {
        self.screens.iter().filter_map(|screen| screen.space)
    }

    /// Every space macOS still lists on an attached display, the ones not on screen included, and
    /// each screen's current space whether or not the list names it. `None` when the window server
    /// did not list them, since the screens alone leave out every Desktop off screen.
    pub fn listed_spaces(&self) -> Option<Vec<SpaceId>> {
        if self.display_space_ids_from_screens {
            return None;
        }
        let mut listed: Vec<SpaceId> = self.iter_known_spaces().collect();
        listed.extend(self.display_space_ids.values().flatten().copied());
        listed.sort_unstable();
        listed.dedup();
        Some(listed)
    }

    pub fn screen_for_point(&self, point: CGPoint) -> Option<&ScreenInfo> {
        self.screens.iter().find(|screen| screen.frame.contains(point))
    }

    pub fn screen_for_direction_from_point(
        &self,
        origin: CGPoint,
        direction: Direction,
    ) -> Option<&ScreenInfo> {
        fn interval_gap(a_min: f64, a_max: f64, b_min: f64, b_max: f64) -> f64 {
            if a_max < b_min {
                b_min - a_max
            } else if b_max < a_min {
                a_min - b_max
            } else {
                0.0
            }
        }

        let mut best: Option<(f64, f64, &ScreenInfo)> = None;

        for screen in &self.screens {
            let frame = screen.frame;

            if frame.contains(origin) {
                continue;
            }

            let min = frame.min();
            let max = frame.max();

            let (primary_dist, orth_gap) = match direction {
                Direction::Left => {
                    if max.x > origin.x {
                        continue;
                    }
                    (origin.x - max.x, interval_gap(min.y, max.y, origin.y, origin.y))
                }
                Direction::Right => {
                    if min.x < origin.x {
                        continue;
                    }
                    (min.x - origin.x, interval_gap(min.y, max.y, origin.y, origin.y))
                }
                Direction::Up => {
                    // Smaller y means visually "up".
                    if max.y > origin.y {
                        continue;
                    }
                    (origin.y - max.y, interval_gap(min.x, max.x, origin.x, origin.x))
                }
                Direction::Down => {
                    if min.y < origin.y {
                        continue;
                    }
                    (min.y - origin.y, interval_gap(min.x, max.x, origin.x, origin.x))
                }
            };

            let should_replace = best.as_ref().is_none_or(|(best_primary, best_orth, _)| {
                primary_dist < *best_primary
                    || (primary_dist == *best_primary && orth_gap < *best_orth)
            });

            if should_replace {
                best = Some((primary_dist, orth_gap, screen));
            }
        }

        best.map(|(_, _, screen)| screen)
    }

    /// `origin` is where a `Direction` selector is measured from; `None` resolves no direction.
    pub fn screen_for_selector(
        &self,
        selector: &DisplaySelector,
        origin: Option<CGPoint>,
    ) -> Option<&ScreenInfo> {
        match selector {
            DisplaySelector::Direction(direction) => {
                self.screen_for_direction_from_point(origin?, *direction)
            }
            DisplaySelector::Relative(RelativeDisplay::Next) => self.screen_after(origin?),
            DisplaySelector::Index(index) => self.screens_in_physical_order().get(*index).copied(),
            DisplaySelector::Uuid(uuid) => {
                self.screens.iter().find(|screen| screen.display_uuid.as_str() == uuid)
            }
        }
    }

    /// The display after the one holding `origin`, in physical order, wrapping to the first.
    ///
    /// Wrapping is what makes one key enough on two displays. An origin on no known display answers
    /// the first display rather than nothing, so the key still moves the window somewhere reachable
    /// when it has been parked off screen.
    pub fn screen_after(&self, origin: CGPoint) -> Option<&ScreenInfo> {
        let order = self.screens_in_physical_order();
        match order.iter().position(|screen| screen.frame.contains(origin)) {
            Some(current) => order.get((current + 1) % order.len()).copied(),
            None => order.first().copied(),
        }
    }

    pub fn screens_in_physical_order(&self) -> Vec<&ScreenInfo> {
        let mut screens: Vec<&ScreenInfo> = self.screens.iter().collect();
        screens.sort_by(|a, b| {
            let x_order = a.frame.origin.x.total_cmp(&b.frame.origin.x);
            if x_order == std::cmp::Ordering::Equal {
                a.frame.origin.y.total_cmp(&b.frame.origin.y)
            } else {
                x_order
            }
        });
        screens
    }
}
/// What a snapshot changed about the displays against the snapshot before. Each tier holds the
/// one before it: a refreshed layout is a changed topology, and a remap is a refreshed layout.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    #[default]
    Unchanged,
    /// The first displays attached, with nothing laid out on any before: no display set has been
    /// seen. Attaching them is a display set change.
    Changed,
    /// The displays changed under a layout, which is refreshed.
    Relayout(DisplaySet),
    /// As `Relayout`, and every screen shows a space no other screen shows, so a display now on
    /// another space takes its layout there.
    Remap(DisplaySet),
}

/// Whether a changed topology attached or detached a display, or only moved, reordered or resized
/// the ones attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplaySet {
    Same,
    Changed,
}

impl Topology {
    /// The topology `screens` has against `previous`. A display set seen before, even one since
    /// detached, means a layout exists to refresh.
    pub fn between(
        previous: &[ScreenInfo],
        screens: &[ScreenInfo],
        has_seen_display_set: bool,
    ) -> Self {
        let uuids = |screens: &[ScreenInfo]| -> HashSet<DisplayUuid> {
            screens.iter().map(|screen| screen.display_uuid.clone()).collect()
        };
        let previous_displays = uuids(previous);
        let display_set = if previous_displays == uuids(screens) {
            DisplaySet::Same
        } else {
            DisplaySet::Changed
        };
        let order_changed = previous
            .iter()
            .map(|screen| &screen.display_uuid)
            .ne(screens.iter().map(|screen| &screen.display_uuid));
        let previous_frames: HashMap<_, _> =
            previous.iter().map(|screen| (&screen.display_uuid, screen.frame)).collect();
        let geometry_changed = screens.iter().any(|screen| {
            previous_frames
                .get(&screen.display_uuid)
                .is_some_and(|before| *before != screen.frame)
        });
        if display_set == DisplaySet::Same && !order_changed && !geometry_changed {
            Self::Unchanged
        } else if !has_seen_display_set && previous_displays.is_empty() {
            Self::Changed
        } else if every_space_once(screens) {
            Self::Remap(display_set)
        } else {
            Self::Relayout(display_set)
        }
    }

    pub fn is_changed(self) -> bool { self != Self::Unchanged }

    pub fn display_set_changed(self) -> bool {
        matches!(
            self,
            Self::Changed | Self::Relayout(DisplaySet::Changed) | Self::Remap(DisplaySet::Changed)
        )
    }

    pub fn forces_refresh_layout(self) -> bool {
        matches!(self, Self::Relayout(_) | Self::Remap(_))
    }

    pub fn allows_space_remap(self) -> bool { matches!(self, Self::Remap(_)) }
}

/// Every screen shows a space, and none shows one another screen shows.
fn every_space_once(screens: &[ScreenInfo]) -> bool {
    let mut seen: HashSet<SpaceId> = HashSet::default();
    screens
        .iter()
        .all(|screen| screen.space.is_some_and(|space| seen.insert(space)))
}

/// What an incoming topology snapshot changes relative to the one in force. `spaces` and
/// `authoritative_spaces` are per screen.
#[derive(Debug)]
pub struct SpaceSnapshotAnalysis {
    pub spaces: Vec<Option<SpaceId>>,
    pub authoritative_spaces: Vec<Option<SpaceId>>,
    pub reach: SnapshotReach,
}

/// How far an incoming snapshot reaches past the one in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotReach {
    /// Nothing on screen moves: only the menu-bar and command spaces are new.
    CommandSpaceOnly,
    /// Something on screen moves, and the pending hidden-window targets still hold.
    OnScreen,
    /// The displays changed, a layout refresh is forced, spaces were remapped or resized, or
    /// windows moved between spaces, so no pending hidden-window target holds.
    InvalidatesPendingTargets,
}

pub fn analyze_space_snapshot(
    current: &ForwardedSpaceState,
    current_effective_active_spaces: &HashSet<SpaceId>,
    activation_policy: &SpaceActivationPolicy,
    activation_config: SpaceActivationConfig,
    incoming: &ForwardedSpaceState,
) -> SpaceSnapshotAnalysis {
    let spaces = incoming.screens.iter().map(|screen| screen.space).collect();
    let display_uuids: Vec<Option<DisplayUuid>> =
        incoming.screens.iter().map(|screen| screen.display_uuid_owned()).collect();
    let authoritative_spaces: Vec<Option<SpaceId>> = incoming
        .screens
        .iter()
        .map(|screen| screen.space.filter(|space| incoming.active_spaces.contains(space)))
        .collect();
    let effective_active_spaces = activation_policy
        .compute_active_spaces(activation_config, &authoritative_spaces, &display_uuids)
        .into_iter()
        .flatten()
        .collect();
    let invalidates_pending_targets = incoming.topology.display_set_changed()
        || incoming.topology.forces_refresh_layout()
        || !incoming.space_remaps.is_empty()
        || !incoming.resized_spaces.is_empty()
        || incoming.topology_window_delta.is_some();
    let moves_nothing_on_screen = current.screens == incoming.screens
        && current.fullscreen_spaces == incoming.fullscreen_spaces
        && current_effective_active_spaces == &effective_active_spaces
        && current.display_space_ids == incoming.display_space_ids
        && current.display_space_ids_from_screens == incoming.display_space_ids_from_screens
        && current.last_user_space_by_display == incoming.last_user_space_by_display
        && current.active_window_spaces == incoming.active_window_spaces;
    let reach = if invalidates_pending_targets {
        SnapshotReach::InvalidatesPendingTargets
    } else if moves_nothing_on_screen {
        SnapshotReach::CommandSpaceOnly
    } else {
        SnapshotReach::OnScreen
    };
    SpaceSnapshotAnalysis {
        spaces,
        authoritative_spaces,
        reach,
    }
}
/// Whether a snapshot's spaces are coherent enough to commit.
///
/// One user space may not appear on two screens at once. macOS reports exactly that mid-transition,
/// and committing it assigns one space's windows to two displays. Fullscreen spaces are exempt:
/// they are nulled out before anything reads them, so a repeat is not a contradiction.
pub fn snapshot_spaces_are_coherent(
    spaces: &[Option<SpaceId>],
    is_fullscreen: impl Fn(SpaceId) -> bool,
) -> bool {
    let mut seen: HashSet<SpaceId> = HashSet::default();
    spaces.iter().all(|space| match space {
        Some(space) if !is_fullscreen(*space) => seen.insert(*space),
        _ => true,
    })
}

/// Whether a snapshot can be forwarded as authoritative.
///
/// An empty screen list is never authoritative — it is what macOS reports mid-reconfiguration, and
/// treating it as the truth evacuates every window. Every screen must have a space: a screen whose
/// space is still unknown means the sample was taken too early.
pub fn snapshot_is_committable(
    spaces: &[Option<SpaceId>],
    is_fullscreen: impl Fn(SpaceId) -> bool,
) -> bool {
    !spaces.is_empty()
        && spaces.iter().all(Option::is_some)
        && snapshot_spaces_are_coherent(spaces, is_fullscreen)
}

/// What changed about the attached display set between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySetDelta {
    /// Displays that were attached and are not now, in the order they were attached.
    pub departed: Vec<DisplayUuid>,
    /// Displays that are attached and were not, in the order they are now attached.
    pub arrived: Vec<DisplayUuid>,
    /// The attached set is the same as it was, so the current window assignments can be trusted.
    pub unchanged: bool,
}

/// Compare two snapshots of the attached displays.
///
/// Both slices carry one entry per screen, in screen order; a screen macOS cannot identify carries
/// an empty UUID. Taking both lists up front is the point of the function: the previous set is read
/// before the current one replaces it, an ordering the signature holds rather than a comment.
///
/// `unchanged` needs `display_set_changed` from the snapshot AS WELL as the counts, because a
/// display can be replaced by another one between two snapshots and leave the count alone. It
/// decides whether the live window assignments are worth recording as display homes: mid-change they
/// are part-way through an evacuation and would record the wrong display.
///
/// A screen with no UUID counts toward the previous set's size — that is what `unchanged` compares
/// against — but can never be matched, so it never appears in `arrived` and is not reported as
/// `departed`. There is nothing to record about a display that cannot be named.
pub fn display_set_delta(
    previous: &[DisplayUuid],
    current: &[DisplayUuid],
    display_set_changed: bool,
) -> DisplaySetDelta {
    let named = |uuids: &[DisplayUuid]| -> Vec<DisplayUuid> {
        let mut seen: Vec<DisplayUuid> = Vec::new();
        for uuid in uuids {
            if !uuid.is_empty() && !seen.iter().any(|kept| kept == uuid) {
                seen.push(uuid.clone());
            }
        }
        seen
    };
    let previous_named = named(previous);
    let current_named = named(current);
    let distinct_previous = previous.iter().collect::<std::collections::BTreeSet<_>>().len();

    DisplaySetDelta {
        departed: previous_named
            .iter()
            .filter(|uuid| !current_named.contains(uuid))
            .cloned()
            .collect(),
        arrived: current_named
            .iter()
            .filter(|uuid| !previous_named.contains(uuid))
            .cloned()
            .collect(),
        unchanged: !display_set_changed && current.len() == distinct_previous,
    }
}

#[derive(Debug, Clone)]
pub struct TopologyWindowDelta {
    pub epoch: u64,
    pub flags: DisplayReconfigFlags,
    pub appeared: Vec<(WindowServerId, SpaceId)>,
    pub disappeared: Vec<(WindowServerId, SpaceId)>,
}
impl Default for TopologyWindowDelta {
    fn default() -> Self {
        Self {
            epoch: 0,
            flags: DisplayReconfigFlags::empty(),
            appeared: Vec::new(),
            disappeared: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGRect, CGSize};
    use rini_core::ids::ScreenId;

    use super::*;

    fn state(frames: &[CGRect]) -> ForwardedSpaceState {
        ForwardedSpaceState {
            screens: frames
                .iter()
                .enumerate()
                .map(|(i, frame)| ScreenInfo {
                    is_builtin: false,
                    id: ScreenId::new(i as u32 + 1),
                    frame: *frame,
                    bounds: *frame,
                    display_uuid: format!("uuid-{i}").into(),
                    name: None,
                    space: Some(SpaceId::new(i as u64 + 1)),
                    scale: 2.0,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn rect(x: f64, y: f64) -> CGRect { CGRect::new(CGPoint::new(x, y), CGSize::new(1000., 1000.)) }

    #[test]
    fn index_selector_orders_screens_left_to_right_then_top_to_bottom() {
        let s = state(&[rect(2000., 0.), rect(0., 1000.), rect(0., 0.)]);
        let ordered: Vec<_> = s.screens_in_physical_order().iter().map(|sc| sc.id).collect();
        assert_eq!(ordered, vec![
            ScreenId::new(3),
            ScreenId::new(2),
            ScreenId::new(1)
        ]);
        assert_eq!(
            s.screen_for_selector(&DisplaySelector::Index(1), None).map(|sc| sc.id),
            Some(ScreenId::new(2))
        );
    }

    /// One key has to get back as well as there, which is what wrapping buys. `Direction` cannot do
    /// this: right from the rightmost display answers nothing.
    #[test]
    fn next_cycles_through_the_displays_and_wraps() {
        let s = state(&[rect(0., 0.), rect(1000., 0.)]);
        let next = |x: f64| {
            s.screen_for_selector(
                &DisplaySelector::Relative(RelativeDisplay::Next),
                Some(CGPoint::new(x, 500.)),
            )
            .map(|sc| sc.id)
        };

        assert_eq!(next(500.), Some(ScreenId::new(2)), "left display -> right");
        assert_eq!(
            next(1500.),
            Some(ScreenId::new(1)),
            "right display wraps back to the left one"
        );
    }

    /// A window parked off every screen still has to go somewhere the user can see.
    #[test]
    fn next_from_nowhere_answers_the_first_display() {
        let s = state(&[rect(0., 0.), rect(1000., 0.)]);
        assert_eq!(
            s.screen_for_selector(
                &DisplaySelector::Relative(RelativeDisplay::Next),
                Some(CGPoint::new(-9000., -9000.))
            )
            .map(|sc| sc.id),
            Some(ScreenId::new(1))
        );
    }

    /// With one display the cycle is a no-op rather than an error: the caller compares source and
    /// target and does nothing, which is the same as the key being unbound.
    #[test]
    fn next_on_a_single_display_answers_that_display() {
        let s = state(&[rect(0., 0.)]);
        assert_eq!(
            s.screen_for_selector(
                &DisplaySelector::Relative(RelativeDisplay::Next),
                Some(CGPoint::new(500., 500.))
            )
            .map(|sc| sc.id),
            Some(ScreenId::new(1))
        );
    }

    #[test]
    fn direction_selector_prefers_nearest_screen_with_smallest_orthogonal_gap() {
        // Origin on screen 1; two screens to the right, one aligned and one far below.
        let s = state(&[rect(0., 0.), rect(1000., 2500.), rect(1000., 0.)]);
        let origin = Some(CGPoint::new(500., 500.));
        let right = s.screen_for_selector(&DisplaySelector::Direction(Direction::Right), origin);
        assert_eq!(right.map(|sc| sc.id), Some(ScreenId::new(3)));
        assert!(
            s.screen_for_selector(&DisplaySelector::Direction(Direction::Left), origin)
                .is_none()
        );
        assert!(
            s.screen_for_selector(&DisplaySelector::Direction(Direction::Right), None)
                .is_none()
        );
    }

    #[test]
    fn uuid_and_point_lookups() {
        let s = state(&[rect(0., 0.), rect(1000., 0.)]);
        assert_eq!(
            s.screen_for_selector(&DisplaySelector::Uuid("uuid-1".into()), None)
                .map(|sc| sc.id),
            Some(ScreenId::new(2))
        );
        assert_eq!(
            s.screen_for_point(CGPoint::new(1500., 10.)).map(|sc| sc.id),
            Some(ScreenId::new(2))
        );
        assert!(s.screen_for_point(CGPoint::new(5000., 10.)).is_none());
    }

    #[test]
    fn the_listed_spaces_are_the_desktops_off_screen_and_the_ones_on_it() {
        let mut s = state(&[rect(0., 0.), rect(1000., 0.)]);
        s.display_space_ids
            .insert("uuid-0".into(), vec![SpaceId::new(1), SpaceId::new(7)]);
        assert_eq!(
            s.listed_spaces(),
            Some(vec![SpaceId::new(1), SpaceId::new(2), SpaceId::new(7)])
        );
    }

    #[test]
    fn spaces_the_window_server_did_not_list_are_not_a_listing() {
        let mut s = state(&[rect(0., 0.)]);
        s.display_space_ids.insert("uuid-0".into(), vec![SpaceId::new(1)]);
        s.display_space_ids_from_screens = true;
        assert_eq!(s.listed_spaces(), None);
    }

    fn snapshot(frames: &[CGRect]) -> ForwardedSpaceState {
        let mut s = state(frames);
        s.active_spaces = s.iter_known_spaces().collect();
        s
    }

    fn analyze(
        current: &ForwardedSpaceState,
        incoming: &ForwardedSpaceState,
    ) -> SpaceSnapshotAnalysis {
        analyze_space_snapshot(
            current,
            &current.active_spaces,
            &SpaceActivationPolicy::new(),
            SpaceActivationConfig { default_disable: false },
            incoming,
        )
    }

    #[test]
    fn an_identical_snapshot_is_a_command_space_only_update() {
        let current = snapshot(&[rect(0., 0.)]);
        let mut incoming = current.clone();
        incoming.command_space = Some(SpaceId::new(1));
        let a = analyze(&current, &incoming);
        assert_eq!(a.reach, SnapshotReach::CommandSpaceOnly);
        assert_eq!(a.spaces, vec![Some(SpaceId::new(1))]);
        assert_eq!(a.authoritative_spaces, vec![Some(SpaceId::new(1))]);
    }

    /// Each row: what the incoming snapshot changes against the one in force, and how far that
    /// reaches. Anything that invalidates the pending targets does so whatever else moved.
    #[test]
    fn a_snapshot_reaches_as_far_as_the_furthest_thing_it_changes() {
        use SnapshotReach::{CommandSpaceOnly, InvalidatesPendingTargets, OnScreen};
        type Change = fn(&mut ForwardedSpaceState);
        let rows: [(&str, Change, SnapshotReach); 11] = [
            (
                "the command space alone",
                |s| s.command_space = Some(SpaceId::new(1)),
                CommandSpaceOnly,
            ),
            (
                "the menu-bar space alone",
                |s| s.menu_bar_space = Some(SpaceId::new(1)),
                CommandSpaceOnly,
            ),
            ("a screen moved", |s| s.screens[0].frame.origin.x = 10., OnScreen),
            (
                "a fullscreen space",
                |s| {
                    s.fullscreen_spaces.insert(SpaceId::new(9));
                },
                OnScreen,
            ),
            (
                "a window newly on an active space",
                |s| {
                    s.active_window_spaces.insert(WindowServerId::new(5), SpaceId::new(1));
                },
                OnScreen,
            ),
            (
                "a display's last user space",
                |s| {
                    s.last_user_space_by_display.insert("uuid-0".into(), SpaceId::new(2));
                },
                OnScreen,
            ),
            (
                "the display set",
                |s| s.topology = Topology::Changed,
                InvalidatesPendingTargets,
            ),
            (
                "a forced refresh",
                |s| s.topology = Topology::Relayout(DisplaySet::Same),
                InvalidatesPendingTargets,
            ),
            (
                "a remapped space",
                |s| s.space_remaps.push((SpaceId::new(1), SpaceId::new(2))),
                InvalidatesPendingTargets,
            ),
            (
                "a resized space",
                |s| s.resized_spaces.push((SpaceId::new(1), CGSize::new(1., 1.))),
                InvalidatesPendingTargets,
            ),
            (
                "a window delta and a screen moved",
                |s| {
                    s.topology_window_delta = Some(TopologyWindowDelta::default());
                    s.screens[0].frame.origin.x = 10.;
                },
                InvalidatesPendingTargets,
            ),
        ];
        let current = snapshot(&[rect(0., 0.)]);
        for (case, change, expected) in rows {
            let mut incoming = current.clone();
            change(&mut incoming);
            assert_eq!(analyze(&current, &incoming).reach, expected, "{case}");
        }
    }

    #[test]
    fn an_inactive_screen_space_is_not_authoritative() {
        let current = snapshot(&[rect(0., 0.), rect(1000., 0.)]);
        let mut incoming = current.clone();
        incoming.active_spaces.remove(&SpaceId::new(2));
        let a = analyze(&current, &incoming);
        assert_eq!(a.authoritative_spaces, vec![Some(SpaceId::new(1)), None]);
        assert_eq!(
            a.reach,
            SnapshotReach::OnScreen,
            "the effective active set changed"
        );
    }

    fn shown(display: &str, x: f64, space: Option<u64>) -> ScreenInfo {
        ScreenInfo {
            is_builtin: false,
            id: ScreenId::new(1),
            frame: rect(x, 0.),
            bounds: rect(x, 0.),
            display_uuid: display.into(),
            name: None,
            space: space.map(SpaceId::new),
            scale: 2.0,
        }
    }

    /// Each row: the screens before, the screens now, whether a display set was seen before, and
    /// the tier that holds.
    #[test]
    fn a_topology_takes_the_highest_tier_its_change_reaches() {
        use DisplaySet::{Changed as Set, Same};
        let a = |space| shown("a", 0., space);
        let b = |space| shown("b", 1000., space);
        let a_moved = shown("a", 500., Some(1));
        let rows = [
            (
                "nothing moved",
                vec![a(Some(1))],
                vec![a(Some(1))],
                true,
                Topology::Unchanged,
            ),
            (
                "a space switch alone",
                vec![a(Some(1))],
                vec![a(Some(2))],
                true,
                Topology::Unchanged,
            ),
            (
                "no screens either side",
                vec![],
                vec![],
                true,
                Topology::Unchanged,
            ),
            (
                "the first display set",
                vec![],
                vec![a(Some(1))],
                false,
                Topology::Changed,
            ),
            (
                "back after every display left",
                vec![],
                vec![a(Some(1))],
                true,
                Topology::Remap(Set),
            ),
            (
                "a display arrived",
                vec![a(Some(1))],
                vec![a(Some(1)), b(Some(2))],
                false,
                Topology::Remap(Set),
            ),
            (
                "a display arrived with no space",
                vec![a(Some(1))],
                vec![a(Some(1)), b(None)],
                true,
                Topology::Relayout(Set),
            ),
            (
                "one space on two screens",
                vec![a(Some(1))],
                vec![a(Some(1)), b(Some(1))],
                true,
                Topology::Relayout(Set),
            ),
            (
                "every display left",
                vec![a(Some(1))],
                vec![],
                true,
                Topology::Remap(Set),
            ),
            (
                "a display moved",
                vec![a(Some(1))],
                vec![a_moved],
                true,
                Topology::Remap(Same),
            ),
        ];
        for (case, previous, screens, seen, expected) in rows {
            assert_eq!(Topology::between(&previous, &screens, seen), expected, "{case}");
        }
        let reordered =
            Topology::between(&[a(Some(1)), b(Some(2))], &[b(Some(2)), a(Some(1))], true);
        assert_eq!(reordered, Topology::Remap(Same), "the displays reordered");
    }

    /// The implications the tiers hold: a remap refreshes the layout, a refresh is a change, and a
    /// display attached or detached is a change.
    #[test]
    fn each_tier_answers_for_the_tiers_below_it() {
        use DisplaySet::{Changed as Set, Same};
        let rows = [
            (Topology::Unchanged, [false, false, false, false]),
            (Topology::Changed, [true, true, false, false]),
            (Topology::Relayout(Set), [true, true, true, false]),
            (Topology::Relayout(Same), [true, false, true, false]),
            (Topology::Remap(Set), [true, true, true, true]),
            (Topology::Remap(Same), [true, false, true, true]),
        ];
        for (topology, expected) in rows {
            let answered = [
                topology.is_changed(),
                topology.display_set_changed(),
                topology.forces_refresh_layout(),
                topology.allows_space_remap(),
            ];
            assert_eq!(answered, expected, "{topology:?}");
        }
    }

    fn uuids(list: &[&str]) -> Vec<DisplayUuid> {
        list.iter().map(|uuid| DisplayUuid::new(*uuid)).collect()
    }

    #[test]
    fn nothing_attached_or_detached_is_no_delta() {
        let before = uuids(&["A", "B"]);
        let delta = display_set_delta(&before, &before, false);
        assert!(delta.departed.is_empty());
        assert!(delta.arrived.is_empty());
        assert!(delta.unchanged);
    }

    #[test]
    fn unplugging_a_display_reports_it_departed() {
        let delta = display_set_delta(&uuids(&["A", "B"]), &uuids(&["A"]), true);
        assert_eq!(delta.departed, uuids(&["B"]));
        assert!(delta.arrived.is_empty());
        assert!(!delta.unchanged);
    }

    #[test]
    fn plugging_a_display_in_reports_it_arrived() {
        let delta = display_set_delta(&uuids(&["A"]), &uuids(&["A", "B"]), true);
        assert_eq!(delta.arrived, uuids(&["B"]));
        assert!(delta.departed.is_empty());
    }

    /// Swapping one display for another keeps the count, which is why `unchanged` needs the
    /// snapshot's own flag and not just the sizes.
    #[test]
    fn swapping_one_display_for_another_is_not_unchanged() {
        let delta = display_set_delta(&uuids(&["A", "B"]), &uuids(&["A", "C"]), true);
        assert_eq!(delta.departed, uuids(&["B"]));
        assert_eq!(delta.arrived, uuids(&["C"]));
        assert!(!delta.unchanged);
    }

    /// The flag alone is not enough either: a snapshot can say nothing changed while the counts
    /// disagree, and then the live assignments are still mid-evacuation.
    #[test]
    fn a_count_that_disagrees_is_not_unchanged_even_without_the_flag() {
        let delta = display_set_delta(&uuids(&["A", "B"]), &uuids(&["A"]), false);
        assert!(!delta.unchanged);
    }

    /// Departed and arrived keep screen order, because the caller records display homes per
    /// departing display and reads them back per arriving one.
    #[test]
    fn both_lists_keep_screen_order() {
        let delta = display_set_delta(&uuids(&["A", "B", "C"]), &uuids(&["D", "E"]), true);
        assert_eq!(delta.departed, uuids(&["A", "B", "C"]));
        assert_eq!(delta.arrived, uuids(&["D", "E"]));
    }

    /// A screen macOS cannot name is counted, so it does not make a settled topology look changed,
    /// but it is never reported either way: there is nothing to record about it.
    #[test]
    fn a_screen_with_no_uuid_is_counted_but_never_reported() {
        let before = uuids(&["A", ""]);
        let after = uuids(&["A", ""]);
        let delta = display_set_delta(&before, &after, false);
        assert!(delta.departed.is_empty());
        assert!(delta.arrived.is_empty());
        assert!(delta.unchanged, "two screens before, two now");
    }

    #[test]
    fn a_display_reported_twice_is_one_display() {
        let delta = display_set_delta(&uuids(&["A", "A"]), &uuids(&["A"]), false);
        assert!(delta.departed.is_empty());
        assert!(delta.unchanged, "one distinct display before, one screen now");
    }

    #[test]
    fn starting_from_nothing_is_all_arrivals() {
        let delta = display_set_delta(&[], &uuids(&["A", "B"]), true);
        assert_eq!(delta.arrived, uuids(&["A", "B"]));
        assert!(delta.departed.is_empty());
    }

    fn never_fullscreen(_: SpaceId) -> bool { false }

    /// One user space on two screens at once is what macOS reports mid-transition. Committing it
    /// assigns one space's windows to two displays.
    #[test]
    fn one_user_space_on_two_screens_is_not_coherent() {
        let spaces = [Some(SpaceId::new(1)), Some(SpaceId::new(1))];
        assert!(!snapshot_spaces_are_coherent(&spaces, never_fullscreen));
    }

    #[test]
    fn distinct_user_spaces_are_coherent() {
        let spaces = [Some(SpaceId::new(1)), Some(SpaceId::new(2))];
        assert!(snapshot_spaces_are_coherent(&spaces, never_fullscreen));
    }

    /// Fullscreen spaces are nulled out before anything reads them, so a repeat is not a
    /// contradiction. This is the rule the classifier had to become injectable to test.
    #[test]
    fn a_repeated_fullscreen_space_is_still_coherent() {
        let fullscreen = SpaceId::new(99);
        let spaces = [Some(fullscreen), Some(fullscreen)];
        assert!(snapshot_spaces_are_coherent(&spaces, |space| space == fullscreen));
        assert!(
            !snapshot_spaces_are_coherent(&spaces, never_fullscreen),
            "the same pair is incoherent once those ids are user spaces"
        );
    }

    #[test]
    fn a_screen_with_no_space_never_makes_a_snapshot_incoherent() {
        let spaces = [None, None];
        assert!(snapshot_spaces_are_coherent(&spaces, never_fullscreen));
    }

    /// An empty screen list is what macOS reports mid-reconfiguration. Treating it as the truth
    /// evacuates every window.
    #[test]
    fn no_screens_is_never_committable() {
        assert!(!snapshot_is_committable(&[], never_fullscreen));
    }

    #[test]
    fn an_unknown_space_blocks_a_commit() {
        let spaces = [Some(SpaceId::new(1)), None];
        assert!(!snapshot_is_committable(&spaces, never_fullscreen));
    }

    #[test]
    fn a_complete_coherent_snapshot_is_committable() {
        let spaces = [Some(SpaceId::new(1)), Some(SpaceId::new(2))];
        assert!(snapshot_is_committable(&spaces, never_fullscreen));
    }

    #[test]
    fn an_incoherent_snapshot_is_never_committable() {
        let spaces = [Some(SpaceId::new(1)), Some(SpaceId::new(1))];
        assert!(!snapshot_is_committable(&spaces, never_fullscreen));
    }
}
