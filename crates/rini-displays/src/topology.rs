//! The authoritative picture of displays and spaces the spaces actor hands the application:
//! screens with their current space, what changed, and which windows sit on which active space.
use objc2_core_foundation::CGSize;
use rini_shared::collections::{HashMap, HashSet};
use rini_skylight_sys::{DisplayReconfigFlags, WindowServerId};

use crate::ids::SpaceId;
use crate::screen::ScreenInfo;

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
