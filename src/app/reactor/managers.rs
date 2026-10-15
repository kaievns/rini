use objc2_core_foundation::CGRect;
use rini_core::ids::{WindowId, pid_t};
use rustc_hash::FxHashSet as HashSet;

use super::{Event, WorkspaceSwitchOrigin, WorkspaceSwitchState};
use crate::app::{channels, controller};
use crate::input::domain::drag_swap::DragSwap;
use crate::input::platform::input_tap as event_tap;
use crate::input::settings::WindowSnappingSettings;
use crate::windows::domain::raise as raise_manager;

/// Manages drag operations and window swapping
pub(super) struct DragManager {
    pub(super) drag_state: super::DragState,
    pub(super) drag_swap: DragSwap,
    pub(super) skip_layout_for_window: Option<WindowId>,
    /// The last double-click on a tiled window's title bar, while it can still explain that
    /// window's resizes.
    pub(super) title_bar_click: Option<crate::windows::domain::zoom::Evidence>,
}

impl DragManager {
    pub(super) fn reset(&mut self) { self.drag_swap.reset(); }

    pub(super) fn last_target(&self) -> Option<WindowId> { self.drag_swap.last_target() }

    pub(super) fn dragged(&self) -> Option<WindowId> { self.drag_swap.dragged() }

    pub(super) fn origin_frame(&self) -> Option<CGRect> { self.drag_swap.origin_frame() }

    pub(super) fn update_config(&mut self, config: WindowSnappingSettings) {
        self.drag_swap.update_config(config);
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

/// Whether the machine sleeps, and once it wakes, whether a snapshot releasing it has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Wake {
    Awake,
    Asleep,
    AwaitingSnapshot,
}

/// Whether the login window owns the displays, and once it lets go, whether a snapshot releasing
/// the session has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Session {
    Active,
    Inactive,
    AwaitingSnapshot,
}

/// Whether a display reconfiguration holds window events and refreshes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Churn {
    Steady,
    /// From the spaces actor's `DisplayChurnBegin` to its end: window events are dropped and
    /// refreshes held.
    Churning,
    /// Ended, with no snapshot incorporated since: refreshes are still held.
    Settling,
}

/// The one visible-window refresh owed until every quarantine lifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VisibleRefresh {
    pub(super) tracks_mission_control: bool,
}

impl VisibleRefresh {
    const TRACKING_MISSION_CONTROL: Self = Self { tracks_mission_control: true };

    fn and(self, other: Self) -> Self {
        Self {
            tracks_mission_control: self.tracks_mission_control || other.tracks_mission_control,
        }
    }
}

/// What moves the refresh quarantine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuarantineSignal {
    WillSleep,
    Woke,
    SessionResigned,
    SessionActivated,
    ChurnBegan,
    /// The spaces actor's `DisplayChurnEnd`, or a snapshot, which the spaces actor forwards during
    /// a churn only as the settled one, ahead of its end.
    ChurnEnded,
    /// A refresh asked for while refreshes are blocked.
    RefreshDeferred(VisibleRefresh),
    /// A forwarded snapshot, incorporated. Every one the spaces actor forwards is coherent, so each
    /// ends the churn and owes one visible-window refresh; `releases_lifecycle` when it is the first
    /// taken after a wake or an unlock.
    Incorporated {
        releases_lifecycle: bool,
    },
}

/// What the reactor holds back while the displays or the session are unstable. It changes only
/// through `on`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RefreshQuarantineManager {
    wake: Wake,
    session: Session,
    churn: Churn,
    owed: Option<VisibleRefresh>,
}

impl Default for RefreshQuarantineManager {
    fn default() -> Self {
        Self {
            wake: Wake::Awake,
            session: Session::Active,
            churn: Churn::Steady,
            owed: None,
        }
    }
}

impl RefreshQuarantineManager {
    /// Every transition. Answers the owed refresh to make now, which only an incorporated snapshot
    /// that leaves nothing held does.
    pub(super) fn on(&mut self, signal: QuarantineSignal) -> Option<VisibleRefresh> {
        match signal {
            QuarantineSignal::WillSleep => self.wake = Wake::Asleep,
            QuarantineSignal::Woke => {
                self.wake = Wake::AwaitingSnapshot;
                self.owe(VisibleRefresh::TRACKING_MISSION_CONTROL);
            }
            QuarantineSignal::SessionResigned => self.session = Session::Inactive,
            QuarantineSignal::SessionActivated => {
                self.session = Session::AwaitingSnapshot;
                self.owe(VisibleRefresh::TRACKING_MISSION_CONTROL);
            }
            QuarantineSignal::ChurnBegan => self.churn = Churn::Churning,
            QuarantineSignal::ChurnEnded => {
                if self.churn == Churn::Churning {
                    self.churn = Churn::Settling;
                }
            }
            QuarantineSignal::RefreshDeferred(refresh) => self.owe(refresh),
            QuarantineSignal::Incorporated { releases_lifecycle } => {
                self.churn = Churn::Steady;
                self.owe(VisibleRefresh::TRACKING_MISSION_CONTROL);
                if releases_lifecycle {
                    if self.wake == Wake::AwaitingSnapshot {
                        self.wake = Wake::Awake;
                    }
                    if self.session == Session::AwaitingSnapshot {
                        self.session = Session::Active;
                    }
                }
                if !self.blocks_refreshes() {
                    return self.owed.take();
                }
            }
        }
        None
    }

    fn owe(&mut self, refresh: VisibleRefresh) {
        self.owed = Some(self.owed.map_or(refresh, |owed| owed.and(refresh)));
    }

    /// Sleep first, then the session, then churn.
    pub(super) fn state(&self) -> RefreshQuarantineState {
        if self.wake != Wake::Awake {
            RefreshQuarantineState::Sleeping
        } else if self.session != Session::Active {
            RefreshQuarantineState::SessionInactive
        } else if self.churn != Churn::Steady {
            RefreshQuarantineState::DisplayChurn
        } else {
            RefreshQuarantineState::Ready
        }
    }

    pub(super) fn blocks_refreshes(&self) -> bool { self.state() != RefreshQuarantineState::Ready }

    #[cfg(test)]
    pub(super) fn wake(&self) -> Wake { self.wake }

    #[cfg(test)]
    pub(super) fn session(&self) -> Session { self.session }

    pub(super) fn churn(&self) -> Churn { self.churn }

    #[cfg(test)]
    pub(super) fn owed(&self) -> Option<VisibleRefresh> { self.owed }
}

/// Manages communication channels to other actors
pub(super) struct CommunicationManager {
    pub(super) event_tap_tx: Option<event_tap::Sender>,
    pub(super) cursor_warp_tx: Option<crate::displays::platform::cursor_warp::Sender>,
    pub(super) transitions_tx: Option<crate::transitions::platform::engine::Sender>,
    pub(super) pictures: Option<crate::pictures::platform::service::Pictures>,
    pub(super) raise_manager_tx: raise_manager::Sender,
    pub(super) wm_sender: Option<controller::Sender>,
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

    mod quarantine {
        use super::super::{
            Churn, QuarantineSignal, RefreshQuarantineManager, RefreshQuarantineState, Session,
            VisibleRefresh, Wake,
        };

        const UNTRACKED: VisibleRefresh = VisibleRefresh { tracks_mission_control: false };
        const TRACKED: VisibleRefresh = VisibleRefresh { tracks_mission_control: true };

        /// Every signal, in the column order of the tables below.
        const SIGNALS: [QuarantineSignal; 9] = [
            QuarantineSignal::WillSleep,
            QuarantineSignal::Woke,
            QuarantineSignal::SessionResigned,
            QuarantineSignal::SessionActivated,
            QuarantineSignal::ChurnBegan,
            QuarantineSignal::ChurnEnded,
            QuarantineSignal::RefreshDeferred(UNTRACKED),
            QuarantineSignal::Incorporated { releases_lifecycle: true },
            QuarantineSignal::Incorporated { releases_lifecycle: false },
        ];

        fn quarantine(wake: Wake, session: Session, churn: Churn) -> RefreshQuarantineManager {
            RefreshQuarantineManager {
                wake,
                session,
                churn,
                owed: None,
            }
        }

        #[test]
        fn wake_moves_on_every_signal_as_its_table_says() {
            use Wake::{Asleep, AwaitingSnapshot as Waiting, Awake};
            #[rustfmt::skip]
            let table: [(Wake, [Wake; SIGNALS.len()]); 3] = [
                //        sleep   woke     resign   active   churn    ended    defer    released snapshot
                (Awake,   [Asleep, Waiting, Awake,   Awake,   Awake,   Awake,   Awake,   Awake,   Awake]),
                (Asleep,  [Asleep, Waiting, Asleep,  Asleep,  Asleep,  Asleep,  Asleep,  Asleep,  Asleep]),
                (Waiting, [Asleep, Waiting, Waiting, Waiting, Waiting, Waiting, Waiting, Awake,   Waiting]),
            ];
            for (from, row) in table {
                for (signal, to) in SIGNALS.into_iter().zip(row) {
                    let mut quarantine = quarantine(from, Session::Active, Churn::Steady);
                    quarantine.on(signal);
                    assert_eq!(quarantine.wake, to, "{from:?} on {signal:?}");
                }
            }
        }

        #[test]
        fn the_session_moves_on_every_signal_as_its_table_says() {
            use Session::{Active, AwaitingSnapshot as Waiting, Inactive};
            #[rustfmt::skip]
            let table: [(Session, [Session; SIGNALS.len()]); 3] = [
                //         sleep     woke      resign    active   churn     ended     defer     released  snapshot
                (Active,   [Active,   Active,   Inactive, Waiting, Active,   Active,   Active,   Active,   Active]),
                (Inactive, [Inactive, Inactive, Inactive, Waiting, Inactive, Inactive, Inactive, Inactive, Inactive]),
                (Waiting,  [Waiting,  Waiting,  Inactive, Waiting, Waiting,  Waiting,  Waiting,  Active,   Waiting]),
            ];
            for (from, row) in table {
                for (signal, to) in SIGNALS.into_iter().zip(row) {
                    let mut quarantine = quarantine(Wake::Awake, from, Churn::Steady);
                    quarantine.on(signal);
                    assert_eq!(quarantine.session, to, "{from:?} on {signal:?}");
                }
            }
        }

        #[test]
        fn churn_moves_on_every_signal_as_its_table_says() {
            use Churn::{Churning, Settling, Steady};
            #[rustfmt::skip]
            let table: [(Churn, [Churn; SIGNALS.len()]); 3] = [
                //         sleep     woke      resign    active    churn     ended     defer     released snapshot
                (Steady,   [Steady,   Steady,   Steady,   Steady,   Churning, Steady,   Steady,   Steady,  Steady]),
                (Churning, [Churning, Churning, Churning, Churning, Churning, Settling, Churning, Steady,  Steady]),
                (Settling, [Settling, Settling, Settling, Settling, Churning, Settling, Settling, Steady,  Steady]),
            ];
            for (from, row) in table {
                for (signal, to) in SIGNALS.into_iter().zip(row) {
                    let mut quarantine = quarantine(Wake::Awake, Session::Active, from);
                    quarantine.on(signal);
                    assert_eq!(quarantine.churn, to, "{from:?} on {signal:?}");
                }
            }
        }

        #[test]
        fn sleep_outranks_the_session_which_outranks_churn() {
            use RefreshQuarantineState::{DisplayChurn, Ready, SessionInactive, Sleeping};
            let table = [
                (Wake::Asleep, Session::Inactive, Churn::Churning, Sleeping),
                (Wake::AwaitingSnapshot, Session::Active, Churn::Steady, Sleeping),
                (Wake::Awake, Session::Inactive, Churn::Churning, SessionInactive),
                (
                    Wake::Awake,
                    Session::AwaitingSnapshot,
                    Churn::Steady,
                    SessionInactive,
                ),
                (Wake::Awake, Session::Active, Churn::Churning, DisplayChurn),
                (Wake::Awake, Session::Active, Churn::Settling, DisplayChurn),
                (Wake::Awake, Session::Active, Churn::Steady, Ready),
            ];
            for (wake, session, churn, state) in table {
                let quarantine = quarantine(wake, session, churn);
                assert_eq!(quarantine.state(), state, "{wake:?} {session:?} {churn:?}");
                assert_eq!(quarantine.blocks_refreshes(), state != Ready);
            }
        }

        #[test]
        fn a_snapshot_with_nothing_held_makes_one_refresh_tracking_mission_control() {
            let mut quarantine = RefreshQuarantineManager::default();
            assert_eq!(
                quarantine.on(QuarantineSignal::Incorporated { releases_lifecycle: false }),
                Some(TRACKED)
            );
            assert_eq!(quarantine.owed, None);
        }

        #[test]
        fn a_refresh_deferred_while_held_waits_for_the_snapshot_that_releases_the_hold() {
            let mut quarantine = quarantine(Wake::Awake, Session::Active, Churn::Churning);
            assert_eq!(quarantine.on(QuarantineSignal::RefreshDeferred(UNTRACKED)), None);
            assert_eq!(quarantine.owed, Some(UNTRACKED));
            assert_eq!(
                quarantine.on(QuarantineSignal::Incorporated { releases_lifecycle: false }),
                Some(TRACKED)
            );
        }

        #[test]
        fn a_deferred_refresh_tracks_mission_control_once_any_deferral_asks_it_to() {
            let mut quarantine = quarantine(Wake::Asleep, Session::Active, Churn::Steady);
            quarantine.on(QuarantineSignal::RefreshDeferred(TRACKED));
            quarantine.on(QuarantineSignal::RefreshDeferred(UNTRACKED));
            assert_eq!(quarantine.owed, Some(TRACKED));
        }

        #[test]
        fn waking_and_unlocking_each_owe_a_refresh_and_make_none() {
            for signal in [QuarantineSignal::Woke, QuarantineSignal::SessionActivated] {
                let mut quarantine = RefreshQuarantineManager::default();
                assert_eq!(quarantine.on(signal), None, "{signal:?}");
                assert_eq!(quarantine.owed, Some(TRACKED), "{signal:?}");
            }
        }

        #[test]
        fn a_snapshot_that_leaves_a_hold_on_keeps_the_refresh_owed() {
            let cases = [
                (Wake::Asleep, Session::Active, true),
                (Wake::AwaitingSnapshot, Session::Active, false),
                (Wake::AwaitingSnapshot, Session::Inactive, true),
                (Wake::Awake, Session::AwaitingSnapshot, false),
            ];
            for (wake, session, releases_lifecycle) in cases {
                let mut quarantine = quarantine(wake, session, Churn::Churning);
                let made = quarantine.on(QuarantineSignal::Incorporated { releases_lifecycle });
                assert_eq!(made, None, "{wake:?} {session:?} {releases_lifecycle}");
                assert_eq!(quarantine.owed, Some(TRACKED));
            }
        }

        #[test]
        fn one_snapshot_releasing_the_wake_and_the_session_makes_one_refresh() {
            let mut quarantine =
                quarantine(Wake::AwaitingSnapshot, Session::AwaitingSnapshot, Churn::Steady);
            assert_eq!(
                quarantine.on(QuarantineSignal::Incorporated { releases_lifecycle: true }),
                Some(TRACKED)
            );
            assert_eq!(quarantine, RefreshQuarantineManager::default());
        }
    }
}
