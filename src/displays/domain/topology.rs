//! The authoritative picture of displays and spaces the spaces actor hands the application:
//! screens with their current space, what changed, and which windows sit on which active space.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::SpaceId;
use rini_geometry::CGRectExt;
use rini_ipc::protocol::{Direction, DisplaySelector};
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
    pub display_space_ids: HashMap<String, Vec<SpaceId>>,
    pub last_user_space_by_display: HashMap<String, SpaceId>,
    pub space_remaps: Vec<(SpaceId, SpaceId)>,
    pub display_set_changed: bool,
    pub topology_changed: bool,
    pub allow_space_remap: bool,
    pub should_force_refresh_layout: bool,
    pub releases_lifecycle_refresh_quarantine: bool,
    /// Releases the reactor's display-churn quarantine only after this authoritative
    /// snapshot has been incorporated into its workspace model.
    pub releases_display_churn_refresh_quarantine: bool,
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

    pub fn first_known_space(&self) -> Option<SpaceId> { self.iter_known_spaces().next() }

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

            let should_replace = best.as_ref().map_or(true, |(best_primary, best_orth, _)| {
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
            DisplaySelector::Index(index) => self.screens_in_physical_order().get(*index).copied(),
            DisplaySelector::Uuid(uuid) => {
                self.screens.iter().find(|screen| screen.display_uuid == *uuid)
            }
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
/// What an incoming topology snapshot changes relative to the one in force. `spaces` and
/// `authoritative_spaces` are per screen; a command-space-only update moves nothing on screen.
#[derive(Debug)]
pub struct SpaceSnapshotAnalysis {
    pub spaces: Vec<Option<SpaceId>>,
    pub authoritative_spaces: Vec<Option<SpaceId>>,
    pub command_space_only_update: bool,
    pub invalidates_pending_targets: bool,
}

pub fn analyze_space_snapshot(
    current: &ForwardedSpaceState,
    current_effective_active_spaces: &HashSet<SpaceId>,
    activation_policy: &SpaceActivationPolicy,
    activation_config: SpaceActivationConfig,
    incoming: &ForwardedSpaceState,
) -> SpaceSnapshotAnalysis {
    let active_window_membership_changed =
        current.active_window_spaces != incoming.active_window_spaces;
    let spaces = incoming.screens.iter().map(|screen| screen.space).collect();
    let display_uuids: Vec<Option<String>> =
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
    let command_space_only_update = !incoming.display_set_changed
        && !incoming.should_force_refresh_layout
        && incoming.space_remaps.is_empty()
        && incoming.resized_spaces.is_empty()
        && incoming.topology_window_delta.is_none()
        && current.screens == incoming.screens
        && current.fullscreen_spaces == incoming.fullscreen_spaces
        && current_effective_active_spaces == &effective_active_spaces
        && current.display_space_ids == incoming.display_space_ids
        && current.last_user_space_by_display == incoming.last_user_space_by_display
        && !active_window_membership_changed;
    let invalidates_pending_targets = incoming.display_set_changed
        || incoming.should_force_refresh_layout
        || !incoming.space_remaps.is_empty()
        || !incoming.resized_spaces.is_empty()
        || incoming.topology_window_delta.is_some();
    SpaceSnapshotAnalysis {
        spaces,
        authoritative_spaces,
        command_space_only_update,
        invalidates_pending_targets,
    }
}
/// What to do with the two halves of a buffered snapshot once the actor is settled again.
///
/// The screen list and the space list arrive from two different native callbacks, so a buffered
/// snapshot can hold either, both, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferedSnapshot {
    /// Both halves, agreeing on how many screens there are. Zip the spaces onto the screens and
    /// forward one snapshot: forwarding the two separately lets the reactor see the new topology
    /// with the old space ids and act between them.
    Merge,
    /// Both halves, disagreeing on the screen count. Neither half is worth forwarding on its own, so
    /// resample.
    Resample,
    /// Only the screens were buffered.
    ScreensOnly,
    /// Only the spaces were buffered.
    SpacesOnly,
    /// Nothing was buffered.
    Nothing,
}

/// Decide what a buffered snapshot amounts to, from the size of each half.
pub fn buffered_snapshot(screens: Option<usize>, spaces: Option<usize>) -> BufferedSnapshot {
    match (screens, spaces) {
        (Some(screens), Some(spaces)) if screens == spaces => BufferedSnapshot::Merge,
        (Some(_), Some(_)) => BufferedSnapshot::Resample,
        (Some(_), None) => BufferedSnapshot::ScreensOnly,
        (None, Some(_)) => BufferedSnapshot::SpacesOnly,
        (None, None) => BufferedSnapshot::Nothing,
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
/// treating it as the truth evacuates every window. `require_complete_spaces` additionally demands
/// that every screen has a space: a screen whose space is still unknown means the sample was taken
/// too early.
pub fn snapshot_is_committable(
    spaces: &[Option<SpaceId>],
    require_complete_spaces: bool,
    is_fullscreen: impl Fn(SpaceId) -> bool,
) -> bool {
    !spaces.is_empty()
        && (!require_complete_spaces || spaces.iter().all(Option::is_some))
        && snapshot_spaces_are_coherent(spaces, is_fullscreen)
}

/// What changed about the attached display set between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySetDelta {
    /// Displays that were attached and are not any more, in the order they were attached.
    pub departed: Vec<String>,
    /// Displays that are attached and were not, in the order they are now attached.
    pub arrived: Vec<String>,
    /// The attached set is the same as it was, so the current window assignments can be trusted.
    pub unchanged: bool,
}

/// Compare two snapshots of the attached displays.
///
/// Both slices carry one entry per screen, in screen order; a screen macOS cannot identify carries
/// an empty string. Taking both lists up front is the point of the function: the caller used to
/// capture the previous set inline, one line before it replaced it, and the ordering was held in
/// place by a comment.
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
    previous: &[String],
    current: &[String],
    display_set_changed: bool,
) -> DisplaySetDelta {
    let named = |uuids: &[String]| -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
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
                    id: ScreenId::new(i as u32 + 1),
                    frame: *frame,
                    display_uuid: format!("uuid-{i}"),
                    name: None,
                    space: Some(SpaceId::new(i as u64 + 1)),
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

    fn snapshot(frames: &[CGRect]) -> ForwardedSpaceState {
        let mut s = state(frames);
        s.active_spaces = s.iter_known_spaces().collect();
        s
    }

    #[test]
    fn an_identical_snapshot_is_a_command_space_only_update() {
        let current = snapshot(&[rect(0., 0.)]);
        let mut incoming = current.clone();
        incoming.command_space = Some(SpaceId::new(1));
        let active: HashSet<SpaceId> = current.active_spaces.clone();
        let a = analyze_space_snapshot(
            &current,
            &active,
            &SpaceActivationPolicy::new(),
            SpaceActivationConfig {
                default_disable: false,
                one_space: false,
            },
            &incoming,
        );
        assert!(a.command_space_only_update);
        assert!(!a.invalidates_pending_targets);
        assert_eq!(a.spaces, vec![Some(SpaceId::new(1))]);
        assert_eq!(a.authoritative_spaces, vec![Some(SpaceId::new(1))]);
    }

    #[test]
    fn a_display_set_change_invalidates_pending_targets_and_is_not_command_only() {
        let current = snapshot(&[rect(0., 0.)]);
        let mut incoming = current.clone();
        incoming.display_set_changed = true;
        let active: HashSet<SpaceId> = current.active_spaces.clone();
        let a = analyze_space_snapshot(
            &current,
            &active,
            &SpaceActivationPolicy::new(),
            SpaceActivationConfig {
                default_disable: false,
                one_space: false,
            },
            &incoming,
        );
        assert!(!a.command_space_only_update);
        assert!(a.invalidates_pending_targets);
    }

    #[test]
    fn an_inactive_screen_space_is_not_authoritative() {
        let current = snapshot(&[rect(0., 0.), rect(1000., 0.)]);
        let mut incoming = current.clone();
        incoming.active_spaces.remove(&SpaceId::new(2));
        let active: HashSet<SpaceId> = current.active_spaces.clone();
        let a = analyze_space_snapshot(
            &current,
            &active,
            &SpaceActivationPolicy::new(),
            SpaceActivationConfig {
                default_disable: false,
                one_space: false,
            },
            &incoming,
        );
        assert_eq!(a.authoritative_spaces, vec![Some(SpaceId::new(1)), None]);
        assert!(!a.command_space_only_update, "the effective active set changed");
    }

    fn uuids(list: &[&str]) -> Vec<String> { list.iter().map(|uuid| uuid.to_string()).collect() }

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
        assert_eq!(
            delta.unchanged, true,
            "one distinct display before, one screen now"
        );
    }

    #[test]
    fn starting_from_nothing_is_all_arrivals() {
        let delta = display_set_delta(&[], &uuids(&["A", "B"]), true);
        assert_eq!(delta.arrived, uuids(&["A", "B"]));
        assert!(delta.departed.is_empty());
    }

    #[test]
    fn two_agreeing_halves_merge() {
        assert_eq!(buffered_snapshot(Some(2), Some(2)), BufferedSnapshot::Merge);
    }

    /// Forwarding the two halves separately lets the reactor see the new topology with the old space
    /// ids and act between them, so a disagreement is resampled rather than half-forwarded.
    #[test]
    fn two_disagreeing_halves_are_resampled() {
        assert_eq!(buffered_snapshot(Some(2), Some(1)), BufferedSnapshot::Resample);
        assert_eq!(buffered_snapshot(Some(1), Some(2)), BufferedSnapshot::Resample);
    }

    #[test]
    fn one_half_forwards_on_its_own() {
        assert_eq!(buffered_snapshot(Some(1), None), BufferedSnapshot::ScreensOnly);
        assert_eq!(buffered_snapshot(None, Some(1)), BufferedSnapshot::SpacesOnly);
    }

    #[test]
    fn nothing_buffered_is_nothing_to_do() {
        assert_eq!(buffered_snapshot(None, None), BufferedSnapshot::Nothing);
    }

    /// Two empty halves still agree, which is a merge of nothing rather than a resample.
    #[test]
    fn two_empty_halves_agree() {
        assert_eq!(buffered_snapshot(Some(0), Some(0)), BufferedSnapshot::Merge);
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
        assert!(!snapshot_is_committable(&[], false, never_fullscreen));
        assert!(!snapshot_is_committable(&[], true, never_fullscreen));
    }

    #[test]
    fn an_unknown_space_blocks_a_complete_commit_only() {
        let spaces = [Some(SpaceId::new(1)), None];
        assert!(!snapshot_is_committable(&spaces, true, never_fullscreen));
        assert!(snapshot_is_committable(&spaces, false, never_fullscreen));
    }

    #[test]
    fn an_incoherent_snapshot_is_never_committable() {
        let spaces = [Some(SpaceId::new(1)), Some(SpaceId::new(1))];
        assert!(!snapshot_is_committable(&spaces, false, never_fullscreen));
    }
}
