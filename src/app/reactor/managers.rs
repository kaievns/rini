use objc2_core_foundation::CGRect;
use rini_core::ids::{WindowId, pid_t};
use rustc_hash::FxHashSet as HashSet;

use super::{Event, WorkspaceSwitchOrigin, WorkspaceSwitchState};
use crate::app::{channels, hotkeys as wm_controller};
use crate::input::domain::drag_swap::DragManager as DragSwapManager;
use crate::input::platform::input_tap as event_tap;
use crate::input::settings::WindowSnappingSettings;
use crate::windows::domain::raise as raise_manager;

/// Manages drag operations and window swapping
pub(super) struct DragManager {
    pub(super) drag_state: super::DragState,
    pub(super) drag_swap_manager: DragSwapManager,
    pub(super) skip_layout_for_window: Option<WindowId>,
    /// The last double-click on a tiled window's title bar, while it can still explain that
    /// window's resizes.
    pub(super) title_bar_click: Option<crate::windows::domain::zoom::Evidence>,
}

impl DragManager {
    pub(super) fn reset(&mut self) { self.drag_swap_manager.reset(); }

    pub(super) fn last_target(&self) -> Option<WindowId> { self.drag_swap_manager.last_target() }

    pub(super) fn dragged(&self) -> Option<WindowId> { self.drag_swap_manager.dragged() }

    pub(super) fn origin_frame(&self) -> Option<CGRect> { self.drag_swap_manager.origin_frame() }

    pub(super) fn update_config(&mut self, config: WindowSnappingSettings) {
        self.drag_swap_manager.update_config(config);
    }
}

/// The sorted id list to tell the window server about, or `None` if `last` already says it.
///
/// Sorted because the caller collects from a hash map, so the same set arrives in a different order
/// each time and an unsorted comparison would resubscribe on every event. The subscription is a
/// system call per change, and the set is unchanged for most events.
pub(super) fn notification_ids_to_publish(
    last: &[u32],
    ids: impl Iterator<Item = u32>,
) -> Option<Vec<u32>> {
    let mut ids: Vec<u32> = ids.collect();
    ids.sort_unstable();
    (ids != last).then_some(ids)
}

/// Manages Mission Control state
pub(super) struct MissionControlManager {
    pub(super) mission_control_state: super::MissionControlState,
    pub(super) pending_mission_control_refresh: HashSet<pid_t>,
}

/// Manages workspace switching state
pub(super) struct WorkspaceSwitchManager {
    pub(super) workspace_switch_state: super::WorkspaceSwitchState,
    pub(super) workspace_switch_generation: u64,
    pub(super) active_workspace_switch: Option<u64>,
    pub(super) pending_workspace_switch_origin: Option<WorkspaceSwitchOrigin>,
    pub(super) pending_workspace_mouse_warp: Option<WindowId>,
}

impl WorkspaceSwitchManager {
    pub(super) fn start_workspace_switch(&mut self, origin: WorkspaceSwitchOrigin) {
        self.workspace_switch_generation = self.workspace_switch_generation.wrapping_add(1);
        self.active_workspace_switch = Some(self.workspace_switch_generation);
        self.workspace_switch_state = WorkspaceSwitchState::Active;
        self.pending_workspace_switch_origin = Some(origin);
    }

    pub(super) fn manual_switch_in_progress(&self) -> bool {
        self.workspace_switch_state == WorkspaceSwitchState::Active
            && self.pending_workspace_switch_origin == Some(WorkspaceSwitchOrigin::Manual)
    }

    pub(super) fn mark_workspace_switch_inactive(&mut self) {
        self.workspace_switch_state = WorkspaceSwitchState::Inactive;
        self.pending_workspace_switch_origin = None;
    }
}

/// Manages refocus and cleanup state
pub(super) struct RefocusManager {
    pub(super) stale_cleanup_state: super::StaleCleanupState,
    pub(super) refocus_state: super::RefocusState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RefreshQuarantineState {
    Ready,
    Sleeping,
    SessionInactive,
    DisplayChurn,
}

pub(super) struct RefreshQuarantineManager {
    pub(super) sleeping: bool,
    pub(super) session_inactive: bool,
    pub(super) display_churn_active: bool,
    pub(super) awaiting_post_wake_snapshot: bool,
    pub(super) awaiting_post_session_snapshot: bool,
    pub(super) pending_visible_refresh: bool,
    pub(super) deferred_refresh_tracks_mission_control: bool,
}

impl RefreshQuarantineManager {
    pub(super) fn state(&self) -> RefreshQuarantineState {
        if self.sleeping {
            RefreshQuarantineState::Sleeping
        } else if self.session_inactive {
            RefreshQuarantineState::SessionInactive
        } else if self.display_churn_active {
            RefreshQuarantineState::DisplayChurn
        } else {
            RefreshQuarantineState::Ready
        }
    }

    pub(super) fn blocks_refreshes(&self) -> bool { self.state() != RefreshQuarantineState::Ready }
}

/// Manages communication channels to other actors
pub(super) struct CommunicationManager {
    pub(super) event_tap_tx: Option<event_tap::Sender>,
    pub(super) cursor_warp_tx: Option<crate::displays::platform::cursor_warp::Sender>,
    pub(super) transitions_tx: Option<crate::transitions::platform::engine::Sender>,
    pub(super) pictures: Option<crate::pictures::platform::service::Pictures>,
    pub(super) raise_manager_tx: raise_manager::Sender,
    pub(super) wm_sender: Option<wm_controller::Sender>,
    pub(super) events_tx: Option<channels::Sender<Event>>,
}

#[cfg(test)]
mod tests {
    use super::notification_ids_to_publish;

    #[test]
    fn a_new_window_is_published() {
        assert_eq!(
            notification_ids_to_publish(&[1, 2], [1, 2, 3].into_iter()),
            Some(vec![1, 2, 3])
        );
    }

    /// The caller collects from a hash map, so the same set arrives in a different order each time.
    /// Comparing unsorted would resubscribe on every event, and each resubscribe is a system call.
    #[test]
    fn the_same_set_in_a_different_order_publishes_nothing() {
        assert_eq!(
            notification_ids_to_publish(&[1, 2, 3], [3, 1, 2].into_iter()),
            None
        );
    }

    #[test]
    fn a_closed_window_is_published() {
        assert_eq!(
            notification_ids_to_publish(&[1, 2, 3], [1, 3].into_iter()),
            Some(vec![1, 3])
        );
    }

    #[test]
    fn the_last_window_closing_is_published_as_an_empty_set() {
        assert_eq!(
            notification_ids_to_publish(&[1], std::iter::empty()),
            Some(vec![])
        );
    }

    #[test]
    fn nothing_tracked_and_nothing_known_publishes_nothing() {
        assert_eq!(notification_ids_to_publish(&[], std::iter::empty()), None);
    }
}
