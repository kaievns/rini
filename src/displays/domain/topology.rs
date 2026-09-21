//! The authoritative picture of displays and spaces the spaces actor hands the application:
//! screens with their current space, what changed, and which windows sit on which active space.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_geometry::CGRectExt;
use rini_ipc::protocol::{Direction, DisplaySelector};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use rini_skylight_sys::{DisplayReconfigFlags, WindowServerId};

use rini_core::ids::SpaceId;
use crate::displays::screen::ScreenInfo;
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
    /// Releases the reactor's display-churn gate only after this authoritative
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

    pub fn first_known_space(&self) -> Option<SpaceId> {
        self.iter_known_spaces().next()
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

    use super::*;
    use rini_core::ids::ScreenId;

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

    fn rect(x: f64, y: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(1000., 1000.))
    }

    #[test]
    fn index_selector_orders_screens_left_to_right_then_top_to_bottom() {
        let s = state(&[rect(2000., 0.), rect(0., 1000.), rect(0., 0.)]);
        let ordered: Vec<_> = s.screens_in_physical_order().iter().map(|sc| sc.id).collect();
        assert_eq!(ordered, vec![ScreenId::new(3), ScreenId::new(2), ScreenId::new(1)]);
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
        assert!(s.screen_for_selector(&DisplaySelector::Direction(Direction::Left), origin).is_none());
        assert!(s.screen_for_selector(&DisplaySelector::Direction(Direction::Right), None).is_none());
    }

    #[test]
    fn uuid_and_point_lookups() {
        let s = state(&[rect(0., 0.), rect(1000., 0.)]);
        assert_eq!(
            s.screen_for_selector(&DisplaySelector::Uuid("uuid-1".into()), None).map(|sc| sc.id),
            Some(ScreenId::new(2))
        );
        assert_eq!(s.screen_for_point(CGPoint::new(1500., 10.)).map(|sc| sc.id), Some(ScreenId::new(2)));
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
            SpaceActivationConfig { default_disable: false, one_space: false },
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
            SpaceActivationConfig { default_disable: false, one_space: false },
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
            SpaceActivationConfig { default_disable: false, one_space: false },
            &incoming,
        );
        assert_eq!(a.authoritative_spaces, vec![Some(SpaceId::new(1)), None]);
        assert!(!a.command_space_only_update, "the effective active set changed");
    }
}
