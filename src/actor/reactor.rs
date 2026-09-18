//! The Reactor's job is to maintain coherence between the system and model state.
//!
//! It takes events from the rest of the system and builds a coherent picture of
//! what is going on. It shares this with the layout actor, and reacts to layout
//! changes by sending requests out to the other actors in the system.

mod animation;
mod events;
mod main_window;
mod managers;
mod query;
mod replay;
pub mod transaction_manager;
mod utils;

#[cfg(test)]
mod testing;

#[cfg(test)]
#[allow(non_snake_case)]
mod SpaceEventHandler {
    pub use super::events::space::WindowServerLifecyclePayload;

    pub fn handle_window_server_destroyed(
        reactor: &mut super::Reactor,
        payload: WindowServerLifecyclePayload,
    ) -> anyhow::Result<super::EventOutcome> {
        let wsid = payload.window_server_id;
        let tracked_window = reactor.state.windows.tracked_window_id(wsid);
        let assigned_space =
            tracked_window.and_then(|window| reactor.assigned_space_for_window_id(window));
        let observations = super::events::space::WindowServerDestroyedObservations {
            resolved_space: reactor.resolve_native_space(wsid, None),
            active_spaces: reactor.active_spaces.clone(),
            mission_control_active: reactor.is_mission_control_active(),
            ordered_in: rini_macos::window_server::window_ordered_in(wsid),
            assigned_space,
            last_known_user_space: super::events::space::resolve_last_known_user_space(
                tracked_window.and_then(|window| reactor.best_space_for_window_id(window)),
                reactor.space_state.iter_known_spaces().next(),
            ),
        };
        let outcome = super::events::space::handle_window_server_destroyed(
            &mut reactor.state,
            &reactor.transaction_manager,
            &mut reactor.drag_manager,
            payload,
            observations,
        )?;
        reactor.apply_event_outcome(outcome);
        Ok(super::EventOutcome::default())
    }

    pub fn handle_window_server_appeared(
        reactor: &mut super::Reactor,
        window_server_id: rini_macos::window_server::WindowServerId,
        space: rini_macos::screen::SpaceId,
        kind: super::SpaceEventKind,
    ) {
        reactor.handle_event(super::Event::WindowServerAppeared(window_server_id, space, kind));
    }
}

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::thread;

use events::{
    EventOutcome, app as application_workflow, command as command_workflow,
    drag as interaction_workflow, focus as focus_service, space as topology_workflow,
    system as system_workflow, window as window_workflow,
};
use main_window::MainWindowTracker;
use managers::LayoutManager;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
pub use replay::{Record, replay};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use tracing::{debug, info, instrument, trace, warn};
use transaction_manager::TransactionId;

use super::{event_tap, gesture_tap};
use crate::actor::app::{AppInfo, AppThreadHandle, Quiet, Request, WindowId, WindowInfo, pid_t};
use crate::actor::raise_manager::{self, RaiseManager, RaiseRequest};
use crate::actor::reactor::events::window_discovery;
use crate::actor::spaces::{ForwardedSpaceState, TopologyWindowDelta};
use crate::actor;
use rini_shared::collections::{BTreeMap, HashMap, HashSet};
use rini_config::Config;
use crate::layout_engine::{self as layout, Direction, LayoutEngine, LayoutEvent};
use crate::model::broadcast::{
    BroadcastEvent, BroadcastSender, protocol_window_id, protocol_workspace_id,
};
use crate::model::space_activation::{SpaceActivationConfig, SpaceActivationPolicy};
use crate::model::tx_store::WindowTxStore;
use crate::model::{AppRuleResult, RiniState};
use rini_macos::event::MouseState;
use rini_macos::executor::Executor;
use rini_shared::geometry::{CGRectDef, CGRectExt};
pub use rini_macos::screen::ScreenInfo;
use rini_macos::screen::{SpaceId, order_visible_spaces_by_position};
use rini_macos::window_server::{
    self, WindowServerId, WindowServerInfo, window_level, window_sub_level,
};

pub type Sender = actor::Sender<Event>;
type Receiver = actor::Receiver<Event>;
use managers::RefreshQuarantineState;
pub use query::ReactorQueryHandle;

pub(crate) use crate::model::reactor::{AppState, WindowFilter, WindowState};
pub use crate::model::reactor::{
    Command, DisplaySelector, DragSession, DragState, MenuState, MissionControlState,
    ReactorCommand, RefocusState, Requested, StaleCleanupState, WorkspaceSwitchOrigin,
    WorkspaceSwitchState,
};

#[derive(Clone)]
pub struct ReactorHandle {
    sender: Sender,
    queries: ReactorQueryHandle,
}

impl ReactorHandle {
    pub fn new(sender: Sender, queries: ReactorQueryHandle) -> Self {
        Self { sender, queries }
    }

    pub fn sender(&self) -> Sender {
        self.sender.clone()
    }

    pub fn send(&self, event: Event) {
        self.sender.send(event)
    }

    pub fn try_send(
        &self,
        event: Event,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<(tracing::Span, Event)>> {
        self.sender.try_send(event)
    }
}

impl std::ops::Deref for ReactorHandle {
    type Target = ReactorQueryHandle;

    fn deref(&self) -> &Self::Target {
        &self.queries
    }
}

use crate::model::server::RuntimeWindowData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceEventKind {
    User,
    Fullscreen,
}

#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub enum Event {
    /// Place these windows at these frames now, with no animation.
    ///
    /// Sent by the overlay animation actor once its animation is far enough along that the real
    /// windows are safely hidden behind it. Applying them any earlier let the windows visibly jump
    /// into place BEFORE the overlay appeared, so a switch read as a flicker followed by a slide.
    #[serde(skip)]
    ApplyOverlayFrames(Vec<(WindowId, CGRect)>),
    #[serde(skip)]
    SpaceStateChanged(ForwardedSpaceState),
    #[serde(skip)]
    ActiveDisplayChanged {
        menu_bar_space: Option<SpaceId>,
        command_space: Option<SpaceId>,
    },
    /// An application was launched. This event is also sent for every running
    /// application on startup.
    ///
    /// Both WindowInfo (accessibility) and WindowServerInfo are collected for
    /// any already-open windows when the launch event is sent. Since this
    /// event isn't ordered with respect to the Space events, it is possible to
    /// receive this event for a space we just switched off of.. FIXME. The same
    /// is true of WindowCreated events.
    ApplicationLaunched {
        pid: pid_t,
        info: AppInfo,
        #[serde(skip, default = "replay::deserialize_app_thread_handle")]
        handle: AppThreadHandle,
        is_frontmost: bool,
        main_window: Option<WindowId>,
        visible_windows: Vec<(WindowId, WindowInfo)>,
        window_server_info: Vec<WindowServerInfo>,
    },
    ApplicationTerminated(pid_t),
    ApplicationThreadTerminated(pid_t),
    ApplicationActivated(pid_t, Quiet),
    ApplicationDeactivated(pid_t),
    ApplicationGloballyActivated(pid_t),
    ApplicationGloballyDeactivated(pid_t),
    ApplicationMainWindowChanged(pid_t, Option<WindowId>, Quiet),
    /// Authoritative focus resolved from WindowServer's key-focus process and
    /// the z-ordered windows on the active native space.
    #[serde(skip)]
    WindowServerFocusChanged(WindowId, SpaceId),

    WindowsDiscovered {
        pid: pid_t,
        new: Vec<(WindowId, WindowInfo)>,
        known_visible: Vec<WindowId>,
    },
    WindowCreated(
        WindowId,
        WindowInfo,
        Option<WindowServerInfo>,
        Option<MouseState>,
    ),
    WindowDestroyed(WindowId),
    #[serde(skip)]
    WindowServerDestroyed(
        rini_macos::window_server::WindowServerId,
        SpaceId,
        SpaceEventKind,
    ),
    #[serde(skip)]
    WindowServerAppeared(
        rini_macos::window_server::WindowServerId,
        SpaceId,
        SpaceEventKind,
    ),
    #[serde(skip)]
    SpaceCreated(SpaceId),
    #[serde(skip)]
    SpaceDestroyed(SpaceId),
    WindowMinimized(WindowId),
    WindowDeminiaturized(WindowId),
    WindowFrameChanged(
        WindowId,
        #[serde(with = "CGRectDef")] CGRect,
        Option<TransactionId>,
        Requested,
        Option<MouseState>,
    ),
    WindowTitleChanged(WindowId, String),
    MenuOpened(pid_t),
    MenuClosed(pid_t),

    /// Left mouse button was released.
    ///
    /// Layout changes are suppressed while the button is down so that they
    /// don't interfere with drags. This event is used to update the layout in
    /// case updates were supressed while the button was down.
    ///
    /// FIXME: This can be interleaved incorrectly with the MouseState in app
    /// actor events.
    MouseUp,
    /// Sent by the event tap only when the cursor enters a different window.
    /// Window resolution and transition deduplication stay on the input
    /// thread; the reactor only applies the model-dependent focus/raise work.
    MouseMoved(WindowServerId),
    /// Forwarded by the spaces actor after wake has been observed.
    ///
    /// The spaces actor is the authority for sleep/lock/display lifecycle.
    /// The reactor uses this only to reopen refresh gating and resubscribe
    /// WindowServer notifications once the topology authority says wake
    /// processing has advanced.
    SystemWoke,
    #[serde(skip)]
    SystemWillSleep,
    #[serde(skip)]
    SessionDidResignActive,
    #[serde(skip)]
    SessionDidBecomeActive,

    #[serde(skip)]
    DisplayChurnBegin,
    #[serde(skip)]
    DisplayChurnEnd,

    #[serde(skip)]
    MissionControlNativeEntered,
    #[serde(skip)]
    MissionControlNativeExited,

    /// A raise request completed. Used by the raise manager to track when
    /// all raise requests in a sequence have finished.
    RaiseCompleted {
        window_id: WindowId,
        sequence_id: u64,
    },

    /// A raise sequence timed out. Used by the raise manager to clean up
    /// pending raises that took too long.
    RaiseTimeout {
        sequence_id: u64,
    },

    #[serde(skip)]
    Query(query::QueryRequest),

    Command(Command),

    #[serde(skip)]
    RegisterWmSender(crate::actor::wm_controller::Sender),

    #[serde(skip)]
    ConfigUpdated(Config),
}

pub struct Reactor {
    pub config: Config,
    pub one_space: bool,
    app_manager: managers::AppManager,
    layout_manager: managers::LayoutManager,
    pub(crate) state: RiniState,
    space_state: ForwardedSpaceState,
    /// Where each WORKSPACE's strip viewport sat the last time a layout pass looked.
    ///
    /// The animation path needs the DISTANCE the strip is moving. Reading that off the windows is
    /// unreliable: macOS clamps the ones it will not place off screen, and the layout is recomputed several
    /// times per keystroke, so the same press produced five different answers. The scroll offset is the
    /// layout's own description of the movement.
    ///
    /// Keyed by workspace, not by space, because every workspace has its own strip. Keyed by space alone,
    /// the first pass after a workspace switch compared the new workspace's offset against the old one's
    /// and animated the difference as a horizontal pan that no window had made.
    last_strip_offset: HashMap<(SpaceId, crate::model::VirtualWorkspaceId), f64>,
    space_activation_policy: SpaceActivationPolicy,
    main_window_tracker: MainWindowTracker,
    /// The focus reports rini's own raises are about to produce, so they are not mistaken for the user
    /// moving. See [`main_window::RaiseEcho`].
    raise_echo: main_window::RaiseEcho,
    /// The strip windows the current event's regroup already raised, so the post-layout judgment does
    /// not raise them a second time. See `regroup_after_layout`.
    regroup_raised: Vec<WindowId>,
    drag_manager: managers::DragManager,
    workspace_switch_manager: managers::WorkspaceSwitchManager,
    recording_manager: managers::RecordingManager,
    communication_manager: managers::CommunicationManager,
    /// Last active workspace index per space, so a switch can be recognised without intercepting the
    /// command that caused it.
    last_active_workspace: HashMap<SpaceId, usize>,
    notification_manager: managers::NotificationManager,
    transaction_manager: transaction_manager::TransactionManager,
    menu_manager: managers::MenuManager,
    mission_control_manager: managers::MissionControlManager,
    refocus_manager: managers::RefocusManager,
    refresh_quarantine_manager: managers::RefreshQuarantineManager,
    pending_space_change_manager: managers::PendingSpaceChangeManager,
    active_spaces: HashSet<SpaceId>,
    /// When the layout file was last written, for debouncing autosaves.
    last_autosave: Option<std::time::Instant>,
    /// A layout change arrived inside the debounce window and has not been written
    /// yet. Flushed on shutdown and on display reconfiguration.
    autosave_pending: bool,
    /// Where autosave writes. `None` disables it.
    ///
    /// A field rather than a call to `config::restore_file()` at the write site because
    /// autosave fires from `update_layout_or_warn_with`, which almost every test drives.
    /// Resolving the real path there meant the suite overwrote the user's own
    /// ~/.rini/layout.ron with test fixtures.
    autosave_path: Option<PathBuf>,
}

impl Reactor {
    pub fn spawn(
        config: Config,
        layout_engine: LayoutEngine,
        record: Record,
        event_tap_tx: event_tap::Sender,
        broadcast_tx: BroadcastSender,
        cursor_warp_tx: Option<crate::actor::cursor_warp::Sender>,
        workspace_animation_tx: Option<crate::actor::workspace_animation::Sender>,
        window_notify: Option<(crate::actor::window_notify::Sender, WindowTxStore)>,
        gesture_tap_tx: Option<gesture_tap::Sender>,
        one_space: bool,
    ) -> ReactorHandle {
        let (events_tx, events) = actor::channel();
        let events_tx_clone = events_tx.clone();
        let mut reactor = Reactor::new(
            config,
            layout_engine,
            record,
            broadcast_tx,
            window_notify,
            one_space,
        );
        reactor.communication_manager.event_tap_tx = Some(event_tap_tx);
        reactor.communication_manager.cursor_warp_tx = cursor_warp_tx;
        reactor.communication_manager.workspace_animation_tx = workspace_animation_tx;
        reactor.communication_manager.gesture_tap_tx = gesture_tap_tx;
        reactor.communication_manager.events_tx = Some(events_tx_clone.clone());
        let query_handle = ReactorQueryHandle::new(events_tx_clone.clone());
        thread::Builder::new()
            .name("reactor".to_string())
            .spawn(move || {
                Executor::run(Reactor::run(reactor, events, events_tx_clone));
            })
            .unwrap();
        ReactorHandle::new(events_tx, query_handle)
    }

    pub fn new(
        config: Config,
        layout_engine: LayoutEngine,
        mut record: Record,
        broadcast_tx: BroadcastSender,
        window_notify: Option<(crate::actor::window_notify::Sender, WindowTxStore)>,
        one_space: bool,
    ) -> Reactor {
        // FIXME: Remove apps that are no longer running from restored state.
        record.start(&config, &layout_engine);
        let (raise_manager_tx, _rx) = actor::channel();
        let (window_notify_tx, window_tx_store) = match window_notify {
            Some((tx, store)) => (Some(tx), store),
            None => (None, WindowTxStore::new()),
        };
        let reactor = Reactor {
            config: config.clone(),
            one_space,
            app_manager: managers::AppManager::new(),
            layout_manager: managers::LayoutManager { layout_engine },
            state: RiniState::default(),
            space_state: ForwardedSpaceState::default(),
            last_strip_offset: HashMap::default(),
            space_activation_policy: SpaceActivationPolicy::new(),
            main_window_tracker: MainWindowTracker::default(),
            raise_echo: main_window::RaiseEcho::default(),
            regroup_raised: Vec::new(),
            drag_manager: managers::DragManager {
                drag_state: DragState::Inactive,
                drag_swap_manager: crate::actor::drag_swap::DragManager::new(
                    config.settings.window_snapping,
                ),
                skip_layout_for_window: None,
            },
            workspace_switch_manager: managers::WorkspaceSwitchManager {
                workspace_switch_state: WorkspaceSwitchState::Inactive,
                workspace_switch_generation: 0,
                active_workspace_switch: None,
                pending_workspace_switch_origin: None,
                pending_workspace_mouse_warp: None,
            },
            recording_manager: managers::RecordingManager { record },
            last_active_workspace: HashMap::default(),
            communication_manager: managers::CommunicationManager {
                event_tap_tx: None,
                gesture_tap_tx: None,
                cursor_warp_tx: None,
                workspace_animation_tx: None,
                raise_manager_tx,
                event_broadcaster: broadcast_tx,
                wm_sender: None,
                events_tx: None,
            },
            notification_manager: managers::NotificationManager {
                last_sls_notification_ids: Vec::new(),
                _window_notify_tx: window_notify_tx,
            },
            transaction_manager: transaction_manager::TransactionManager::new(window_tx_store),
            menu_manager: managers::MenuManager {
                menu_state: MenuState::Closed,
            },
            mission_control_manager: managers::MissionControlManager {
                mission_control_state: MissionControlState::Inactive,
                pending_mission_control_refresh: HashSet::default(),
            },
            refocus_manager: managers::RefocusManager {
                stale_cleanup_state: StaleCleanupState::Enabled,
                refocus_state: RefocusState::None,
            },
            refresh_quarantine_manager: managers::RefreshQuarantineManager {
                sleeping: false,
                session_inactive: false,
                display_churn_active: false,
                awaiting_post_wake_snapshot: false,
                awaiting_post_session_snapshot: false,
                pending_visible_refresh: false,
                deferred_refresh_tracks_mission_control: false,
            },
            pending_space_change_manager: managers::PendingSpaceChangeManager {
                pending_space_change: None,
            },
            active_spaces: HashSet::default(),
            last_autosave: None,
            autosave_pending: false,
            #[cfg(not(test))]
            autosave_path: Some(rini_config::restore_file()),
            // Tests drive update_layout, which autosaves. Never let the suite write to the
            // real layout file; a test that wants to exercise autosave sets a temp path.
            #[cfg(test)]
            autosave_path: None,
        };
        reactor
    }

    fn set_active_spaces(&mut self, spaces: &[Option<SpaceId>]) {
        self.active_spaces.clear();
        for space in spaces.iter().flatten().copied() {
            self.active_spaces.insert(space);
        }
    }

    fn is_space_active(&self, space: SpaceId) -> bool {
        self.active_spaces.contains(&space)
    }

    fn iter_active_spaces(&self) -> impl Iterator<Item = SpaceId> + '_ {
        self.active_spaces.iter().copied()
    }

    fn active_space_ids(&self) -> Vec<u64> {
        self.active_spaces.iter().map(|space| space.get()).collect()
    }

    fn is_window_on_active_space(&self, wid: WindowId) -> bool {
        self.best_space_for_window_id(wid)
            .is_some_and(|space| self.is_space_active(space))
    }

    fn activation_cfg(&self) -> SpaceActivationConfig {
        SpaceActivationConfig {
            default_disable: self.config.settings.default_disable,
            one_space: self.one_space,
        }
    }

    fn screens_for_current_spaces(&self) -> Vec<ScreenInfo> {
        self.space_state.screens.clone()
    }

    fn display_uuids_for_current_screens(&self) -> Vec<Option<String>> {
        self.space_state
            .screens
            .iter()
            .map(|screen| screen.display_uuid_owned())
            .collect()
    }

    #[cfg(test)]
    fn raw_spaces_for_current_screens(&self) -> Vec<Option<SpaceId>> {
        self.space_state.screens.iter().map(|s| s.space).collect()
    }

    fn display_uuid_for_space(&self, space: SpaceId) -> Option<String> {
        self.space_state
            .screen_by_space(space)
            .and_then(|screen| screen.display_uuid_owned())
    }

    fn expose_space_if_known(&mut self, space: SpaceId) {
        let Some(screen) = self.space_state.screen_by_space(space) else {
            return;
        };
        self.layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .list_workspaces(space);
        self.send_layout_event(LayoutEvent::SpaceExposed(space, screen.frame.size));
    }

    fn recompute_and_set_active_spaces(&mut self, spaces: &[Option<SpaceId>]) {
        let cfg = self.activation_cfg();
        let display_uuids = self.display_uuids_for_current_screens();
        let active_spaces =
            self.space_activation_policy.compute_active_spaces(cfg, spaces, &display_uuids);
        let previous_active = self.active_spaces.clone();
        self.set_active_spaces(&active_spaces);
        self.handle_active_space_change(previous_active);
    }

    fn recompute_and_set_active_spaces_from_current_screens(&mut self) {
        let raw_spaces = self.authoritative_spaces_for_current_screens();
        self.recompute_and_set_active_spaces(&raw_spaces);
    }

    fn authoritative_spaces_for_current_screens(&self) -> Vec<Option<SpaceId>> {
        self.space_state
            .screens
            .iter()
            .map(|screen| {
                screen.space.filter(|space| self.space_state.active_spaces.contains(space))
            })
            .collect()
    }

    fn handle_active_space_change(&mut self, previous_active: HashSet<SpaceId>) {
        if previous_active == self.active_spaces {
            return;
        }

        let deactivated: Vec<SpaceId> =
            previous_active.difference(&self.active_spaces).copied().collect();
        let activated: Vec<SpaceId> =
            self.active_spaces.difference(&previous_active).copied().collect();

        // Do not remove windows when a space is merely deactivated (e.g. macOS Space
        // switches). Removing them clears workspace assignments and causes windows
        // without app rules to be re-assigned to the current workspace.

        if !activated.is_empty() {
            for space in &activated {
                self.expose_space_if_known(*space);
            }
        }

        if !activated.is_empty() || !deactivated.is_empty() {
            self.refresh_window_server_snapshot_for_active_spaces();
            self.check_for_new_windows();
        }

        if !activated.is_empty() {
            self.apply_app_rules_for_activated_spaces(&activated);
        }
    }

    fn apply_app_rules_for_activated_spaces(&mut self, activated: &[SpaceId]) {
        let activated_set: HashSet<SpaceId> = activated.iter().copied().collect();
        let mut windows_by_pid: HashMap<pid_t, Vec<WindowId>> = HashMap::default();

        for (wid, state) in self.state.windows.iter_windows() {
            if !state.matches_filter(WindowFilter::Manageable) {
                continue;
            }
            let Some(space) = self.best_space_for_window_id(wid) else {
                continue;
            };

            if !activated_set.contains(&space) {
                continue;
            }

            windows_by_pid.entry(wid.pid).or_default().push(wid);
        }

        for (pid, window_ids) in windows_by_pid {
            let Some(app_state) = self.app_manager.apps.get(&pid) else {
                continue;
            };

            self.process_windows_for_app_rules(pid, window_ids, app_state.info.clone());
        }
    }

    fn refresh_window_server_snapshot_for_active_spaces(&mut self) {
        let active_windows = self.authoritative_active_space_windows();
        self.reconcile_authoritative_active_window_snapshot(active_windows, false);
    }

    fn authoritative_active_space_windows(&self) -> Vec<(WindowServerId, Option<SpaceId>)> {
        let mut queried = HashMap::default();
        for space in self.iter_active_spaces() {
            for wsid in window_server::space_window_list_for_connection(&[space.get()], 0, false)
                .into_iter()
                .map(WindowServerId::new)
            {
                queried.entry(wsid).or_insert(space);
            }
        }

        // A refresh can be partial while WindowServer is waking. Keep the last
        // forwarded per-space sample in that case, but never use the global
        // visible-window union as a substitute for querying each active space.
        let membership = if queried.is_empty() {
            self.space_state.active_window_spaces.clone()
        } else {
            queried
        };

        let mut membership: Vec<_> = membership
            .into_iter()
            .map(|(wsid, space)| (wsid, self.resolve_native_space(wsid, Some(space))))
            .collect();
        membership.sort_by_key(|(wsid, _)| *wsid);
        membership
    }

    fn has_known_windows_for_active_spaces(&self) -> bool {
        self.state.windows.iter_windows().any(|(wid, _)| {
            self.authoritative_space_for_window_id(wid)
                .is_some_and(|space| self.is_space_active(space))
        })
    }

    fn refresh_active_space_window_membership(
        &mut self,
        active_windows: Vec<(WindowServerId, Option<SpaceId>)>,
    ) {
        let active_wsids: HashSet<WindowServerId> =
            active_windows.iter().map(|(wsid, _)| *wsid).collect();

        // An empty active-space list is valid, but an empty WS-id result while we
        // already know about windows assigned to the active space is typically the
        // transient post-wake race on same-display space switches. Preserve the
        // existing visibility basis in that case and let the follow-up AX refresh
        // reconcile instead of blanking the workspace immediately.
        if active_wsids.is_empty() && self.has_known_windows_for_active_spaces() {
            return;
        }

        let previously_visible_wsids: Vec<_> =
            self.state.windows.iter_visible_window_server_ids().collect();
        for wsid in previously_visible_wsids {
            if !active_wsids.contains(&wsid) {
                self.state.windows.mark_window_hidden(wsid);
            }
        }

        for (wsid, space) in active_windows {
            let space = self.resolve_native_space(wsid, space);
            if let Some(space) = space {
                self.state.windows.set_window_server_space(wsid, Some(space));
                self.clear_pending_target_if_confirmed_space(wsid, space);
            }
            self.state.windows.mark_window_visible(wsid);
            self.state.windows.clear_window_server_observed(wsid);
        }
    }

    fn remove_windows_missing_from_active_space_snapshot(
        &mut self,
        previously_visible_wsids: Vec<WindowServerId>,
        preserve_assignments: bool,
    ) {
        for wsid in previously_visible_wsids {
            if self.state.windows.is_window_visible(wsid) {
                continue;
            }
            let Some(wid) = self.state.windows.tracked_window_id(wsid) else {
                continue;
            };
            let Some(space) = self.assigned_space_for_window_id(wid) else {
                continue;
            };
            if !self.is_space_active(space) {
                continue;
            }

            let inactive_target = self
                .resolve_native_space(wsid, None)
                .filter(|current_space| *current_space != space)
                .filter(|current_space| {
                    #[cfg(test)]
                    {
                        let _ = current_space;
                        true
                    }
                    #[cfg(not(test))]
                    {
                        window_server::space_is_user(current_space.get())
                    }
                })
                .filter(|current_space| !self.is_space_active(*current_space));
            if let Some(current_space) = inactive_target {
                self.state.windows.set_window_server_space(wsid, Some(current_space));
                let _ = self.reassign_window_to_authoritative_space(wid, current_space);
                continue;
            }

            if preserve_assignments {
                debug!(
                    ?wid,
                    ?wsid,
                    "Preserving workspace assignment omitted from partial authoritative snapshot"
                );
                continue;
            }

            // If the authoritative active-space snapshot no longer includes a
            // previously visible window and WindowServer cannot confirm a new
            // native space for it, drop the stale origin-space ownership. Keeping
            // the old assignment lets later discovery/MC refresh rebuild the
            // origin layout from stale workspace state.
            self.state.windows.set_window_server_space(wsid, None);
            self.send_layout_event(LayoutEvent::WindowRemoved(wid));
        }
    }

    fn reconcile_authoritative_active_window_snapshot(
        &mut self,
        active_windows: Vec<(WindowServerId, Option<SpaceId>)>,
        preserve_missing_assignments: bool,
    ) {
        let previously_visible_wsids: Vec<_> =
            self.state.windows.iter_visible_window_server_ids().collect();
        self.refresh_active_space_window_membership(active_windows);
        self.remove_windows_missing_from_active_space_snapshot(
            previously_visible_wsids,
            preserve_missing_assignments,
        );
        self.reconcile_windows_with_authoritative_spaces();
    }

    fn is_login_window_pid(&self, pid: pid_t) -> bool {
        self.app_manager.apps.get(&pid).and_then(|a| a.info.bundle_id.as_deref())
            == Some("com.apple.loginwindow")
    }

    // fn store_txid(&self, wsid: Option<WindowServerId>, txid: TransactionId, target: CGRect) {
    //     self.transaction_manager.store_txid(wsid, txid, target);
    // }
    //
    // fn update_txid_entries<I>(&self, entries: I)
    // where
    //     I: IntoIterator<Item = (WindowServerId, TransactionId, CGRect)>,
    // {
    //     self.transaction_manager.update_entries(entries);
    // }
    //
    // fn remove_txid_for_window(&self, wsid: Option<WindowServerId>) {
    //     self.transaction_manager.remove_for_window(wsid);
    // }

    fn clear_pending_hidden_window_targets(&self) {
        for (wid, window) in self.state.windows.iter_windows() {
            if self.hidden_assigned_space_for_window_id(wid).is_none() {
                continue;
            }
            if let Some(wsid) = window.info.sys_id {
                self.transaction_manager.clear_target_for_window(wsid);
            }
        }
    }

    fn clear_pending_target_if_confirmed_space(
        &self,
        wsid: WindowServerId,
        confirmed_space: SpaceId,
    ) {
        if self.pending_target_space_for_window_server_id(wsid) == Some(confirmed_space) {
            self.transaction_manager.clear_target_for_window(wsid);
        }
    }

    fn is_in_drag(&self) -> bool {
        matches!(
            self.drag_manager.drag_state,
            DragState::Active { .. } | DragState::PendingSwap { .. }
        )
    }

    fn is_mission_control_active(&self) -> bool {
        matches!(
            self.mission_control_manager.mission_control_state,
            MissionControlState::Active
        )
    }

    fn get_pending_drag_swap(&self) -> Option<(WindowId, WindowId)> {
        if let DragState::PendingSwap { session, target } = &self.drag_manager.drag_state {
            Some((session.window, *target))
        } else {
            None
        }
    }

    fn get_active_drag_session(&self) -> Option<&DragSession> {
        if let DragState::Active { session } = &self.drag_manager.drag_state {
            Some(session)
        } else {
            None
        }
    }

    fn take_active_drag_session(&mut self) -> Option<DragSession> {
        match std::mem::replace(&mut self.drag_manager.drag_state, DragState::Inactive) {
            DragState::Active { session } => Some(session),
            DragState::PendingSwap { session, .. } => Some(session),
            _ => None,
        }
    }

    async fn run(mut reactor: Reactor, events: Receiver, events_tx: Sender) {
        let (raise_manager_tx, raise_manager_rx) = actor::channel();
        reactor.communication_manager.raise_manager_tx = raise_manager_tx.clone();
        let event_tap_tx = reactor.communication_manager.event_tap_tx.clone();
        let reactor_task = Self::run_reactor_loop(reactor, events);
        let raise_manager_task = RaiseManager::run(raise_manager_rx, events_tx, event_tap_tx);
        let _ = tokio::join!(reactor_task, raise_manager_task);
    }

    async fn run_reactor_loop(mut reactor: Reactor, mut events: Receiver) {
        const MAX_EVENT_BATCH: usize = 64;

        while let Some((span, event)) = events.recv().await {
            let _guard = span.enter();
            reactor.handle_loop_event(event);
            // Drain a bounded batch to reduce recv/select overhead.
            for _ in 1..MAX_EVENT_BATCH {
                let Ok((span, event)) = events.try_recv() else {
                    break;
                };
                let _guard = span.enter();
                reactor.handle_loop_event(event);
            }
        }
    }

    fn handle_loop_event(&mut self, event: Event) {
        if let Event::Query(req) = event {
            self.handle_query_request(req);
            return;
        }
        if self.should_quarantine_space_lifecycle_event(&event) {
            trace!(?event, state = ?self.refresh_quarantine_state(), "quarantined space lifecycle event");
            return;
        }
        if self.should_quarantine_during_display_churn(&event) {
            trace!(?event, "quarantined during display churn");
            return;
        }
        Self::note_windowserver_activity(&event);
        self.handle_event(event);
        #[cfg(any(test, debug_assertions))]
        self.state.windows.debug_assert_invariants();
    }

    fn note_windowserver_activity(event: &Event) {
        let wsid = match event {
            Event::WindowFrameChanged(wid, ..) => Some(wid.idx.get()),
            Event::WindowCreated(wid, ..) => Some(wid.idx.get()),
            Event::WindowDestroyed(wid) => Some(wid.idx.get()),
            Event::WindowMinimized(wid) => Some(wid.idx.get()),
            Event::WindowDeminiaturized(wid) => Some(wid.idx.get()),
            Event::MouseMoved(_) => None,
            Event::WindowServerDestroyed(wsid, ..) => Some(wsid.as_u32()),
            Event::WindowServerAppeared(wsid, ..) => Some(wsid.as_u32()),
            _ => None,
        };
        if let Some(wsid) = wsid {
            window_server::note_windowserver_activity(wsid);
        }
    }

    fn log_event(&self, event: &Event) {
        match event {
            Event::WindowFrameChanged(..) | Event::MouseUp | Event::MouseMoved(_) => {
                trace!(?event, "Event")
            }
            _ => debug!(?event, "Event"),
        }
    }

    fn should_update_notifications(event: &Event) -> bool {
        matches!(
            event,
            Event::WindowCreated(..)
                | Event::WindowDestroyed(..)
                | Event::WindowServerDestroyed(..)
                | Event::WindowServerAppeared(..)
                | Event::WindowsDiscovered { .. }
                | Event::ApplicationLaunched { .. }
                | Event::ApplicationTerminated(..)
                | Event::ApplicationThreadTerminated(..)
                | Event::SpaceStateChanged(..)
        )
    }

    fn should_quarantine_during_display_churn(&self, event: &Event) -> bool {
        if !rini_macos::display_churn::is_active() {
            return false;
        }

        matches!(
            event,
            Event::WindowCreated(..)
                | Event::WindowDestroyed(..)
                | Event::WindowServerDestroyed(..)
                | Event::WindowServerAppeared(..)
                | Event::WindowFrameChanged(..)
                | Event::WindowMinimized(..)
                | Event::WindowDeminiaturized(..)
                | Event::WindowTitleChanged(..)
                | Event::WindowsDiscovered { .. }
                | Event::SpaceCreated(..)
                | Event::SpaceDestroyed(..)
        )
    }

    fn should_quarantine_space_lifecycle_event(&self, event: &Event) -> bool {
        self.refreshes_blocked()
            && matches!(event, Event::SpaceCreated(..) | Event::SpaceDestroyed(..))
    }

    fn refresh_quarantine_state(&self) -> RefreshQuarantineState {
        self.refresh_quarantine_manager.state()
    }

    fn refreshes_blocked(&self) -> bool {
        self.refresh_quarantine_manager.blocks_refreshes()
    }

    fn defer_visible_refresh(&mut self, track_mission_control_refresh: bool) {
        self.refresh_quarantine_manager.pending_visible_refresh = true;
        self.refresh_quarantine_manager.deferred_refresh_tracks_mission_control |=
            track_mission_control_refresh;
    }

    fn flush_deferred_visible_refresh(&mut self) {
        if self.refreshes_blocked() {
            return;
        }

        if self.refresh_quarantine_manager.pending_visible_refresh {
            let track_mission_control_refresh =
                self.refresh_quarantine_manager.deferred_refresh_tracks_mission_control;
            self.refresh_quarantine_manager.pending_visible_refresh = false;
            self.refresh_quarantine_manager.deferred_refresh_tracks_mission_control = false;
            self.request_visible_windows_for_apps(track_mission_control_refresh);
        }
    }

    // All lifecycle churn is upstreamed through the spaces actor. The reactor
    // only remembers that one visibility refresh is owed, then flushes it once
    // every upstream gate is open again.
    fn request_refresh_when_spaces_actor_stabilizes(&mut self) {
        self.defer_visible_refresh(true);
        self.flush_deferred_visible_refresh();
    }

    fn release_post_instability_quarantine_after_authoritative_snapshot(&mut self) {
        let released_wake = self.refresh_quarantine_manager.awaiting_post_wake_snapshot;
        let released_session = self.refresh_quarantine_manager.awaiting_post_session_snapshot;

        if !released_wake && !released_session {
            return;
        }

        self.refresh_quarantine_manager.awaiting_post_wake_snapshot = false;
        self.refresh_quarantine_manager.awaiting_post_session_snapshot = false;
        if released_wake {
            self.refresh_quarantine_manager.sleeping = false;
        }
        if released_session {
            self.refresh_quarantine_manager.session_inactive = false;
        }
        self.flush_deferred_visible_refresh();
    }

    #[instrument(name = "reactor::handle_event", skip(self), fields(event=?event))]
    fn handle_event(&mut self, event: Event) {
        let previously_focused_window = self.main_window();
        match self.dispatch_workflow(event) {
            Ok(mut outcome) => {
                let focused_window = self.main_window();
                if focused_window != previously_focused_window
                    && let Some(focused_window) = focused_window
                {
                    outcome = outcome.with_focused_window_broadcast(focused_window);
                }
                self.apply_event_outcome(outcome);
            }
            Err(error) => warn!(%error, "reactor workflow failed"),
        }
    }

    fn dispatch_workflow(&mut self, event: Event) -> anyhow::Result<EventOutcome> {
        self.log_event(&event);
        self.recording_manager.record.on_event(&event);

        match event {
            Event::SystemWillSleep => {
                self.refresh_quarantine_manager.sleeping = true;
                self.refresh_quarantine_manager.awaiting_post_wake_snapshot = false;
                // Sleep is the last chance to persist before a possible reboot, so
                // write out any debounced change rather than risk losing it.
                if self.autosave_pending {
                    self.save_layout_now();
                }
                return Ok(EventOutcome::default());
            }
            Event::SystemWoke => {
                self.refresh_quarantine_manager.sleeping = true;
                self.refresh_quarantine_manager.awaiting_post_wake_snapshot = true;
                let outcome = system_workflow::handle_system_woke()?;
                self.defer_visible_refresh(true);
                return Ok(outcome);
            }
            Event::SessionDidResignActive => {
                self.refresh_quarantine_manager.session_inactive = true;
                self.refresh_quarantine_manager.awaiting_post_session_snapshot = false;
                return Ok(EventOutcome::default());
            }
            Event::SessionDidBecomeActive => {
                self.refresh_quarantine_manager.session_inactive = true;
                self.refresh_quarantine_manager.awaiting_post_session_snapshot = true;
                self.defer_visible_refresh(true);
                return Ok(EventOutcome::default());
            }
            Event::DisplayChurnBegin => {
                self.refresh_quarantine_manager.display_churn_active = true;
                return Ok(EventOutcome::default());
            }
            Event::DisplayChurnEnd => {
                self.refresh_quarantine_manager.display_churn_active = false;
                self.request_refresh_when_spaces_actor_stabilizes();
                return Ok(EventOutcome::default());
            }
            _ => {}
        }

        let should_update_notifications = Self::should_update_notifications(&event);
        let duplicate_global_activation = matches!(
            &event,
            Event::ApplicationGloballyActivated(pid)
                if self.main_window_tracker.is_globally_frontmost(*pid)
        );

        let raised_window = self.main_window_tracker.handle_event(&event);
        match event {
            Event::ApplicationLaunched {
                pid,
                info,
                handle,
                visible_windows,
                window_server_info,
                is_frontmost,
                main_window,
            } => {
                let _ = (is_frontmost, main_window);
                let mut outcome = application_workflow::handle_application_launched(
                    &mut self.app_manager,
                    application_workflow::ApplicationLaunchedPayload {
                        pid,
                        info,
                        handle,
                        visible_windows,
                        window_server_info,
                    },
                )?;
                if self.main_window_tracker.is_globally_frontmost(pid) {
                    outcome.app_requests.push((pid, Request::ApplicationGloballyActivated(pid)));
                }
                outcome.focused_window = raised_window;
                return Ok(outcome);
            }
            Event::ApplicationTerminated(pid) => {
                return application_workflow::handle_application_terminated(pid);
            }
            Event::ApplicationThreadTerminated(pid) => {
                self.clear_menu_state_for_pid(pid);
                return application_workflow::handle_application_thread_terminated(
                    &mut self.app_manager,
                    pid,
                );
            }
            Event::ApplicationActivated(pid, quiet) => {
                self.clear_menu_state_for_non_owner(pid);
                let mut outcome = application_workflow::handle_application_activated(
                    application_workflow::ApplicationActivatedPayload { pid, quiet },
                )?;
                if quiet == Quiet::No {
                    outcome.absorb(self.handle_app_activation_workspace_switch(pid));
                }
                outcome.focused_window = raised_window;
                return Ok(outcome);
            }
            Event::ApplicationDeactivated(pid) => {
                self.clear_menu_state_for_pid(pid);
            }
            Event::ApplicationGloballyDeactivated(pid) => {
                self.clear_menu_state_for_pid(pid);
            }
            Event::ApplicationGloballyActivated(pid) => {
                if duplicate_global_activation {
                    trace!(pid, "Ignoring duplicate global application activation");
                    return Ok(EventOutcome::focus_changed(None, should_update_notifications));
                }
                self.clear_menu_state_for_non_owner(pid);
                if !self.is_login_window_pid(pid) {
                    if let Some(app) = self.app_manager.apps.get(&pid) {
                        let _ = app.handle.send(Request::ApplicationGloballyActivated(pid));
                    }
                }
                // The app thread will resolve the current AX main window and
                // emit ApplicationActivated. Do not replay cached focus here.
                return Ok(EventOutcome::focus_changed(None, should_update_notifications));
            }
            Event::WindowServerFocusChanged(window, reported_space) => {
                if self.layout_manager.layout_engine.focused_window() == Some(window) {
                    if let Some(event_tap_tx) = &self.communication_manager.event_tap_tx {
                        _ = event_tap_tx.send(crate::actor::event_tap::Request::EnforceHidden);
                    }
                    return Ok(EventOutcome::default());
                }
                if !self.state.windows.contains_window(window) {
                    if let Some(app) = self.app_manager.apps.get(&window.pid) {
                        let _ = app.handle.send(Request::GetVisibleWindows);
                    }
                    return Ok(EventOutcome::default());
                }
                if !self.is_space_active(reported_space) {
                    return Ok(EventOutcome::default());
                }
                // rini's own raise, coming back as a focus report. Following it moves the layout's
                // selection to a window the user did not ask for, and the strip scrolls to it.
                if self.raise_echo.swallows(window, std::time::Instant::now()) {
                    trace!(?window, "ignoring a focus report provoked by rini's own raise");
                    return Ok(EventOutcome::default());
                }
                // Whichever window gains or loses focus renders differently now, so both need a fresh
                // picture before the next animation draws them.
                self.refresh_focus_pictures(window);
                // An app activation picked its own window, and it may not be the one the user was in.
                if let Some(redirect) = self.activation_redirect(window, reported_space) {
                    return Ok(redirect);
                }
                // Follow focus to the window's own workspace.
                //
                // Auto-switching only happened on APP activation, so cmd-` — which cycles
                // windows inside the already-active app — moved focus to a window sitting in
                // another workspace without the display switching to it. The window is parked
                // off-screen, so focus went somewhere invisible and the keystroke looked like
                // it had done nothing.
                let outcome =
                    self.maybe_auto_switch_to_window_workspace(window.pid, window, reported_space);
                // Once per activation: whatever was not used above is not used later either.
                self.main_window_tracker.take_activation_target(window.pid);
                if outcome.arrange.requested {
                    return Ok(outcome);
                }
                // A click raises only the window under the cursor, which splits the strip around a
                // floating window. The strip is one group, so `apply_event_outcome` lifts all of it
                // once this focus has landed (`regroup_after_layout`).
                return Ok(EventOutcome::default()
                    .with_layout_event(LayoutEvent::WindowFocused(reported_space, window)));
            }
            Event::RegisterWmSender(sender) => {
                return Ok(system_workflow::handle_register_wm_sender(
                    &mut self.communication_manager,
                    sender,
                )?);
            }
            Event::WindowsDiscovered { pid, new, known_visible } => {
                if self.refreshes_blocked() {
                    debug!(
                        pid,
                        state = ?self.refresh_quarantine_state(),
                        "Ignoring windows discovery while refresh quarantine is active"
                    );
                    self.defer_visible_refresh(true);
                    return Ok(EventOutcome::default());
                }
                let mut outcome = application_workflow::handle_windows_discovered(
                    application_workflow::WindowsDiscoveredPayload { pid, new, known_visible },
                )?;
                outcome.focused_window = raised_window;
                return Ok(outcome);
            }
            Event::WindowCreated(wid, window, ws_info, mouse_state) => {
                let _ = mouse_state;
                let mut outcome = window_workflow::handle_window_created(
                    &mut self.state,
                    &self.transaction_manager,
                    window_workflow::WindowCreatedPayload {
                        window_id: wid,
                        window,
                        window_server_info: ws_info,
                    },
                )?;
                outcome.focused_window = raised_window;
                return Ok(outcome);
            }
            Event::WindowDestroyed(wid) => {
                // macOS can replace AXUIElements during lifecycle/display churn while the
                // native window remains alive. Recovery already schedules a stable refresh,
                // so preserve topology until then. Outside churn, retain the original AX
                // destruction behavior and remove the window immediately.
                if self.refreshes_blocked() {
                    return Ok(EventOutcome::default());
                }
                // A closed window disappears; only its cached picture has to go. Once: the
                // window-server path may have removed the window, and forgotten it, already. See
                // "A closed window disappears" in `docs/animation-smoothness.md`.
                if self.state.windows.window(wid).is_some()
                    && let Some(tx) = &self.communication_manager.workspace_animation_tx
                {
                    _ = tx.send(crate::actor::workspace_animation::Event::ForgetWindow(wid));
                }
                let mut outcome = window_workflow::handle_window_destroyed(
                    &mut self.state,
                    &self.transaction_manager,
                    &mut self.drag_manager,
                    window_workflow::WindowDestroyedPayload { window: wid },
                )?;
                outcome.focused_window = raised_window;
                return Ok(outcome);
            }
            Event::WindowServerDestroyed(wsid, sid, kind) => {
                let tracked_window = self.state.windows.tracked_window_id(wsid);
                let assigned_space =
                    tracked_window.and_then(|window| self.assigned_space_for_window_id(window));
                let last_known_user_space = topology_workflow::resolve_last_known_user_space(
                    tracked_window.and_then(|window| self.best_space_for_window_id(window)),
                    self.space_state.iter_known_spaces().next(),
                );
                let observations = topology_workflow::WindowServerDestroyedObservations {
                    resolved_space: self.resolve_native_space(wsid, None),
                    active_spaces: self.active_spaces.clone(),
                    mission_control_active: self.is_mission_control_active(),
                    ordered_in: window_server::window_ordered_in(wsid),
                    assigned_space,
                    last_known_user_space,
                };
                return topology_workflow::handle_window_server_destroyed(
                    &mut self.state,
                    &self.transaction_manager,
                    &mut self.drag_manager,
                    topology_workflow::WindowServerLifecyclePayload {
                        window_server_id: wsid,
                        space: sid,
                        kind,
                    },
                    observations,
                );
            }
            Event::WindowServerAppeared(wsid, sid, kind) => {
                let tracked_window = self.state.windows.tracked_window_id(wsid);
                let assigned_space =
                    tracked_window.and_then(|window| self.assigned_space_for_window_id(window));
                let last_known_user_space = topology_workflow::resolve_last_known_user_space(
                    tracked_window.and_then(|window| self.best_space_for_window_id(window)),
                    self.space_state.iter_known_spaces().next(),
                );
                let window_server_info = window_server::get_window(wsid);
                let owner_pid = window_server_info.as_ref().map(|info| info.pid);
                let app_known =
                    owner_pid.is_some_and(|pid| self.app_manager.apps.contains_key(&pid));
                let running_app_info = owner_pid.filter(|_| !app_known).and_then(|pid| {
                    objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(
                        pid,
                    )
                    .map(|app| AppInfo::from(&*app))
                });
                // A window belonging to a workspace its display is not showing is one rini
                // parked off-screen. Membership rather than geometry, because the parked
                // coordinates deliberately sit inside the neighbouring display and so cannot
                // distinguish "parked" from "genuinely moved there".
                let is_parked_by_rini = self
                    .state
                    .windows
                    .tracked_window_id(wsid)
                    .and_then(|wid| {
                        let assignment = self.state.windows.workspace_info_for_window(wid)?;
                        let showing =
                            self.layout_manager.layout_engine.active_workspace(assignment.space)?;
                        if assignment.workspace_id == showing {
                            return Some(false);
                        }
                        // Parked, so its POSITION proves nothing. But WindowServer's own
                        // space membership still does: Mission Control and a genuine
                        // cross-display move both update it, whereas parking only changes
                        // coordinates. So only distrust the appearance when membership does
                        // not corroborate it.
                        let membership = window_server::window_spaces(wsid);
                        Some(!membership.contains(&sid))
                    })
                    .unwrap_or(false);
                let observations = topology_workflow::WindowServerAppearedObservations {
                    is_parked_by_rini,
                    resolved_space: self.resolve_native_space(wsid, Some(sid)),
                    active_spaces: self.active_spaces.clone(),
                    mission_control_active: self.is_mission_control_active(),
                    assigned_space,
                    last_known_user_space,
                    window_server_info,
                    app_known,
                    running_app_info,
                };
                return topology_workflow::handle_window_server_appeared(
                    &mut self.state,
                    topology_workflow::WindowServerLifecyclePayload {
                        window_server_id: wsid,
                        space: sid,
                        kind,
                    },
                    observations,
                );
            }
            Event::SpaceCreated(space) => {
                return topology_workflow::handle_space_lifecycle(
                    &mut self.space_activation_policy,
                    topology_workflow::SpaceLifecyclePayload { space, created: true },
                );
            }
            Event::SpaceDestroyed(space) => {
                return topology_workflow::handle_space_lifecycle(
                    &mut self.space_activation_policy,
                    topology_workflow::SpaceLifecyclePayload { space, created: false },
                );
            }
            Event::WindowMinimized(wid) => {
                return window_workflow::handle_window_minimized(&mut self.state, wid);
            }
            Event::WindowDeminiaturized(wid) => {
                let active_space = self.state.windows.window(wid).and_then(|window| {
                    self.best_space_for_window(&window.frame_monotonic, window.info.sys_id)
                        .filter(|space| self.is_space_active(*space))
                        .or_else(|| {
                            window
                                .info
                                .sys_id
                                .is_none()
                                .then(|| self.workspace_command_space())
                                .flatten()
                        })
                });
                return window_workflow::handle_window_deminiaturized(
                    &mut self.state,
                    window_workflow::WindowDeminiaturizedPayload { window: wid, active_space },
                );
            }
            Event::WindowFrameChanged(wid, new_frame, last_seen, requested, mouse_state) => {
                let mission_control_active = self.is_mission_control_active();
                let mut effective_mouse_state = mouse_state;
                if matches!(
                    window_workflow::classify_window_frame_change(
                        &mut self.state,
                        &self.transaction_manager,
                        &mut self.drag_manager,
                        wid,
                        new_frame,
                        last_seen,
                        requested.0,
                        &mut effective_mouse_state,
                        mission_control_active,
                    ),
                    window_workflow::FrameChangeDisposition::Handled
                ) {
                    let mut outcome = EventOutcome::no_change();
                    outcome.dispatch_mouse_up = effective_mouse_state
                        == Some(rini_macos::event::MouseState::Up)
                        && matches!(
                            self.drag_manager.drag_state,
                            DragState::Active { .. } | DragState::PendingSwap { .. }
                        );
                    outcome.focused_window = raised_window;
                    return Ok(outcome);
                }
                let (server_id, old_frame) = self
                    .state
                    .windows
                    .window(wid)
                    .map(|window| (window.info.sys_id, window.frame_monotonic))
                    .unwrap_or((None, new_frame));
                let old_space = self.geometry_space_for_window(&old_frame, server_id);
                let new_space = self.geometry_space_for_window(&new_frame, server_id);
                let old_space_active = old_space.is_some_and(|space| self.is_space_active(space));
                let new_space_active = new_space.is_some_and(|space| self.is_space_active(space));
                let best_resize_space = self.best_space_for_window(&new_frame, server_id);
                let active_resize_space =
                    best_resize_space.filter(|space| self.is_space_active(*space)).or_else(|| {
                        server_id.is_none().then(|| self.workspace_command_space()).flatten()
                    });
                let pending_target_space = server_id
                    .and_then(|server| self.pending_target_space_for_window_server_id(server));
                let assigned_space = self.assigned_space_for_window_id(wid);
                let keep_assigned_for_scrolling = old_space.is_some_and(|space| {
                    !self.layout_manager.layout_engine.is_window_floating(wid)
                        && self
                            .layout_manager
                            .layout_engine
                            .virtual_workspace_manager()
                            .workspace_for_window(&self.state.windows, space, wid)
                            .is_some()
                });
                let screens = self
                    .space_state
                    .screens
                    .iter()
                    .filter_map(|screen| {
                        Some((screen.space?, screen.frame, screen.display_uuid_owned()))
                    })
                    .collect();
                let mut outcome = window_workflow::handle_window_frame_changed(
                    &mut self.state,
                    &mut self.layout_manager,
                    &mut self.drag_manager,
                    window_workflow::WindowFrameChangedPayload {
                        window: wid,
                        new_frame,
                        mouse_state: effective_mouse_state,
                        old_space,
                        new_space,
                        old_space_active,
                        new_space_active,
                        active_resize_space,
                        pending_target_space,
                        assigned_space,
                        keep_assigned_for_scrolling,
                        screens,
                    },
                )?;
                // Frame acknowledgements and no-op geometry changes can return
                // early from the reducer. Mouse release still has to terminate
                // an existing drag session in those cases.
                if effective_mouse_state == Some(rini_macos::event::MouseState::Up)
                    && matches!(
                        self.drag_manager.drag_state,
                        DragState::Active { .. } | DragState::PendingSwap { .. }
                    )
                {
                    outcome.dispatch_mouse_up = true;
                }
                outcome.focused_window = raised_window;
                return Ok(outcome);
            }
            Event::WindowTitleChanged(wid, new_title) => {
                let mut outcome = window_workflow::handle_window_title_changed(
                    &mut self.state,
                    window_workflow::WindowTitleChangedPayload { window: wid, title: new_title },
                )?;
                outcome.focused_window = raised_window;
                return Ok(outcome);
            }
            Event::ApplyOverlayFrames(frames) => {
                self.apply_overlay_frames(frames);
                return Ok(EventOutcome::no_change());
            }
            Event::SpaceStateChanged(space_state) => {
                let releases_lifecycle_refresh_quarantine =
                    space_state.releases_lifecycle_refresh_quarantine;
                let releases_display_churn_refresh_quarantine =
                    space_state.releases_display_churn_refresh_quarantine;
                let outcome = self.handle_authoritative_space_snapshot(space_state)?;
                if releases_lifecycle_refresh_quarantine {
                    self.release_post_instability_quarantine_after_authoritative_snapshot();
                }
                if releases_display_churn_refresh_quarantine {
                    self.refresh_quarantine_manager.display_churn_active = false;
                    self.request_refresh_when_spaces_actor_stabilizes();
                    // A display was plugged in or unplugged. Flush any debounced save
                    // now: this is exactly the transition where the arrangement about
                    // to be replaced is the one worth remembering, and it is also
                    // when the layout file gets consulted to put windows back on the
                    // display they came from.
                    if self.autosave_pending {
                        self.save_layout_now();
                    }
                }
                return Ok(outcome);
            }
            Event::ActiveDisplayChanged { menu_bar_space, command_space } => {
                self.space_state.menu_bar_space = menu_bar_space;
                self.space_state.command_space = command_space;
                return Ok(EventOutcome::default());
            }
            Event::MouseUp => {
                let pending_swap = self.get_pending_drag_swap();
                let (visible_spaces, visible_space_centers) = self.visible_spaces_for_layout(true);
                let swap_space = pending_swap
                    .and_then(|(dragged, _)| {
                        self.state.windows.window(dragged).and_then(|window| {
                            self.best_space_for_window(&window.frame_monotonic, window.info.sys_id)
                        })
                    })
                    .or_else(|| {
                        self.drag_manager
                            .drag_swap_manager
                            .origin_frame()
                            .and_then(|frame| self.best_space_for_frame(&frame))
                    })
                    .or_else(|| self.space_state.screens.iter().find_map(|screen| screen.space));
                let session = match &self.drag_manager.drag_state {
                    DragState::Active { session } | DragState::PendingSwap { session, .. } => {
                        Some(session.clone())
                    }
                    DragState::Inactive => None,
                };
                let final_space = session.as_ref().and_then(|session| {
                    session
                        .settled_space
                        .or_else(|| self.best_space_for_frame(&session.last_frame))
                        .or_else(|| self.best_space_for_window_id(session.window))
                });
                let focused = self.window_id_under_cursor().and_then(|window| {
                    self.best_space_for_window_id(window).map(|space| (space, window))
                });
                let mut outcome = interaction_workflow::handle_mouse_up(
                    &mut self.state,
                    &mut self.layout_manager,
                    &mut self.drag_manager,
                    interaction_workflow::MouseUpPayload {
                        pending_swap,
                        swap_space,
                        final_space,
                        visible_spaces,
                        visible_space_centers,
                    },
                )?;
                if let Some((space, window)) = focused {
                    outcome = outcome.with_layout_event(LayoutEvent::WindowFocused(space, window));
                }
                return Ok(outcome);
            }
            Event::MenuOpened(pid) => {
                return Ok(system_workflow::handle_menu_opened(&mut self.menu_manager, pid)?);
            }
            Event::MenuClosed(pid) => {
                return Ok(system_workflow::handle_menu_closed(&mut self.menu_manager, pid)?);
            }
            Event::MouseMoved(wsid) => {
                let window = self.state.windows.tracked_window_id(wsid);
                let active_space = window.and_then(|window| {
                    self.state.windows.window(window).and_then(|state| {
                        self.best_space_for_window(&state.frame_monotonic, state.info.sys_id)
                            .filter(|space| self.is_space_active(*space))
                            .or_else(|| {
                                state
                                    .info
                                    .sys_id
                                    .is_none()
                                    .then(|| self.workspace_command_space())
                                    .flatten()
                            })
                    })
                });
                let needs_layout_sync = window.is_some_and(|window| {
                    self.layout_manager.layout_engine.focused_window() != Some(window)
                });
                return window_workflow::handle_mouse_moved_over_window(
                    &self.app_manager,
                    window_workflow::MouseMovedPayload {
                        window,
                        should_sync: window
                            .is_some_and(|window| self.should_raise_on_mouse_over(window)),
                        is_main: window.is_some_and(|window| self.main_window() == Some(window)),
                        needs_layout_sync,
                        active_space,
                    },
                );
            }
            Event::MissionControlNativeEntered => {
                return topology_workflow::handle_mission_control_native_entered(
                    &mut self.mission_control_manager,
                    &mut self.drag_manager,
                );
            }
            Event::MissionControlNativeExited => {
                return topology_workflow::handle_mission_control_native_exited(
                    &mut self.mission_control_manager,
                );
            }
            Event::RaiseCompleted { window_id, sequence_id } => {
                return Ok(system_workflow::handle_raise_completed(
                    system_workflow::RaiseCompletedPayload {
                        window: window_id,
                        sequence: sequence_id,
                    },
                )?);
            }
            Event::RaiseTimeout { sequence_id } => {
                return Ok(system_workflow::handle_raise_timeout(sequence_id)?);
            }
            Event::ConfigUpdated(new_cfg) => {
                return command_workflow::handle_config_updated(
                    &mut self.config,
                    &mut self.layout_manager,
                    &mut self.drag_manager,
                    new_cfg,
                );
            }
            Event::Command(Command::Metrics(cmd)) => {
                return command_workflow::handle_command_metrics(cmd);
            }
            Event::Command(Command::Reactor(ReactorCommand::DebugOverlaySlide {
                dx,
                dy,
                duration_ms,
            })) => {
                // Make sure the actor knows the display before asking it to draw, since it silently
                // declines to animate without geometry.
                self.publish_animation_display();
                let response = match &self.communication_manager.workspace_animation_tx {
                    Some(tx) => {
                        _ = tx.send(crate::actor::workspace_animation::Event::DebugSlide {
                            dx: dx as f64,
                            dy: dy as f64,
                            duration: std::time::Duration::from_millis(duration_ms),
                        });
                        format!("overlay slide requested: dx {dx}, dy {dy}, {duration_ms}ms")
                    }
                    None => "the workspace animation actor is not running".to_string(),
                };
                return Ok(EventOutcome::no_change().with_stdout_line(response));
            }
            Event::Command(Command::Reactor(ReactorCommand::DebugWarmSnapshots)) => {
                self.publish_animation_display();
                let response = match &self.communication_manager.workspace_animation_tx {
                    Some(tx) => {
                        _ = tx.send(crate::actor::workspace_animation::Event::WarmCache);
                        "snapshot cache warm requested".to_string()
                    }
                    None => "the workspace animation actor is not running".to_string(),
                };
                return Ok(EventOutcome::no_change().with_stdout_line(response));
            }
            Event::Command(Command::Reactor(ReactorCommand::Debug)) => {
                return command_workflow::handle_command_reactor_debug(
                    &self.layout_manager,
                    &self.space_state,
                );
            }
            Event::Command(Command::Reactor(ReactorCommand::SaveAndExit)) => {
                let active_space = self.active_display_space();
                return command_workflow::handle_command_reactor_save_and_exit(
                    &self.state,
                    &mut self.layout_manager,
                    active_space,
                );
            }
            Event::Command(Command::Reactor(ReactorCommand::SaveLayout { path })) => {
                let active_space = self.active_display_space();
                return command_workflow::handle_command_reactor_save_layout(
                    &self.state,
                    &mut self.layout_manager,
                    path,
                    active_space,
                );
            }
            Event::Command(Command::Reactor(ReactorCommand::RestoreLayout {
                path,
                scope,
                source,
            })) => {
                let Some(active_space) = self.active_display_space() else {
                    return Ok(EventOutcome::no_change().with_stdout_line(
                        "Could not restore saved layout: no active macOS space is available".into(),
                    ));
                };
                let request = layout::RestoreRequest { scope, active_space, source };
                let outcome = EventOutcome::window_membership_changed(false, true);
                let report = self.layout_manager.layout_engine.restore_layout(
                    path,
                    request,
                    &mut self.state.windows,
                    &self.config.virtual_workspaces,
                    &self.config.settings.layout,
                );
                return Ok(match report {
                    Ok(report) => outcome.with_stdout_line(report.summary()),
                    Err(error) => {
                        tracing::error!(?scope, %error, "Could not restore saved layout");
                        outcome.with_stdout_line(format!("Could not restore saved layout: {error}"))
                    }
                });
            }
            Event::Command(Command::Reactor(ReactorCommand::Serialize)) => {
                let serialized = self.serialize_state();
                return command_workflow::handle_command_reactor_serialize(serialized);
            }
            Event::Command(Command::Reactor(ReactorCommand::SwitchSpace(direction))) => {
                return command_workflow::handle_switch_native_space(direction);
            }
            Event::Command(Command::Reactor(ReactorCommand::RedistributeWindows)) => {
                return Ok(self.redistribute_windows());
            }
            Event::Command(Command::Reactor(ReactorCommand::CycleAppWindows { backward })) => {
                return Ok(self.cycle_app_windows(backward));
            }
            Event::Command(Command::Reactor(ReactorCommand::ToggleSpaceActivated)) => {
                let space = self.active_display_space();
                let display_uuid = space.and_then(|space| {
                    self.space_state
                        .screen_by_space(space)
                        .and_then(|screen| screen.display_uuid_owned())
                });
                let config = self.activation_cfg();
                return command_workflow::handle_command_reactor_toggle_space_activated(
                    &mut self.space_activation_policy,
                    command_workflow::ToggleSpacePayload { config, space, display_uuid },
                );
            }
            Event::Command(Command::Reactor(ReactorCommand::CloseWindow { window_server_id })) => {
                return command_workflow::handle_close_window(
                    window_server_id.map(WindowServerId::new),
                );
            }
            Event::Command(Command::Reactor(ReactorCommand::FocusWindow {
                window_id,
                window_server_id,
            })) => {
                let window_id = WindowId::new(window_id.pid, window_id.idx);
                let window_server_id = window_server_id.map(WindowServerId::new);
                let resolved_space = self.best_space_for_window_id(window_id).or_else(|| {
                    self.state.windows.window(window_id).and_then(|window| {
                        self.best_space_for_window(&window.frame_monotonic, window.info.sys_id)
                    })
                });
                return command_workflow::handle_command_reactor_focus_window(
                    &self.state,
                    &self.app_manager,
                    command_workflow::FocusWindowPayload {
                        window_id,
                        window_server_id,
                        resolved_space,
                        space_is_active: resolved_space
                            .is_some_and(|space| self.is_space_active(space)),
                    },
                );
            }
            Event::Command(Command::Reactor(ReactorCommand::MoveMouseToDisplay(selector))) => {
                let screen = self.screen_for_selector(&selector, None).cloned();
                let focus_window = screen.as_ref().and_then(|screen| {
                    let space = screen.space?;
                    self.last_focused_window_in_space(space).or_else(|| {
                        self.layout_manager
                            .layout_engine
                            .windows_in_active_workspace(&self.state.windows, space)
                            .into_iter()
                            .next()
                    })
                });
                let target_is_active = screen
                    .as_ref()
                    .and_then(|screen| screen.space)
                    .is_none_or(|space| self.is_space_active(space));
                return command_workflow::handle_move_mouse_to_display(
                    command_workflow::DisplayFocusPayload {
                        screen,
                        target_is_active,
                        focus_window,
                    },
                );
            }
            Event::Command(Command::Reactor(ReactorCommand::FocusDisplay(selector))) => {
                let screen = self.screen_for_selector(&selector, None).cloned();
                let focus_window = screen.as_ref().and_then(|screen| {
                    let space = screen.space?;
                    self.last_focused_window_in_space(space).or_else(|| {
                        self.layout_manager
                            .layout_engine
                            .windows_in_active_workspace(&self.state.windows, space)
                            .into_iter()
                            .next()
                    })
                });
                let target_is_active = screen
                    .as_ref()
                    .and_then(|screen| screen.space)
                    .is_none_or(|space| self.is_space_active(space));
                return command_workflow::handle_focus_display(
                    command_workflow::DisplayFocusPayload {
                        screen,
                        target_is_active,
                        focus_window,
                    },
                );
            }
            Event::Command(Command::Layout(command)) => {
                let command_space = self.command_context_space();
                let (visible_spaces, visible_space_centers) = self.visible_spaces_for_layout(false);
                return command_workflow::handle_command_layout(
                    &mut self.state,
                    &mut self.layout_manager,
                    &mut self.workspace_switch_manager,
                    command_workflow::LayoutCommandPayload {
                        command,
                        command_space,
                        visible_spaces,
                        visible_space_centers,
                    },
                );
            }
            Event::Command(Command::Reactor(ReactorCommand::MoveWindowToDisplay {
                selector,
                window_id,
            })) => {
                if self.is_in_drag() {
                    warn!("Ignoring move-window-to-display while a drag is active");
                    return Ok(EventOutcome::no_change());
                }
                let command_space = self.workspace_command_space();
                let resolved_window = {
                    let workspaces = self.layout_manager.layout_engine.virtual_workspace_manager();
                    match window_id {
                        Some(index) => command_space
                            .and_then(|space| {
                                workspaces.find_window_by_idx(&self.state.windows, space, index)
                            })
                            .or_else(|| {
                                self.iter_active_spaces().find_map(|space| {
                                    workspaces.find_window_by_idx(&self.state.windows, space, index)
                                })
                            }),
                        None => self
                            .main_window()
                            .or_else(|| self.window_id_under_cursor())
                            .or_else(|| {
                                command_space.and_then(|space| {
                                    workspaces.find_window_by_idx(&self.state.windows, space, 0)
                                })
                            }),
                    }
                };
                let Some(window) = resolved_window else {
                    warn!("Move window to display ignored because no target window was resolved");
                    return Ok(EventOutcome::no_change());
                };
                let Some(window_state) = self.state.windows.window(window) else {
                    warn!(?window, "Move window to display ignored: unknown window");
                    return Ok(EventOutcome::no_change());
                };
                let window_server_id = window_state.info.sys_id;
                let window_frame = window_state.frame_monotonic;
                let source_space = self
                    .assigned_space_for_window_id(window)
                    .or_else(|| self.best_space_for_window_id(window))
                    .or_else(|| self.best_space_for_window(&window_frame, window_server_id));
                let Some(source_space) = source_space.filter(|space| self.is_space_active(*space))
                else {
                    warn!(
                        ?window,
                        "Move window to display ignored: source space unavailable"
                    );
                    return Ok(EventOutcome::no_change());
                };
                let origin = self
                    .space_state
                    .screen_by_space(source_space)
                    .map(|screen| screen.frame.mid())
                    .or_else(|| self.current_screen_center());
                let Some(target_screen) = self.screen_for_selector(&selector, origin).cloned()
                else {
                    warn!(
                        ?selector,
                        "Move window to display ignored: target display not found"
                    );
                    return Ok(EventOutcome::no_change());
                };
                let Some(target_space) =
                    target_screen.space.filter(|space| self.is_space_active(*space))
                else {
                    warn!(
                        ?selector,
                        "Move window to display ignored: target space unavailable"
                    );
                    return Ok(EventOutcome::no_change());
                };
                if source_space == target_space {
                    return Ok(EventOutcome::no_change());
                }
                let mut target_frame = window_frame;
                let mut origin = target_screen.frame.mid();
                origin.x -= window_frame.size.width / 2.0;
                origin.y -= window_frame.size.height / 2.0;
                let min = target_screen.frame.min();
                let max = target_screen.frame.max();
                origin.x = origin.x.max(min.x).min(max.x - window_frame.size.width);
                origin.y = origin.y.max(min.y).min(max.y - window_frame.size.height);
                target_frame.origin = origin;
                return command_workflow::handle_command_reactor_move_window_to_display(
                    &mut self.state,
                    &mut self.layout_manager,
                    command_workflow::MoveWindowToDisplayPayload {
                        window,
                        window_server_id,
                        source_space,
                        target_space,
                        target_screen: target_screen.frame,
                        target_frame,
                    },
                );
            }
            _ => (),
        }

        Ok(EventOutcome::focus_changed(
            raised_window,
            should_update_notifications,
        ))
    }

    /// Applies workflow follow-up requests in one stable order.
    ///
    /// Explicit transition frames are written before layout calculation so the
    /// resulting layout remains authoritative. Focus selection follows layout
    /// writes, then UI/platform presentation state is refreshed. Broadcast and
    /// discovery requests made directly by a workflow are consequently observed
    /// only after its model mutation is complete.
    fn apply_event_outcome(&mut self, outcome: EventOutcome) {
        // Per event: what this event's regroup raises is only known to this event.
        self.regroup_raised.clear();
        // Ahead of the layout events, as on the AX path: the closed window's picture goes first.
        for window in outcome.forgotten_windows {
            if let Some(tx) = &self.communication_manager.workspace_animation_tx {
                _ = tx.send(crate::actor::workspace_animation::Event::ForgetWindow(window));
            }
        }
        if !outcome.window_server_updates.is_empty() {
            self.update_partial_window_server_info(outcome.window_server_updates);
        }
        if outcome.recompute_active_spaces {
            self.recompute_and_set_active_spaces_from_current_screens();
        }
        if outcome.repair_spaces_after_mission_control {
            self.repair_spaces_after_mission_control();
        }
        if outcome.refresh_after_mission_control {
            self.refresh_windows_after_mission_control();
        }
        if outcome.force_refresh_all_windows {
            self.force_refresh_all_windows();
        }
        // Discovery responses reconcile model state before layout. Requests
        // which schedule new discovery are deferred to the final phase below.
        for discovery in outcome.discoveries {
            self.on_windows_discovered_with_app_info(
                discovery.pid,
                discovery.new,
                discovery.known_visible,
                discovery.app_info,
            );
        }
        for window in outcome.reapply_app_rules {
            self.maybe_reapply_app_rules_for_window(window);
        }
        for window in outcome.finalize_created_windows {
            let active_space = self.state.windows.window(window).and_then(|state| {
                self.best_space_for_window(&state.frame_monotonic, state.info.sys_id)
                    .filter(|space| self.is_space_active(*space))
                    .or_else(|| {
                        state
                            .info
                            .sys_id
                            .is_none()
                            .then(|| self.workspace_command_space())
                            .flatten()
                    })
            });
            if let Some(space) = active_space {
                if let Some(app_info) =
                    self.app_manager.apps.get(&window.pid).map(|app| app.info.clone())
                {
                    if let Some(window_server_id) =
                        self.state.windows.window(window).and_then(|state| state.info.sys_id)
                    {
                        self.state.windows.mark_wsids_recent(std::iter::once(window_server_id));
                    }
                    self.process_windows_for_app_rules(window.pid, vec![window], app_info);
                }
                if self
                    .state
                    .windows
                    .window(window)
                    .is_some_and(|state| state.matches_filter(WindowFilter::EffectivelyManageable))
                {
                    self.send_layout_event(LayoutEvent::WindowAdded(space, window));
                }
            }
        }

        for (window_server_id, space) in outcome.confirmed_window_spaces {
            self.clear_pending_target_if_confirmed_space(window_server_id, space);
        }
        for (window_server_id, space, window) in outcome.fullscreen_restorations {
            let mut nested = EventOutcome::default();
            if self
                .restore_fullscreen_window_to_user_space(
                    window_server_id,
                    space,
                    window,
                    &mut nested,
                )
                .is_none()
            {
                self.reassign_window_to_authoritative_space(window, space);
            }
            self.apply_event_outcome(nested);
        }
        for reassignment in outcome.topology_reassignments {
            if reassignment.preserve_workspace_ordinal {
                self.reassign_window_to_authoritative_space_preserving_workspace_ordinal(
                    reassignment.window,
                    reassignment.space,
                );
            } else {
                self.reassign_window_to_authoritative_space(
                    reassignment.window,
                    reassignment.space,
                );
            }
        }

        // Some transitions need to place a window on its destination display
        // before arranging that display. Keep these writes ahead of both layout
        // responses and the arrange pass so tiling always supplies the final frame.
        for write in outcome.pre_layout_window_frame_writes {
            let window_server_id =
                self.state.windows.window(write.window).and_then(|window| window.info.sys_id);
            let transaction = if let Some(window_server_id) = window_server_id {
                let transaction = self.transaction_manager.generate_next_txid(window_server_id);
                self.transaction_manager.store_txid(window_server_id, transaction, write.frame);
                transaction
            } else {
                TransactionId::default()
            };
            if let Some(app) = self.app_manager.apps.get(&write.window.pid)
                && let Err(error) = app.handle.send(Request::SetWindowFrame(
                    write.window,
                    write.frame,
                    transaction,
                    write.requested,
                ))
            {
                warn!(window = ?write.window, %error, "failed to write requested window frame");
            }
        }

        let focus_landed = outcome.focused_window.is_some()
            || outcome.layout_events.iter().any(|event| matches!(event, LayoutEvent::WindowFocused(..)));
        for event in outcome.layout_events {
            self.send_layout_event(event);
        }
        for (response, workspace_switch_space) in outcome.layout_responses {
            self.handle_layout_response(response, workspace_switch_space);
        }
        for (window, frame) in outcome.drag_swap_evaluations {
            self.maybe_swap_on_drag(window, frame);
        }
        if outcome.dispatch_mouse_up {
            self.handle_event(Event::MouseUp);
        }

        let mut layout_changed = false;
        if outcome.arrange.requested && (!self.is_in_drag() || outcome.arrange.window_was_destroyed)
        {
            for _ in 0..outcome.arrange.passes.max(1) {
                layout_changed |= self.update_layout_or_warn(
                    outcome.arrange.is_resize,
                    matches!(
                        self.workspace_switch_manager.workspace_switch_state,
                        WorkspaceSwitchState::Active
                    ),
                    outcome.arrange.space_scope,
                );
            }
        }

        for request in outcome.raise_requests {
            self.dispatch_raise(request);
        }
        if let Some((space, window)) =
            focus_service::resolve(outcome.focused_window, |wid| self.best_space_for_window_id(wid))
        {
            self.send_layout_event(LayoutEvent::WindowFocused(space, window));
        }

        // After the raises above, so a column this pass scrolled in is lifted over a floating window
        // in a sequence that ends after the focus raise, with the focused window last again. A focus
        // report with no layout change (a click) is judged too: macOS raised only the clicked window.
        if layout_changed || focus_landed {
            self.regroup_after_layout();
        }

        if let Some(direction) = outcome.switch_native_space {
            unsafe { window_server::switch_space(direction) };
        }

        for (pid, window) in outcome.make_key_windows {
            if let Err(error) = window_server::make_key_window(pid, window) {
                warn!(?error, "failed to make key window");
            }
        }
        for point in outcome.mouse_warps {
            self.warp_mouse(point);
        }

        for event in outcome.wm_events {
            if let Some(wm) = self.communication_manager.wm_sender.as_ref() {
                wm.send(event);
            }
        }

        if let Some(window_server_id) = outcome.close_window {
            let target = match window_server_id {
                Some(wsid) => self.state.windows.tracked_window_id(wsid),
                None => self.main_window(),
            };
            if let Some(window) = target {
                self.request_close_window(window.pid, window_server_id);
            } else {
                warn!(?window_server_id, "Close target not found");
            }
        }

        if let Some(config) = outcome.service_config_update {
            if let Some(wm) = &self.communication_manager.wm_sender {
                wm.send(crate::actor::wm_controller::WmEvent::ConfigUpdated(config));
            }
            // Re-sends both the flag and the geometry, so toggling
            // warp_cursor_between_stacked_displays applies without a restart.
            self.publish_cursor_warp_screens();
            self.publish_animation_display();
        }
        for line in outcome.stdout_lines {
            println!("{line}");
        }
        self.workspace_switch_manager.mark_workspace_switch_inactive();
        if self.workspace_switch_manager.active_workspace_switch.is_some() && !layout_changed {
            self.workspace_switch_manager.active_workspace_switch = None;
            trace!("Workspace switch stabilized with no further frame changes");
        }

        // Execute deferred mouse warp after workspace switch completes
        if let Some(wid) = self.workspace_switch_manager.pending_workspace_mouse_warp.take() {
            if let Some(window_center) = self.window_center_on_known_screen(wid) {
                self.warp_mouse(window_center);
            }
        }

        if outcome.refresh_window_notifications {
            let mut ids: Vec<u32> = self
                .state
                .windows
                .iter_tracked_window_server_ids()
                .map(|wsid| wsid.as_u32())
                .collect();
            ids.sort_unstable();

            if ids != self.notification_manager.last_sls_notification_ids {
                rini_macos::window_notify::update_window_notifications(&ids);

                self.notification_manager.last_sls_notification_ids = ids;
            }
        }
        if outcome.refresh_focus_follows_mouse {
            self.update_focus_follows_mouse_state();
        }
        for broadcast in outcome.window_title_broadcasts {
            self.broadcast_window_title_changed(
                broadcast.window,
                broadcast.previous_title,
                broadcast.new_title,
            );
        }
        if let Some(window) = outcome.focused_window_broadcast {
            self.broadcast_focused_window_changed(window);
        }
        // Requests which schedule fresh discovery are last so observers see
        // the fully reconciled model, layout, UI, and broadcasts.
        for (pid, request) in outcome.app_requests {
            if let Some(app) = self.app_manager.apps.get(&pid)
                && let Err(error) = app.handle.send(request)
            {
                warn!(pid, %error, "failed to send deferred application request");
            }
        }
    }

    fn create_window_data(&self, window_id: WindowId) -> Option<RuntimeWindowData> {
        let window_state = self.state.windows.window(window_id)?;
        if !window_state.matches_filter(WindowFilter::EffectivelyManageable) {
            return None;
        }
        let app = self.app_manager.apps.get(&window_id.pid)?;

        let app_name = app.info.localized_name.clone();
        let bundle_id = app.info.bundle_id.clone();

        Some(RuntimeWindowData {
            id: window_id,
            is_floating: self.layout_manager.layout_engine.is_window_floating(window_id),
            is_focused: self.main_window() == Some(window_id),
            app_name,
            info: WindowInfo {
                title: window_state.info.title.clone(),
                frame: window_state.frame_monotonic,
                bundle_id,
                ..window_state.info.clone()
            },
        })
    }

    fn update_complete_window_server_info(&mut self, ws_info: Vec<WindowServerInfo>) {
        self.state.windows.clear_visible_windows();
        self.update_partial_window_server_info(ws_info);
    }

    fn update_partial_window_server_info(&mut self, ws_info: Vec<WindowServerInfo>) {
        // Mark visible windows and remove any corresponding observed WSID markers
        // for ids we now have server info for.
        self.state.windows.set_visible_windows(ws_info.iter().map(|info| info.id));
        for info in ws_info.iter() {
            // If we've been observing this server id from SLS callbacks, clear it.
            self.state.windows.clear_window_server_observed(info.id);
            self.state.windows.track_window_server_info(*info);

            if let Some(wid) = self.state.windows.tracked_window_id(info.id) {
                let (server_id, is_minimized, is_ax_standard, is_ax_root, was_manageable) =
                    if let Some(window) = self.state.windows.window_mut(wid) {
                        if info.layer == 0 {
                            window.frame_monotonic = info.frame;
                        }
                        (
                            window.info.sys_id,
                            window.info.is_minimized,
                            window.info.is_standard,
                            window.info.is_root,
                            window.matches_filter(WindowFilter::EffectivelyManageable),
                        )
                    } else {
                        continue;
                    };
                let manageable = utils::compute_window_manageability(
                    server_id,
                    is_minimized,
                    is_ax_standard,
                    is_ax_root,
                    |wsid| self.state.windows.get_window_server_info(wsid),
                );
                if let Some(window) = self.state.windows.window_mut(wid) {
                    window.is_manageable = manageable;
                }

                if was_manageable && !manageable {
                    self.send_layout_event(LayoutEvent::WindowRemoved(wid));
                }
            }
        }
    }

    fn check_for_new_windows(&mut self) {
        // AX discovery remains the source of truth for enumerating app windows.
        // Native-space membership/visibility is supplied separately by the spaces
        // actor; do not replace this with the global CG on-screen window list.
        self.request_visible_windows_for_apps(false);
    }

    fn request_visible_windows_for_apps(&mut self, track_mission_control_refresh: bool) {
        if self.refreshes_blocked() {
            self.defer_visible_refresh(track_mission_control_refresh);
            return;
        }

        let mut refreshed_pids = Vec::new();
        for (&pid, app) in &self.app_manager.apps {
            // Errors mean the app terminated (and a termination event is coming); ignore.
            if app.handle.send(Request::GetVisibleWindows).is_ok() {
                refreshed_pids.push(pid);
            }
        }

        if track_mission_control_refresh {
            self.mission_control_manager
                .pending_mission_control_refresh
                .extend(refreshed_pids);
        }
    }

    fn restore_windows_after_fullscreen_exit(&mut self, spaces: &[Option<SpaceId>]) {
        let refresh_spaces: Vec<SpaceId> = spaces
            .iter()
            .copied()
            .flatten()
            .filter(|space| !self.is_fullscreen_space(*space))
            .collect();

        for space in refresh_spaces {
            let records: Vec<_> = self
                .state
                .windows
                .iter_native_fullscreen_records()
                .filter(|record| {
                    record.last_known_user_space == Some(space)
                        || record.workspace.is_some_and(|workspace| workspace.space == space)
                })
                .collect();

            if records.is_empty() {
                continue;
            }

            for record in records {
                let _ = self
                    .state
                    .windows
                    .restore_window_from_native_fullscreen(record.current_window_id);

                if let Some(app) = self.app_manager.apps.get(&record.current_window_id.pid) {
                    if let Err(e) = app.handle.send(Request::GetVisibleWindows) {
                        warn!(
                            "Failed to send GetVisibleWindows to app {}: {}",
                            record.current_window_id.pid, e
                        );
                    }
                }

                let live_window_id = record
                    .window_server_id
                    .and_then(|wsid| self.state.windows.tracked_window_id(wsid))
                    .or_else(|| {
                        self.state
                            .windows
                            .contains_window(record.current_window_id)
                            .then_some(record.current_window_id)
                    });

                let target_space = record
                    .workspace
                    .map(|workspace| workspace.space)
                    .or(record.last_known_user_space);

                if let (Some(window_id), Some(target_space)) = (live_window_id, target_space)
                    && let Some(source_space) =
                        self.best_space_for_window_id(window_id).or(Some(target_space))
                    && source_space != target_space
                {
                    let target_screen_size = self
                        .space_state
                        .screen_by_space(target_space)
                        .map(|screen| screen.frame.size)
                        .unwrap_or_else(|| CGSize::new(0.0, 0.0));

                    let response = self.layout_manager.layout_engine.move_window_to_space(
                        &mut self.state.windows,
                        source_space,
                        target_space,
                        target_screen_size,
                        window_id,
                    );
                    self.handle_layout_response(response, None);
                }
            }

            self.refocus_manager.refocus_state = RefocusState::Pending(space);
            self.update_layout_or_warn(false, false, None);
            self.update_focus_follows_mouse_state();
        }
    }

    fn is_fullscreen_space(&self, space: SpaceId) -> bool {
        self.space_state.fullscreen_spaces.contains(&space)
    }

    fn finalize_space_change(
        &mut self,
        spaces: &[Option<SpaceId>],
        active_windows: Vec<(WindowServerId, Option<SpaceId>)>,
        preserve_missing_assignments: bool,
    ) {
        self.refocus_manager.stale_cleanup_state = if spaces.iter().all(|space| space.is_none()) {
            StaleCleanupState::Suppressed
        } else {
            StaleCleanupState::Enabled
        };
        self.expose_all_spaces();
        if let Some(main_window) = self.main_window() {
            if let Some(space) = self.main_window_space() {
                self.send_layout_event(LayoutEvent::WindowFocused(space, main_window));
            }
        }
        self.reconcile_authoritative_active_window_snapshot(
            active_windows,
            preserve_missing_assignments,
        );
        self.check_for_new_windows();

        if let Some(space) = self.workspace_command_space() {
            self.focus_desktop_if_active_workspace_empty(space);
        }

        if let Some(space) = self
            .workspace_command_space()
            .or_else(|| spaces.iter().copied().flatten().find(|space| self.is_space_active(*space)))
        {
            if let Some((workspace_id, workspace_name)) =
                self.layout_manager.layout_engine.ensure_active_workspace_info(space)
            {
                let display_uuid = self.display_uuid_for_space(space);
                let broadcast_event = BroadcastEvent::WorkspaceChanged {
                    workspace_id: protocol_workspace_id(workspace_id),
                    workspace_name,
                    space_id: space.get(),
                    display_uuid,
                };
                _ = self.communication_manager.event_broadcaster.send(broadcast_event);
            }
        }
    }

    fn broadcast_window_title_changed(
        &mut self,
        window_id: WindowId,
        previous_title: String,
        new_title: String,
    ) {
        if previous_title != new_title
            && let Some(space) = self.best_space_for_window_id(window_id)
            && self.is_space_active(space)
            && let Some(workspace_id) = self.layout_manager.layout_engine.active_workspace(space)
        {
            let workspace_index = self.layout_manager.layout_engine.active_workspace_idx(space);

            let workspace_name = self
                .layout_manager
                .layout_engine
                .workspace_name(space, workspace_id)
                .unwrap_or_else(|| format!("Workspace {:?}", workspace_id));

            let display_uuid = self.display_uuid_for_space(space);

            let event = BroadcastEvent::WindowTitleChanged {
                window_id: protocol_window_id(window_id),
                workspace_id: protocol_workspace_id(workspace_id),
                workspace_index,
                workspace_name,
                previous_title,
                new_title,
                space_id: space.get(),
                display_uuid,
            };
            let _ = self.communication_manager.event_broadcaster.send(event);
        }
    }

    fn broadcast_focused_window_changed(&self, window_id: WindowId) {
        if let Some(space) = self.best_space_for_window_id(window_id)
            && self.is_space_active(space)
            && let Some(workspace_id) = self.layout_manager.layout_engine.active_workspace(space)
        {
            let workspace_index = self.layout_manager.layout_engine.active_workspace_idx(space);
            let workspace_name = self
                .layout_manager
                .layout_engine
                .workspace_name(space, workspace_id)
                .unwrap_or_else(|| format!("Workspace {:?}", workspace_id));
            let display_uuid = self.display_uuid_for_space(space);

            let event = BroadcastEvent::FocusedWindowChanged {
                window_id: protocol_window_id(window_id),
                workspace_id: protocol_workspace_id(workspace_id),
                workspace_index,
                workspace_name,
                space_id: space.get(),
                display_uuid,
            };
            let _ = self.communication_manager.event_broadcaster.send(event);
        }
    }

    fn maybe_reapply_app_rules_for_window(&mut self, window_id: WindowId) {
        if !self.config.virtual_workspaces.reapply_app_rules_on_title_change {
            return;
        }

        let Some(space) = self.best_space_for_window_id(window_id) else {
            return;
        };
        if !self.is_space_active(space) {
            return;
        }

        let (is_manageable, wsid) = match self.state.windows.window(window_id) {
            Some(window_state) => (
                window_state.matches_filter(WindowFilter::Manageable),
                window_state.info.sys_id,
            ),
            None => return,
        };

        if !is_manageable {
            return;
        }

        let app_info = match self.app_manager.apps.get(&window_id.pid) {
            Some(app_state) => app_state.info.clone(),
            None => return,
        };

        if let Some(window_server_id) = wsid {
            self.state.windows.mark_wsids_recent(std::iter::once(window_server_id));
        }

        self.process_windows_for_app_rules(window_id.pid, vec![window_id], app_info);
    }

    fn handle_authoritative_space_snapshot(
        &mut self,
        space_state: ForwardedSpaceState,
    ) -> anyhow::Result<EventOutcome> {
        let mut outcome = EventOutcome::window_membership_changed(false, true);
        let analysis = topology_workflow::analyze_space_snapshot(
            &self.space_state,
            &self.active_spaces,
            &self.space_activation_policy,
            self.activation_cfg(),
            &space_state,
        );
        let pending_space_state = space_state.clone();
        let ForwardedSpaceState {
            screens,
            fullscreen_spaces,
            has_seen_display_set,
            active_spaces,
            menu_bar_space,
            command_space,
            display_space_ids,
            last_user_space_by_display,
            space_remaps,
            display_set_changed,
            should_force_refresh_layout,
            releases_lifecycle_refresh_quarantine,
            resized_spaces,
            topology_window_delta,
            active_window_spaces,
            ..
        } = space_state;
        self.space_state.active_window_spaces = active_window_spaces;
        let activation_config = self.activation_cfg();
        let topology_workflow::SpaceSnapshotAnalysis {
            spaces,
            authoritative_spaces,
            command_space_only_update,
            invalidates_pending_targets,
        } = analysis;

        let current_display_spaces = screens
            .iter()
            .filter_map(|screen| screen.space.map(|space| (space, screen.display_uuid.clone())))
            .collect::<Vec<_>>();
        self.layout_manager.layout_engine.reconcile_startup_spaces(
            &mut self.state.windows,
            &current_display_spaces,
            screens.len(),
        );

        self.space_state.has_seen_display_set = has_seen_display_set;
        self.space_state.fullscreen_spaces = fullscreen_spaces;
        self.space_state.active_spaces = active_spaces;
        if command_space_only_update {
            self.space_state.menu_bar_space = menu_bar_space;
            self.space_state.command_space = command_space;
            return Ok(outcome);
        }
        // Note on pruning: display state is deliberately NOT pruned when a display goes
        // away. The UUID -> space entry is precisely the memory needed to put windows back
        // when that display returns, and macOS assigns a NEW space id on every reconnect
        // (observed 479 -> 484 -> 487 -> 516 -> 552 for one monitor), so the display UUID
        // is the only durable link between a physical display and its layout. The registry
        // holds one entry per display ever seen, which costs nothing and is what makes
        // dock/undock recoverable.
        self.space_state.menu_bar_space = menu_bar_space;
        self.space_state.command_space = command_space;
        self.space_state.display_space_ids = display_space_ids;
        self.space_state.last_user_space_by_display = last_user_space_by_display;

        if screens.is_empty() {
            self.refocus_manager.stale_cleanup_state = StaleCleanupState::Suppressed;
            if !self.space_state.screens.is_empty() {
                self.space_state.screens.clear();
                self.expose_all_spaces();
            }
            self.recompute_and_set_active_spaces(&[]);
            self.update_complete_window_server_info(Vec::new());
            self.try_apply_pending_space_change();
            return Ok(outcome);
        }

        self.refocus_manager.stale_cleanup_state = StaleCleanupState::Enabled;
        // Which displays were attached BEFORE this snapshot. Captured here because
        // space_state.screens is replaced on the next line, and the reconnect remap
        // below needs to distinguish "this display just came back" from "this display
        // merely switched space".
        let previous_display_uuids: HashSet<String> = self
            .space_state
            .screens
            .iter()
            .map(|screen| screen.display_uuid.clone())
            .collect();
        // Capture affinity for a departing display BEFORE anything reacts to its absence.
        //
        // This is the only moment the truth is still available. `self.state.windows` still
        // holds the pre-change assignments here; the evacuation happens later in this same
        // handler, in apply_topology_window_delta, and once macOS has moved those windows to
        // the remaining display there is no way left to tell which of them had been on the
        // one that vanished. Recording it now is what lets a later replug move back exactly
        // the windows that were there, rather than whichever windows now occupy those slots.
        let departed_displays: Vec<String> = previous_display_uuids
            .iter()
            .filter(|uuid| {
                !screens.iter().any(|screen| screen.display_uuid_opt() == Some(uuid.as_str()))
            })
            .cloned()
            .collect();
        for display_uuid in departed_displays {
            let Some(departing_space) =
                self.layout_manager.layout_engine.last_space_for_display_uuid(&display_uuid)
            else {
                continue;
            };
            let residents: Vec<WindowId> = self
                .state
                .windows
                .iter_workspace_assignments()
                .filter(|(_, assignment)| assignment.space == departing_space)
                .map(|(window, _)| window)
                .collect();
            if residents.is_empty() {
                continue;
            }
            info!(
                display_uuid,
                ?departing_space,
                window_count = residents.len(),
                "Recording display affinity for windows on a departing display"
            );
            for window in residents {
                self.layout_manager
                    .layout_engine
                    .set_window_display_home(window, departing_space);
            }
        }
        self.space_state.screens = screens;
        // The topology the launch memory is keyed by. Set from here because this is the one place the
        // attached set is authoritatively replaced.
        let connected: Vec<String> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.display_uuid_owned())
            .collect();
        self.layout_manager.layout_engine.set_connected_displays(connected);
        // Cursor warping is derived from display geometry, so it has to be told whenever
        // that geometry changes — docking, undocking, or rearranging in System Settings.
        // Pushing it from here rather than having the actor poll CGDisplayBounds keeps one
        // source of truth for the screen set.
        self.publish_cursor_warp_screens();
        if invalidates_pending_targets {
            self.clear_pending_hidden_window_targets();
        }
        if self.is_mission_control_active() {
            self.pending_space_change_manager.pending_space_change = Some(pending_space_state);
            return Ok(outcome);
        }
        for (previous_space, space) in space_remaps {
            self.layout_manager.layout_engine.remap_space(
                &mut self.state.windows,
                previous_space,
                space,
            );
        }
        // Deliberately NOT remapping a reconnected display's whole SPACE onto its new id.
        //
        // An earlier fix did exactly that, keyed by display UUID, because macOS mints a new
        // space id on every reconnect (observed 479 -> 484 -> 487 -> 516 -> 552 for one
        // monitor) and the layout saved under the old id is otherwise orphaned. It made the
        // replug worse, not better, for two reasons:
        //
        //   - The old space is a snapshot from before the unplug. Its windows were long
        //     since evacuated to the remaining display and the user carried on working
        //     there, so replaying it moved back whichever windows now occupied those slots
        //     rather than the ones that had actually been on the external.
        //   - remap_space deletes the auto-created workspaces already sitting on the target
        //     space id, and that drops the WindowStore assignments of any window macOS had
        //     placed there. Those windows were then re-assigned from scratch, which is what
        //     reshuffled the other display's strip.
        //
        // Per-window affinity handles this instead: see repatriate_windows_to_display, run
        // once the topology delta has settled. Startup space-id churn is a genuinely
        // different case and is still remapped, by reconcile_startup_spaces.
        for screen in &self.space_state.screens {
            let (Some(space), Some(display_uuid)) = (screen.space, screen.display_uuid_opt())
            else {
                continue;
            };
            self.layout_manager
                .layout_engine
                .update_space_display(space, Some(display_uuid.to_string()));
        }
        // Re-observe where windows are, and in what order, whenever the topology is settled.
        //
        // Only while the display set is UNCHANGED. During a display change the current
        // assignments are mid-evacuation and would record the wrong display; on a settled
        // topology they are exactly right.
        let display_set_unchanged =
            !display_set_changed && self.space_state.screens.len() == previous_display_uuids.len();
        if display_set_unchanged {
            self.sync_display_affinity_from_live_layout();
        }
        let current_screens = self.screens_for_current_spaces();
        self.space_activation_policy
            .on_spaces_updated(activation_config, &current_screens);
        self.recompute_and_set_active_spaces(&authoritative_spaces);
        self.restore_windows_after_fullscreen_exit(&spaces);

        for (space, size) in resized_spaces {
            if !self.is_space_active(space) {
                continue;
            }
            self.layout_manager
                .layout_engine
                .virtual_workspace_manager_mut()
                .list_workspaces(space);
            outcome = outcome.with_layout_event(LayoutEvent::SpaceExposed(space, size));
        }
        if let Some(delta) = topology_window_delta {
            outcome.absorb(self.apply_topology_window_delta(delta));
        }
        let arrived_displays: Vec<String> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.display_uuid_opt())
            .filter(|uuid| !previous_display_uuids.contains(*uuid))
            .map(str::to_owned)
            .collect();
        let active_windows = self.authoritative_active_space_windows();
        self.finalize_space_change(&spaces, active_windows, releases_lifecycle_refresh_quarantine);
        self.try_apply_pending_space_change();
        // Repatriate LAST, after finalize_space_change.
        //
        // Everything above this line derives window ownership from where macOS currently
        // reports each window, and finalize_space_change's
        // reconcile_windows_with_authoritative_spaces re-derives it for every tracked
        // window. Repatriating before that point is silently undone: the moved windows are
        // still physically on the old display when the reconciliation reads their position,
        // so it puts them straight back. Running afterwards makes affinity the last word on
        // a display arrival, which is the whole point of recording it.
        for display_uuid in arrived_displays {
            outcome.absorb(self.repatriate_windows_to_display(&display_uuid));
        }
        if should_force_refresh_layout {
            outcome = outcome.with_force_window_refresh().with_arrange_passes(1);
        }
        Ok(outcome)
    }

    fn try_apply_pending_space_change(&mut self) {
        if let Some(pending) = self.pending_space_change_manager.pending_space_change.take() {
            if pending.screens.len() == self.space_state.screens.len() {
                // During native Mission Control we must preserve the full forwarded snapshot,
                // not just the raw spaces vector, otherwise command-space and per-display space
                // metadata can remain stale after exit.
                if let Ok(outcome) = self.handle_authoritative_space_snapshot(pending) {
                    self.apply_event_outcome(outcome);
                }
            } else {
                self.pending_space_change_manager.pending_space_change = Some(pending);
            }
        }
    }

    fn repair_spaces_after_mission_control(&mut self) {
        // First, apply any SpaceChanged that arrived while MC was active.
        self.try_apply_pending_space_change();
    }

    fn on_windows_discovered_with_app_info(
        &mut self,
        pid: pid_t,
        new: Vec<(WindowId, WindowInfo)>,
        known_visible: Vec<WindowId>,
        app_info: Option<AppInfo>,
    ) {
        let app_info =
            app_info.or_else(|| self.app_manager.apps.get(&pid).map(|app| app.info.clone()));
        let inactive_windows = self
            .state
            .windows
            .iter_windows()
            .filter_map(|(wid, _)| {
                (wid.pid == pid && self.is_window_on_known_inactive_space(wid)).then_some(wid)
            })
            .collect();
        let server_observations = self
            .state
            .windows
            .iter_windows()
            .filter_map(|(wid, window)| (wid.pid == pid).then_some(window.info.sys_id).flatten())
            .map(|wsid| {
                let info = self
                    .state
                    .windows
                    .get_window_server_info(wsid)
                    .or_else(|| window_server::get_window(wsid));
                (wsid, window_discovery::StaleWindowObservation {
                    info,
                    suitable: window_server::app_window_suitability(wsid),
                    ordered_in: window_server::window_ordered_in(wsid),
                })
            })
            .collect();
        let stale_snapshot = window_discovery::StaleCleanupSnapshot {
            pending_refresh: self
                .mission_control_manager
                .pending_mission_control_refresh
                .contains(&pid),
            suppressed: matches!(
                self.refocus_manager.stale_cleanup_state,
                StaleCleanupState::Suppressed
            ),
            mission_control_active: self.is_mission_control_active(),
            drag_active: self.is_in_drag(),
            inactive_windows,
            server_observations,
        };
        let (stale_windows, pending_refresh) = window_discovery::identify_stale_windows(
            &self.state,
            pid,
            &known_visible,
            &stale_snapshot,
        );
        let mut outcome = match window_discovery::cleanup_stale_windows(
            &mut self.state,
            &self.transaction_manager,
            &mut self.drag_manager,
            &mut self.mission_control_manager,
            pid,
            stale_windows,
            pending_refresh,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                warn!(%error, pid, "window discovery cleanup failed");
                return;
            }
        };
        let observed_windows = new
            .into_iter()
            .map(|(wid, info)| {
                let current_native_space =
                    info.sys_id.and_then(|wsid| self.resolve_native_space(wsid, None));
                let active_space = self
                    .best_space_for_window(&info.frame, info.sys_id)
                    .filter(|space| self.is_space_active(*space))
                    .or_else(|| {
                        info.sys_id.is_none().then(|| self.workspace_command_space()).flatten()
                    });
                window_discovery::ObservedWindow {
                    wid,
                    info,
                    current_native_space,
                    active_space,
                }
            })
            .collect();
        let (new_windows, process_outcome) = window_discovery::process_window_list(
            &mut self.state,
            &mut self.layout_manager,
            observed_windows,
            &app_info,
        );
        outcome.absorb(process_outcome);
        window_discovery::update_window_states(&mut self.state, new_windows);

        let candidate_windows: HashSet<WindowId> = self
            .state
            .windows
            .iter_windows()
            .filter_map(|(wid, _)| (wid.pid == pid).then_some(wid))
            .chain(known_visible.iter().copied().filter(|wid| wid.pid == pid))
            .collect();
        let discovery_spaces = candidate_windows
            .iter()
            .filter_map(|wid| self.discovery_space_for_window_id(*wid).map(|space| (*wid, space)))
            .collect();
        let authoritative_spaces = candidate_windows
            .iter()
            .filter_map(|wid| {
                self.authoritative_space_for_window_id(*wid).map(|space| (*wid, space))
            })
            .collect();
        let active_spaces = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.space)
            .filter(|space| self.is_space_active(*space))
            .collect();
        let focused_window = self.focused_window_for_discovery(pid);
        outcome.absorb(window_discovery::emit_layout_events(
            &mut self.state,
            &mut self.layout_manager,
            window_discovery::EmitLayoutPayload {
                pid,
                known_visible: &known_visible,
                app_info: &app_info,
                discovery_spaces,
                authoritative_spaces,
                active_spaces,
                focused_window,
            },
        ));
        self.apply_event_outcome(outcome);
    }

    fn best_space_for_window(
        &self,
        frame: &CGRect,
        window_server_id: Option<WindowServerId>,
    ) -> Option<SpaceId> {
        if let Some(wsid) = window_server_id
            && self.is_known_fullscreen_window(wsid)
        {
            return None;
        }

        if let Some(wsid) = window_server_id {
            if let Some(space) = self.resolve_native_space(wsid, None) {
                return Some(space);
            }
        }

        if let Some(space) = self.hidden_assigned_space_for_frame(window_server_id, frame) {
            return Some(space);
        }

        self.best_space_for_frame(frame)
    }

    fn best_space_for_frame(&self, frame: &CGRect) -> Option<SpaceId> {
        let center = frame.mid();
        self.screen_for_point(center).and_then(|screen| screen.space).or_else(|| {
            self.space_state
                .screens
                .iter()
                .filter_map(|screen| {
                    let space = screen.space?;
                    let area = screen.frame.intersection(frame).area() as i64;
                    if area > 0 { Some((area, space)) } else { None }
                })
                .max_by_key(|(area, _)| *area)
                .map(|(_, space)| space)
        })
    }

    #[cfg(test)]
    fn ensure_active_drag(&mut self, wid: WindowId, frame: &CGRect) {
        let needs_new_session =
            self.get_active_drag_session().is_none_or(|session| session.window != wid);
        if needs_new_session {
            let server_id = self.state.windows.window(wid).and_then(|window| window.info.sys_id);
            let origin_space = self.best_space_for_window(frame, server_id);
            self.drag_manager.drag_state = DragState::Active {
                session: DragSession {
                    window: wid,
                    last_frame: *frame,
                    origin_space,
                    settled_space: origin_space,
                    layout_dirty: false,
                },
            };
        }
        self.drag_manager.skip_layout_for_window = Some(wid);
    }

    fn best_space_for_window_state(&self, window: &WindowState) -> Option<SpaceId> {
        self.best_space_for_window(&window.frame_monotonic, window.info.sys_id)
    }

    fn hidden_assigned_space_for_frame(
        &self,
        window_server_id: Option<WindowServerId>,
        _frame: &CGRect,
    ) -> Option<SpaceId> {
        let wsid = window_server_id?;
        let wid = self.state.windows.tracked_window_id(wsid)?;
        let assigned_space = self.assigned_space_for_window_id(wid)?;
        if !self.is_space_active(assigned_space)
            || !self.window_in_non_active_workspace(assigned_space, wid)
        {
            return None;
        }

        Some(assigned_space)
    }

    fn hidden_assigned_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        let window = self.state.windows.window(wid)?;
        self.hidden_assigned_space_for_frame(window.info.sys_id, &window.frame_monotonic)
    }

    fn assigned_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        self.layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_info_for_window_any(&self.state.windows, wid)
            .map(|info| info.space)
    }

    fn pending_target_space_for_window_server_id(&self, wsid: WindowServerId) -> Option<SpaceId> {
        let wid = self.state.windows.tracked_window_id(wsid)?;
        let target_frame = self.transaction_manager.get_target_frame(wsid)?;
        let assigned_space = self.assigned_space_for_window_id(wid)?;
        let target_space = self
            .hidden_assigned_space_for_frame(Some(wsid), &target_frame)
            .or_else(|| self.best_space_for_frame(&target_frame))?;
        (target_space == assigned_space).then_some(target_space)
    }

    fn reassign_window_to_authoritative_space(
        &mut self,
        wid: WindowId,
        authoritative_space: SpaceId,
    ) -> bool {
        self.reassign_window_to_authoritative_space_with_workspace_preservation(
            wid,
            authoritative_space,
            false,
        )
    }

    fn apply_topology_window_delta(&mut self, delta: TopologyWindowDelta) -> EventOutcome {
        let appeared: HashMap<WindowServerId, SpaceId> = delta.appeared.into_iter().collect();
        let disappeared: HashMap<WindowServerId, SpaceId> = delta.disappeared.into_iter().collect();
        let window_server_ids: HashSet<WindowServerId> =
            appeared.keys().chain(disappeared.keys()).copied().collect();
        let mut outcome = EventOutcome::default();

        for window_server_id in window_server_ids {
            let appeared_space = appeared.get(&window_server_id).copied();
            let disappeared_space = disappeared.get(&window_server_id).copied();
            let authoritative_space = self.resolve_native_space(window_server_id, appeared_space);
            if let Some(target_space) = authoritative_space {
                self.state.windows.set_window_server_space(window_server_id, Some(target_space));
                if appeared_space == Some(target_space) {
                    self.clear_pending_target_if_confirmed_space(window_server_id, target_space);
                }
                if self.is_space_active(target_space) {
                    self.state.windows.mark_window_visible(window_server_id);
                } else {
                    self.state.windows.mark_window_hidden(window_server_id);
                }
                if let Some(window) = self.state.windows.tracked_window_id(window_server_id) {
                    let restored = self.restore_fullscreen_window_to_user_space(
                        window_server_id,
                        target_space,
                        window,
                        &mut outcome,
                    );
                    if restored.is_none() {
                        self.reassign_window_to_authoritative_space_preserving_workspace_ordinal(
                            window,
                            target_space,
                        );
                    }
                }
            } else if let Some(previous_space) = disappeared_space {
                self.state
                    .windows
                    .set_window_server_space(window_server_id, Some(previous_space));
                self.state.windows.mark_window_hidden(window_server_id);
                if let Some(window) = self.state.windows.tracked_window_id(window_server_id)
                    && self.assigned_space_for_window_id(window) == Some(previous_space)
                    && self.is_space_active(previous_space)
                {
                    outcome = outcome
                        .with_layout_event(LayoutEvent::WindowRemovedPreserveFloating(window));
                }
            }
        }
        outcome
    }

    fn restore_fullscreen_window_to_user_space(
        &mut self,
        window_server_id: WindowServerId,
        space: SpaceId,
        original_window: WindowId,
        outcome: &mut EventOutcome,
    ) -> Option<bool> {
        let restored = self
            .state
            .windows
            .restore_window_from_native_fullscreen_by_window_server_id(window_server_id)
            .or_else(|| {
                self.state.windows.restore_window_from_native_fullscreen(original_window)
            })?;
        let owner = self
            .state
            .windows
            .contains_window(restored.current_window_id)
            .then_some(restored.current_window_id)
            .or_else(|| {
                restored
                    .window_server_id
                    .and_then(|id| self.state.windows.tracked_window_id(id))
            })
            .or_else(|| self.state.windows.tracked_window_id(window_server_id))
            .or_else(|| {
                self.state.windows.contains_window(original_window).then_some(original_window)
            })?;
        if owner != original_window && self.assigned_space_for_window_id(original_window).is_some()
        {
            *outcome = std::mem::take(outcome)
                .with_layout_event(LayoutEvent::WindowRemoved(original_window));
        }
        *outcome = std::mem::take(outcome).with_app_request(owner.pid, Request::GetVisibleWindows);
        Some(if self.assigned_space_for_window_id(owner) == Some(space) {
            self.is_space_active(space)
                && self.restore_window_to_active_layout_if_visible(owner, space)
        } else {
            self.reassign_window_to_authoritative_space(owner, space)
        })
    }

    /// Re-observe display affinity and strip order for every attached display.
    ///
    /// Affinity was originally written once per window and never revised, so it went stale
    /// the moment anything was rearranged: a window dragged from the external to the
    /// built-in kept its old home and was hauled back on the next replug. Reported as a
    /// Chrome and an editor window following the two terminals across.
    ///
    /// Cheap enough to run on every settled layout because it only walks the active
    /// workspace of each screen, which is what the layout pass just computed anyway.
    fn sync_display_affinity_from_live_layout(&mut self) {
        if rini_macos::display_churn::is_active() {
            return;
        }
        // Closed windows keep their affinity otherwise, because the display-change path
        // removes windows with WindowRemovedPreserveFloating, which does not clear it.
        // Measured: the external's affinity list held three long-closed windows while every
        // live window was homed to the built-in, so a replug had nothing to move back.
        self.layout_manager
            .layout_engine
            .forget_affinity_for_dead_windows(&self.state.windows);
        let attached: Vec<String> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.display_uuid_owned())
            .collect();
        let observations: Vec<(String, Vec<WindowId>)> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| {
                let (space, uuid) = (screen.space?, screen.display_uuid_owned()?);
                self.is_space_active(space).then_some((space, uuid))
            })
            .map(|(space, uuid)| {
                let windows =
                    self.layout_manager.layout_engine.ordered_windows_in_active_workspace(space);
                (uuid, windows)
            })
            .collect();
        for (uuid, windows) in observations {
            if windows.is_empty() {
                continue;
            }
            self.layout_manager
                .layout_engine
                .sync_display_affinity(&uuid, &windows, &attached);
        }
    }

    /// Spread every window back onto the display it belongs to, keeping its workspace.
    ///
    /// Recovery command. Windows can end up piled into a single workspace on a single display:
    /// the display-migration feedback loop did that before it was fixed, and any future state
    /// corruption could do it again. Fixing the loop stops it recurring but does not undo the
    /// damage, and until now the only remedy was deleting ~/.rini/layout.ron and losing every
    /// window's size and position.
    ///
    /// Only windows whose recorded home display differs from where they currently sit are
    /// moved, so running this on a healthy layout does nothing. Workspace membership is never
    /// changed — that is the window's identity, and a recovery command has no business
    /// guessing at it.
    #[cfg(test)]
    pub(crate) fn probe_cycle_app_windows(&mut self, backward: bool) -> EventOutcome {
        self.cycle_app_windows(backward)
    }

    /// Cycle focus between the focused app's windows, wherever they are.
    ///
    /// macOS's cmd-` only offers windows it considers reachable on the current Space. rini
    /// parks off-workspace windows off-screen rather than moving them to another native space,
    /// so macOS sees them but treats a parked window as not a sensible cycle target — with
    /// three Ghostty windows across two workspaces only the two sharing the visible workspace
    /// were reachable, which is what "i can only swap between the two on the same workspace"
    /// described.
    ///
    /// rini already knows where every window is, so it can rotate through all of them and let
    /// the existing focus path switch the owning display's workspace to follow. Ordering is by
    /// (space, workspace, window id) so the rotation is stable and does not depend on which
    /// workspace happens to be showing.
    fn cycle_app_windows(&mut self, backward: bool) -> EventOutcome {
        let Some(current) = self.main_window() else {
            return EventOutcome::no_change();
        };
        let pid = current.pid;

        let mut windows: Vec<(SpaceId, crate::model::VirtualWorkspaceId, WindowId)> = self
            .state
            .windows
            .iter_windows()
            .filter(|(wid, _)| wid.pid == pid)
            .map(|(wid, _)| wid)
            .filter(|wid| self.window_is_standard(*wid))
            .filter_map(|wid| {
                let assignment = self.state.windows.workspace_info_for_window(wid)?;
                Some((assignment.space, assignment.workspace_id, wid))
            })
            .collect();
        if windows.len() < 2 {
            return EventOutcome::no_change();
        }
        windows.sort_by(|a, b| {
            (a.0.get(), format!("{:?}", a.1), a.2).cmp(&(b.0.get(), format!("{:?}", b.1), b.2))
        });

        let position = windows.iter().position(|(_, _, wid)| *wid == current);
        let next = match position {
            Some(index) => {
                let len = windows.len();
                let step = if backward { len - 1 } else { 1 };
                windows[(index + step) % len].2
            }
            // The focused window is not in the list (unassigned, or filtered out), so start
            // the rotation from the beginning rather than doing nothing.
            None => windows[0].2,
        };
        if next == current {
            return EventOutcome::no_change();
        }

        let resolved_space = self.best_space_for_window_id(next);
        let space_is_active = resolved_space.is_some_and(|space| self.is_space_active(space));
        match command_workflow::handle_command_reactor_focus_window(
            &self.state,
            &self.app_manager,
            command_workflow::FocusWindowPayload {
                window_id: next,
                window_server_id: self
                    .state
                    .windows
                    .window(next)
                    .and_then(|window| window.info.sys_id),
                resolved_space,
                space_is_active,
            },
        ) {
            Ok(mut outcome) => {
                // Switch the owning display to the target's workspace, or focus lands on a
                // window that is parked off-screen and the keystroke looks like a no-op. This
                // is the same follow used by the focus-changed path.
                if let Some(space) = resolved_space {
                    outcome.absorb(self.maybe_auto_switch_to_window_workspace(pid, next, space));
                }
                outcome
            }
            Err(error) => {
                warn!(%error, ?next, "cycle app windows failed to focus");
                EventOutcome::no_change()
            }
        }
    }

    fn redistribute_windows(&mut self) -> EventOutcome {
        let mut outcome = EventOutcome::default();
        let attached: Vec<(String, SpaceId)> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| Some((screen.display_uuid_owned()?, screen.space?)))
            .collect();
        if attached.is_empty() {
            return outcome;
        }

        let mut moved = 0usize;
        for (display_uuid, target_space) in &attached {
            let windows = self.layout_manager.layout_engine.windows_to_repatriate(
                &self.state.windows,
                display_uuid,
                *target_space,
            );
            for window in windows {
                let Some(source_space) = self.assigned_space_for_window_id(window) else {
                    continue;
                };
                // Do not fight macOS over a window on a display that is no longer attached;
                // those are handled by repatriation when the display returns.
                if !attached.iter().any(|(_, space)| *space == source_space) {
                    continue;
                }
                let Some(assignment) = self.state.windows.workspace_info_for_window(window) else {
                    continue;
                };
                if self
                    .layout_manager
                    .layout_engine
                    .virtual_workspace_manager_mut()
                    .assign_window_to_workspace(
                        &mut self.state.windows,
                        *target_space,
                        window,
                        assignment.workspace_id,
                    )
                {
                    moved += 1;
                }
            }
        }

        info!(
            moved,
            displays = attached.len(),
            "Redistributed windows to their home displays"
        );
        if moved > 0 {
            outcome.absorb(EventOutcome::layout_changed(false).with_arrange_passes(1));
        }
        outcome
    }

    /// Send windows that belong on `display_uuid` back to it after it reappears.
    ///
    /// A replug used to be handled by remapping whole SPACES: the display's old space id
    /// was migrated onto its new one. That is wrong whenever the windows moved in the
    /// meantime. Unplugging evacuates the external's windows onto the built-in, where the
    /// user keeps working — closing some, opening others, reordering the strip. The old
    /// space is by then a stale snapshot, so remapping it wholesale sent back whichever
    /// windows happened to occupy those slots (Excel and Slack, in the reported case,
    /// instead of the two terminals) and reshuffled the built-in's strip on the way out.
    ///
    /// Per-window affinity replaces that: each window remembers the display it was last
    /// deliberately placed on, and only windows whose home is this display move. Windows
    /// with no affinity for it are left exactly where they are, which is what keeps the
    /// other display's column order intact.
    fn repatriate_windows_to_display(&mut self, display_uuid: &str) -> EventOutcome {
        let mut outcome = EventOutcome::default();
        let Some(target_screen) = self
            .space_state
            .screens
            .iter()
            .find(|screen| screen.display_uuid_opt() == Some(display_uuid))
            .cloned()
        else {
            return outcome;
        };
        let Some(target_space) = target_screen.space else {
            return outcome;
        };

        let windows = self.layout_manager.layout_engine.windows_to_repatriate(
            &self.state.windows,
            display_uuid,
            target_space,
        );
        if windows.is_empty() {
            return outcome;
        }

        info!(
            display_uuid,
            ?target_space,
            window_count = windows.len(),
            "Repatriating windows to their home display"
        );

        // Order matters, and this loop depends on two things to rebuild adjacency.
        //
        // `windows` arrives in the strip order last observed on this display, and
        // move_window_to_space inserts each window after the current selection and then
        // selects it. So moving them in order lays them down contiguously, in that order,
        // instead of scattering them among whatever is already on the display. Repatriating
        // in WindowId order — which is what it used to do — is why two terminals the user
        // had kept side by side came back as terminal, Chrome, terminal, editor.
        for window in windows {
            let Some(source_space) = self.assigned_space_for_window_id(window) else {
                continue;
            };
            // Place the window inside the target display's frame before reassigning it.
            //
            // Reassignment alone is not enough. WindowServer decides which space a window
            // belongs to from where it physically is, so a window still sitting on the old
            // display's coordinates gets reported back on the old space at the next
            // snapshot and the repatriation is silently undone. This mirrors what an
            // explicit move-to-display does; tiling supplies the final frame on the
            // following arrange pass.
            let mut target_frame = self
                .state
                .windows
                .window(window)
                .map(|window| window.frame_monotonic)
                .unwrap_or(target_screen.frame);
            let mut origin = target_screen.frame.mid();
            origin.x -= target_frame.size.width / 2.0;
            origin.y -= target_frame.size.height / 2.0;
            let min = target_screen.frame.min();
            let max = target_screen.frame.max();
            origin.x = origin.x.max(min.x).min((max.x - target_frame.size.width).max(min.x));
            origin.y = origin.y.max(min.y).min((max.y - target_frame.size.height).max(min.y));
            target_frame.origin = origin;

            let window_server_id =
                self.state.windows.window(window).and_then(|window| window.info.sys_id);
            if let Some(state) = self.state.windows.window_mut(window) {
                state.frame_monotonic = target_frame;
            }
            let response = self.layout_manager.layout_engine.move_window_to_space(
                &mut self.state.windows,
                source_space,
                target_space,
                target_screen.frame.size,
                window,
            );
            if let Some(window_server_id) = window_server_id {
                self.state.windows.set_window_server_space(window_server_id, Some(target_space));
                self.state.windows.mark_window_visible(window_server_id);
            }
            outcome.absorb(
                EventOutcome::layout_changed(false)
                    .with_layout_response(response, None)
                    .with_pre_layout_window_frame_write(window, target_frame, true),
            );
        }

        outcome.absorb(EventOutcome::layout_changed(false).with_arrange_passes(1));
        outcome
    }

    pub(crate) fn reassign_window_to_authoritative_space_preserving_workspace_ordinal(
        &mut self,
        wid: WindowId,
        authoritative_space: SpaceId,
    ) -> bool {
        self.reassign_window_to_authoritative_space_with_workspace_preservation(
            wid,
            authoritative_space,
            true,
        )
    }

    fn reassign_window_to_authoritative_space_with_workspace_preservation(
        &mut self,
        wid: WindowId,
        authoritative_space: SpaceId,
        preserve_workspace_ordinal: bool,
    ) -> bool {
        // Native WindowServer visibility is not enough to participate in Rini's
        // layout. Fullscreen exit can surface transient AppKit/Electron windows
        // that are visible and space-owned but are filtered out of query output.
        // Treat this as the single gate for authoritative-space reconciliation:
        // if a window is not query-manageable, remove any stale layout/workspace
        // membership instead of re-assigning it from the WindowServer snapshot.
        if !self
            .state
            .windows
            .window(wid)
            .is_some_and(|window| window.matches_filter(WindowFilter::EffectivelyManageable))
        {
            let changed_space = self.assigned_space_for_window_id(wid);
            self.send_layout_event(LayoutEvent::WindowRemoved(wid));
            return changed_space.is_some_and(|space| self.is_space_active(space));
        }

        let assigned_space = self.assigned_space_for_window_id(wid);
        if assigned_space == Some(authoritative_space) {
            return self.restore_window_to_active_layout_if_visible(wid, authoritative_space);
        }

        self.send_layout_event(LayoutEvent::WindowRemovedPreserveFloating(wid));

        let _ = self
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .list_workspaces(authoritative_space);

        let assigned = if preserve_workspace_ordinal {
            self.layout_manager
                .layout_engine
                .virtual_workspace_manager_mut()
                .assign_window_to_workspace_preserving_ordinal(
                    &mut self.state.windows,
                    authoritative_space,
                    wid,
                )
                .is_some()
        } else {
            let Some(target_workspace) = self
                .layout_manager
                .layout_engine
                .ensure_active_workspace_info(authoritative_space)
                .map(|(workspace_id, _)| workspace_id)
                .or_else(|| {
                    self.layout_manager.layout_engine.active_workspace(authoritative_space)
                })
            else {
                return assigned_space.is_some_and(|space| self.is_space_active(space));
            };

            self.layout_manager
                .layout_engine
                .virtual_workspace_manager_mut()
                .assign_window_to_workspace(
                    &mut self.state.windows,
                    authoritative_space,
                    wid,
                    target_workspace,
                )
        };
        if !assigned {
            return assigned_space.is_some_and(|space| self.is_space_active(space));
        }

        let target_active = self.is_space_active(authoritative_space);
        let _ = self.restore_window_to_active_layout_if_visible(wid, authoritative_space);

        assigned_space.is_some_and(|space| self.is_space_active(space)) || target_active
    }

    fn restore_window_to_active_layout_if_visible(
        &mut self,
        wid: WindowId,
        authoritative_space: SpaceId,
    ) -> bool {
        if !self.is_space_active(authoritative_space) {
            return false;
        }

        let Some(window) = self.state.windows.window(wid) else {
            return false;
        };
        // Same invariant as `reassign_window_to_authoritative_space`: a visible
        // WindowServer id may be a transient fullscreen projection. Do not let
        // visibility alone add it back to the active layout.
        if !window.matches_filter(WindowFilter::EffectivelyManageable) {
            self.send_layout_event(LayoutEvent::WindowRemoved(wid));
            return false;
        }

        let Some(wsid) = window.info.sys_id else {
            return false;
        };
        if !self.state.windows.is_window_visible(wsid) {
            return false;
        }

        let was_on_active_space = self.is_window_on_active_space(wid);
        self.send_layout_event(LayoutEvent::WindowAdded(authoritative_space, wid));
        !was_on_active_space && self.is_window_on_active_space(wid)
    }

    fn reconcile_windows_with_authoritative_spaces(&mut self) -> bool {
        if self.refreshes_blocked() {
            self.defer_visible_refresh(true);
            return false;
        }

        let windows: Vec<_> = self.state.windows.iter_windows().map(|(wid, _)| wid).collect();
        let mut layout_changed = false;

        for wid in windows {
            let Some(authoritative_space) = self.authoritative_space_for_window_id(wid) else {
                continue;
            };
            layout_changed |= self.reassign_window_to_authoritative_space(wid, authoritative_space);
        }

        layout_changed
    }

    fn current_reported_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        self.state
            .windows
            .window(wid)
            .and_then(|window| window.info.sys_id)
            .and_then(|wsid| self.resolve_native_space(wsid, None))
    }

    fn authoritative_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        let reported_space = self.current_reported_space_for_window_id(wid);
        if let Some(hidden_assigned_space) = self.hidden_assigned_space_for_window_id(wid) {
            return match reported_space {
                Some(space) if space != hidden_assigned_space => Some(space),
                _ => Some(hidden_assigned_space),
            };
        }

        reported_space.or_else(|| self.assigned_space_for_window_id(wid))
    }

    /// Resolve native space ownership from the strongest available source.
    ///
    /// `observation` is a direct per-space membership observation. A pending
    /// Rini move wins over an observation that is not backed by the live
    /// WindowServer state, while a live conflict is treated as a newer external
    /// move. With no direct observation, the live WindowServer query wins over
    /// the accepted prior observation and the pending target wins over stale
    /// cached state.
    pub(crate) fn resolve_native_space(
        &self,
        wsid: WindowServerId,
        observation: Option<SpaceId>,
    ) -> Option<SpaceId> {
        let pending = self.pending_target_space_for_window_server_id(wsid);
        let live = window_server::window_space(wsid);
        let prior = self.state.windows.window_server_space(wsid);

        match (observation, pending) {
            (Some(observed), Some(target)) if observed != target => {
                if live == Some(observed) {
                    Some(observed)
                } else {
                    Some(target)
                }
            }
            (Some(observed), _) => Some(observed),
            (None, _) => live.or(pending).or(prior),
        }
    }

    fn best_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        self.authoritative_space_for_window_id(wid).or_else(|| {
            self.state
                .windows
                .window(wid)
                .and_then(|window| self.best_space_for_window_state(window))
        })
    }

    fn is_window_on_known_inactive_space(&self, wid: WindowId) -> bool {
        self.authoritative_space_for_window_id(wid)
            .is_some_and(|space| !self.is_space_active(space))
    }

    fn discovery_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        let window = self.state.windows.window(wid)?;
        let authoritative = self.authoritative_space_for_window_id(wid);
        if let Some(space) = authoritative {
            return Some(space);
        }

        if let Some(space) = self.best_space_for_frame(&window.frame_monotonic)
            && self.is_space_active(space)
        {
            return Some(space);
        }

        self.best_space_for_window_id(wid)
    }

    pub(crate) fn geometry_space_for_window(
        &self,
        frame: &CGRect,
        window_server_id: Option<WindowServerId>,
    ) -> Option<SpaceId> {
        if let Some(wsid) = window_server_id
            && self.is_known_fullscreen_window(wsid)
        {
            return None;
        }

        if let Some(space) = self.hidden_assigned_space_for_frame(window_server_id, frame) {
            return Some(space);
        }

        self.best_space_for_frame(frame)
    }

    fn is_known_fullscreen_window(&self, wsid: WindowServerId) -> bool {
        self.state.windows.is_window_server_id_native_fullscreen_suspended(wsid)
    }

    /// True when a window has been parked off-strip: it overlaps its screen by no
    /// more than the hidden sliver, so nothing meaningful of it is on display.
    ///
    /// Used to sort parked columns to the back of the raise order. Floating windows
    /// are never considered parked — they are positioned by the user, and a small
    /// window near a screen edge is not the same thing as a scrolled-away column.
    fn is_window_parked_offscreen(&self, wid: WindowId) -> bool {
        // Generous relative to the 1pt parking sliver: a parked window can sit a
        // fraction of a point inside after rounding, and a genuinely useful window
        // is never this close to invisible.
        const VISIBLE_SLACK: f64 = 4.0;

        if self.layout_manager.layout_engine.is_window_floating(wid) {
            return false;
        }
        let Some(window) = self.state.windows.window(wid) else {
            return false;
        };
        let frame = window.frame_monotonic;
        let Some(screen) =
            self.screen_for_point(frame.mid()).map(|screen| screen.frame).or_else(|| {
                // A fully parked window's midpoint is outside every display, so fall
                // back to whichever screen its own space belongs to.
                self.best_space_for_window_id(wid).and_then(|space| {
                    self.space_state.screen_by_space(space).map(|screen| screen.frame)
                })
            })
        else {
            return false;
        };

        let visible_width =
            (frame.max().x.min(screen.max().x) - frame.origin.x.max(screen.origin.x)).max(0.0);
        visible_width <= VISIBLE_SLACK
    }

    fn window_center_on_known_screen(&self, wid: WindowId) -> Option<CGPoint> {
        let window_center = self.state.windows.window(wid)?.frame_monotonic.mid();
        self.screen_for_point(window_center).map(|_| window_center)
    }

    /// Send the current display geometry to the cursor-warp actor.
    ///
    /// Also re-sends the enabled flag, so a config reload that toggles
    /// `warp_cursor_between_stacked_displays` takes effect without a restart.
    fn publish_cursor_warp_screens(&self) {
        let Some(tx) = &self.communication_manager.cursor_warp_tx else {
            return;
        };
        _ = tx.send(crate::actor::cursor_warp::Request::SetEnabled(
            self.config.settings.warp_cursor_between_stacked_displays,
        ));
        _ = tx.send(crate::actor::cursor_warp::Request::SetUpperSide(
            self.config.settings.stacked_display_upper_is,
        ));
        _ = tx.send(crate::actor::cursor_warp::Request::SetLowerTopAt(
            self.config.settings.stacked_display_lower_top_at,
        ));
        _ = tx.send(crate::actor::cursor_warp::Request::ScreensChanged(
            crate::actor::cursor_warp::screens_of(&self.space_state.screens),
        ));
    }

    /// Send the active display's usable frame to the animation actor.
    ///
    /// The frame must exclude the menu bar strip. sketchybar sits at CG layer -20, below normal
    /// windows, and is visible only because nothing occupies that strip, so an overlay covering it
    /// would make the user's bar flicker on every switch. `ScreenInfo::frame` is already the usable
    /// frame, which is what makes this a straight pass-through.
    /// Places windows at their final frames immediately, with no animation.
    ///
    /// Called when the overlay animation is far enough along that the real windows are hidden behind
    /// it. Each write is a synchronous request into another process, and those land at different
    /// times, but that no longer matters: nothing is visible until the overlay comes down.
    fn apply_overlay_frames(&mut self, frames: Vec<(WindowId, CGRect)>) {
        let display = self
            .space_state
            .screens
            .iter()
            .find(|screen| screen.space == self.active_display_space())
            .or_else(|| self.space_state.screens.first())
            .map_or(CGRect::ZERO, |screen| screen.frame);
        let mut skipped = 0usize;
        for (wid, frame) in animation::frame_send_order(frames, display) {
            let Some(window) = self.state.windows.window_mut(wid) else {
                continue;
            };
            // A park-to-park move is not sent: nothing visible changes, and the app's repaint would
            // stall the compositor under the flight. Judged from where the window server says the
            // window IS, not from the model: a write the app dropped leaves the model saying
            // "parked" while the window sits on screen, and skipping on the model kept it there.
            let wsid = window.info.sys_id;
            let real = wsid.and_then(|wsid| window_server::get_window(wsid)).map(|info| info.frame);
            if !animation::frame_write_needed(real, frame, display) {
                skipped += 1;
                continue;
            }
            window.frame_monotonic = frame;
            let txid = wsid
                .map(|wsid| self.transaction_manager.generate_next_txid(wsid))
                .unwrap_or_default();
            if let Some(wsid) = wsid {
                self.transaction_manager.update_txid_entries([(wsid, txid, frame)]);
            }
            if let Some(app) = self.app_manager.apps.get(&wid.pid) {
                _ = app.handle.send(crate::actor::app::Request::SetWindowFrame(
                    wid, frame, txid, true,
                ));
            }
        }
        if skipped > 0 {
            debug!(skipped, "park-to-park frames not sent");
        }
    }

    /// Queues background captures for every window on every workspace of this space.
    ///
    /// A workspace switch is only legible when BOTH strips are drawn. Warming just the windows in the
    /// current animation left the destination blank, so a switch looked like a one-way slide into
    /// nothing and gave no sense of direction at all.
    ///
    /// Cheap to call repeatedly: the service drops targets already in flight and the cache keeps what
    /// it holds unless something better arrives, so this settles rather than re-capturing. During a
    /// flight the animation actor holds the targets until lift ("Capture work in flight" in
    /// `docs/animation-smoothness.md`), so calling this from the switch handler is safe.
    fn warm_all_workspaces(&mut self, space: SpaceId) {
        let Some(tx) = self.communication_manager.workspace_animation_tx.clone() else {
            return;
        };
        let Some(screen) = self
            .space_state
            .screens
            .iter()
            .find(|s| s.space == Some(space))
            .or_else(|| self.space_state.screens.first())
            .cloned()
        else {
            return;
        };
        let workspaces = self
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .list_workspaces(space);
        let gaps = self
            .config
            .settings
            .layout
            .gaps
            .effective_for_display(screen.display_uuid_opt());

        let mut targets: Vec<crate::ui::snapshot_service::SnapshotTarget> = Vec::new();
        for (workspace_id, _) in &workspaces {
            let layout = self.layout_manager.layout_engine.calculate_layout_for_workspace(
                &self.state.windows,
                space,
                *workspace_id,
                screen.frame,
                &gaps,
            );
            for (wid, frame) in layout {
                let Some(window) = self.state.windows.window(wid) else { continue };
                let Some(server_id) = window.info.sys_id else { continue };
                targets.push(crate::ui::snapshot_service::SnapshotTarget {
                    window: wid,
                    server_id,
                    size: frame.size,
                });
            }
        }
        if targets.is_empty() {
            return;
        }
        _ = tx.send(crate::actor::workspace_animation::Event::WarmWindows(targets));
    }

    /// Which workspace indices this switch is moving between.
    ///
    /// Called only from the workspace-switch layout path, so the currently active workspace is already
    /// the DESTINATION and the previously recorded one is the origin. Recording it here rather than
    /// intercepting the command means any route into a switch is covered.
    fn workspace_switch_indices(&mut self, space: SpaceId) -> Option<(usize, usize)> {
        let active = self.layout_manager.layout_engine.active_workspace(space)?;
        let workspaces =
            self.layout_manager.layout_engine.virtual_workspace_manager_mut().list_workspaces(space);
        let to_index = workspaces.iter().position(|(id, _)| *id == active)?;
        let previous = self.last_active_workspace.insert(space, to_index);
        match previous {
            Some(from_index) if from_index != to_index => Some((from_index, to_index)),
            // Nothing to move between, but the destination is now recorded so the next switch has an
            // origin to compare against.
            _ => None,
        }
    }

    /// Animates a strip scroll as one horizontal viewport pan, the horizontal twin of a workspace
    /// switch. Returns true when the strip surface took over.
    ///
    /// `delta` is how far every window moved. The strip surface holds the whole active workspace at its
    /// final positions, and the viewport starts shifted back by `delta` so the strip appears to
    /// arrive from where it was, then settles.
    fn start_strip_pan(
        &mut self,
        space: SpaceId,
        workspace_id: crate::model::VirtualWorkspaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
        delta: CGPoint,
    ) -> bool {
        let Some(tx) = self.communication_manager.workspace_animation_tx.clone() else {
            return false;
        };
        let Some(screen) = self
            .space_state
            .screens
            .iter()
            .find(|s| s.space == Some(space))
            .or_else(|| self.space_state.screens.first())
            .cloned()
        else {
            return false;
        };

        let gaps = self
            .config
            .settings
            .layout
            .gaps
            .effective_for_display(screen.display_uuid_opt());
        let full = self.layout_manager.layout_engine.calculate_layout_for_workspace(
            &self.state.windows,
            space,
            workspace_id,
            screen.frame,
            &gaps,
        );

        // Full-display coordinates, matching the overlay's own space.
        let display_bounds = objc2_core_graphics::CGDisplayBounds(screen.id.as_u32());
        // A floating window is not in the strip. Panning it with the strip dragged it sideways
        // and snapped it back at the handover.
        let windows = self.strip_windows_at(&full, display_bounds, true);
        if windows.is_empty() {
            return false;
        }

        // Every window in the authoritative layout is placed, so windows parked for other workspaces
        // do not linger as phantoms.
        let final_frames: Vec<(WindowId, CGRect)> = layout
            .iter()
            .copied()
            .filter(|(wid, _)| Some(*wid) != skip_wid)
            .collect();

        // The strip arrives from where it was: start the viewport shifted by the movement, settle at
        // zero.
        //
        // The sign matters and was wrong. A tile at strip-surface coordinate c appears on screen at
        // c - offset, and the strip surface is built from the NEW layout, so at t = 0 the tile must appear
        // where the window currently IS:
        //
        //     new_x - offset = old_x,  and  new_x = old_x + delta,  so  offset = delta
        //
        // Using -delta started the pan on the wrong side and moved it the wrong way. Chaining then
        // compounded the error on every press, which is what made rapid next/prev jerk back and forth.
        let from_offset = CGPoint::new(delta.x, delta.y);
        let to_offset = CGPoint::new(0.0, 0.0);
        let duration =
            std::time::Duration::from_secs_f64(self.config.settings.animation_duration.max(0.0));

        self.publish_animation_display_for(Some(space));
        _ = tx.send(crate::actor::workspace_animation::Event::AnimateStrip {
            windows,
            from_offset,
            to_offset,
            final_frames,
            focus: self.layout_manager.layout_engine.focused_window(),
            duration,
        });
        true
    }

    /// The strip surface for one workspace's layout: every window with a server id, at its layout
    /// frame expressed relative to the display's own origin (the overlay's space). `pin_floating`
    /// holds floating windows still while the strip moves under them (a pan); a vertical movement
    /// carries them with their workspace.
    fn strip_windows_at(
        &self,
        layout: &[(WindowId, CGRect)],
        display_bounds: CGRect,
        pin_floating: bool,
    ) -> Vec<crate::actor::workspace_animation::StripWindow> {
        let mut windows = Vec::with_capacity(layout.len());
        for (wid, frame) in layout {
            let Some(window) = self.state.windows.window(*wid) else { continue };
            let Some(server_id) = window.info.sys_id else { continue };
            let floating = self.layout_manager.layout_engine.is_window_floating(*wid);
            windows.push(crate::actor::workspace_animation::StripWindow {
                window: *wid,
                server_id,
                frame: CGRect::new(
                    CGPoint::new(
                        frame.origin.x - display_bounds.origin.x,
                        frame.origin.y - display_bounds.origin.y,
                    ),
                    frame.size,
                ),
                pinned: pin_floating && floating,
                floating,
            });
        }
        windows
    }

    /// Bounces the view against the end a command ran into: the strip's first or last column
    /// (`Left`/`Right`), or the top or bottom of the workspace stack (`Up`/`Down`). The active
    /// workspace's surface nudges `EDGE_BOUNCE_OVERSHOOT` the way the view was pushed and returns;
    /// the real windows do not move. See "Edge bounce" in `docs/animation-smoothness.md`.
    fn start_edge_bounce(&mut self, space: SpaceId, direction: Direction) {
        if !self.config.settings.animate || rini_macos::power::is_low_power_mode_enabled() {
            return;
        }
        let Some(tx) = self.communication_manager.workspace_animation_tx.clone() else {
            return;
        };
        let Some(screen) = self
            .space_state
            .screens
            .iter()
            .find(|s| s.space == Some(space))
            .or_else(|| self.space_state.screens.first())
            .cloned()
        else {
            return;
        };
        let Some(workspace_id) = self.layout_manager.layout_engine.active_workspace(space) else {
            return;
        };
        let gaps = self
            .config
            .settings
            .layout
            .gaps
            .effective_for_display(screen.display_uuid_opt());
        let layout = self.layout_manager.layout_engine.calculate_layout_for_workspace(
            &self.state.windows,
            space,
            workspace_id,
            screen.frame,
            &gaps,
        );
        let display_bounds = objc2_core_graphics::CGDisplayBounds(screen.id.as_u32());
        let vertical = matches!(direction, Direction::Up | Direction::Down);
        // A strip bounce leaves floating windows where they are, as a pan does; a stack bounce
        // carries them, as a switch does.
        let windows = self.strip_windows_at(&layout, display_bounds, !vertical);
        if windows.is_empty() {
            return;
        }
        let overshoot = crate::actor::workspace_animation::edge_bounce_overshoot(direction);
        let duration =
            std::time::Duration::from_secs_f64(self.config.settings.animation_duration.max(0.0));
        tracing::debug!(?direction, windows = windows.len(), "edge bounce");
        self.publish_animation_display_for(Some(space));
        _ = tx.send(crate::actor::workspace_animation::Event::BounceStrip {
            windows,
            overshoot,
            final_frames: layout,
            focus: self.layout_manager.layout_engine.focused_window(),
            duration,
        });
    }

    /// Builds and starts a strip-surface animation for a workspace switch, if one is warranted.
    ///
    /// Returns true when the strip surface took over, in which case the caller must not touch the real
    /// windows: the animation actor asks for them once it is covering them.
    ///
    /// Every workspace between the two is laid out and stacked below the one above it, so a jump from
    /// 1 to 4 scrolls past 2 and 3. Without that, a four-workspace jump looks exactly like a
    /// one-workspace step, which gives no sense of where you have moved to.
    fn start_strip_switch(
        &mut self,
        space: SpaceId,
        from_index: usize,
        to_index: usize,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
    ) -> bool {
        if from_index == to_index {
            return false;
        }
        tracing::debug!(from_index, to_index, "strip switch requested");
        let Some(tx) = self.communication_manager.workspace_animation_tx.clone() else {
            return false;
        };
        let Some(screen) = self
            .space_state
            .screens
            .iter()
            .find(|s| s.space == Some(space))
            .or_else(|| self.space_state.screens.first())
            .cloned()
        else {
            return false;
        };

        let workspaces = self
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .list_workspaces(space);
        if workspaces.is_empty() {
            return false;
        }

        let gaps = self
            .config
            .settings
            .layout
            .gaps
            .effective_for_display(screen.display_uuid_opt());

        let low = from_index.min(to_index);
        let high = from_index.max(to_index);

        // Strips are stacked with a GAP between them equal to the menu bar inset, so a switch reads as
        // one strip sliding up out of view just above the bar while the next arrives from below,
        // rather than two strips welded edge to edge.
        //
        // The inset is the difference between the display's full height and its usable height, which
        // is exactly the space the menu bar and the bar sitting in it occupy. Adding it to the usable
        // height makes the row pitch the FULL display height.
        // The overlay spans the full display, so the strip surface is expressed in full-display coordinates
        // and the row pitch is simply the full display height. That pitch already contains the menu
        // bar gap, because each workspace's windows start below the bar within their own row.
        let display_bounds = objc2_core_graphics::CGDisplayBounds(screen.id.as_u32());
        let row_pitch = display_bounds.size.height;
        let height = row_pitch;

        let mut windows: Vec<crate::actor::workspace_animation::StripWindow> = Vec::new();
        let mut final_frames: Vec<(WindowId, CGRect)> = Vec::new();
        for index in low..=high {
            let Some((workspace_id, _)) = workspaces.get(index) else { continue };
            let layout = self.layout_manager.layout_engine.calculate_layout_for_workspace(
                &self.state.windows,
                space,
                *workspace_id,
                screen.frame,
                &gaps,
            );
            // Stacked below the workspace above it, separated by the menu bar inset, and expressed
            // relative to the display's own origin so the overlay's space needs no further translation.
            let row = crate::model::strip_stack::row_of(index, from_index, to_index);
            for (wid, frame) in layout {
                let Some(window) = self.state.windows.window(wid) else { continue };
                let Some(server_id) = window.info.sys_id else { continue };
                windows.push(crate::actor::workspace_animation::StripWindow {
                    window: wid,
                    server_id,
                    frame: crate::model::strip_stack::strip_frame(
                        frame,
                        display_bounds.origin,
                        row,
                        row_pitch,
                    ),
                    // Never pinned here. A floating window belongs to a workspace, so it leaves with the
                    // one being left and arrives with the one being entered.
                    pinned: false,
                    floating: self.layout_manager.layout_engine.is_window_floating(wid),
                });
                let _ = &mut final_frames;
            }
        }

        // EVERY window in the layout, not just the destination workspace's. This layout is what the
        // non-animated path would have applied, and it includes the parked positions of windows
        // belonging to other workspaces. Placing only the destination's windows left all the others
        // wherever they happened to be, so windows from one workspace showed up as phantoms on
        // another until something else moved the strip.
        final_frames = layout
            .iter()
            .copied()
            .filter(|(wid, _)| Some(*wid) != skip_wid)
            .collect();

        tracing::debug!(
            from_index,
            to_index,
            workspaces = workspaces.len(),
            strip_windows = windows.len(),
            final_frames = final_frames.len(),
            height,
            "strip switch build"
        );
        if windows.is_empty() {
            return false;
        }

        let travel = crate::model::strip_stack::travel(from_index, to_index, height);
        let (from_offset, to_offset) = (travel.from, travel.to);
        let duration = std::time::Duration::from_secs_f64(
            (self.config.settings.animation_duration.max(0.0)) * travel.duration_stretch,
        );

        // The switch owns the destination's horizontal position. Its row is drawn at that scroll already, so
        // the strip arrives with the target window in view and the only movement the eye sees is vertical.
        // Without claiming it, the next pass reads the destination's offset as a fresh movement and pans
        // sideways after the slide has landed.
        if let Some(destination) = self.layout_manager.layout_engine.active_workspace(space)
            && let Some(offset) = self.layout_manager.layout_engine.strip_scroll_offset(space)
        {
            self.last_strip_offset.insert((space, destination), offset);
        }

        // That claim suppresses the pan that would otherwise reveal the target, so if the destination row
        // was NOT drawn scrolled to it, the target stays off screen. That would be a layout problem rather
        // than an animation one, and it is silent, so say so.
        if let Some(target) = self.layout_manager.layout_engine.focused_window()
            && let Some(placed) = windows.iter().find(|window| window.window == target)
            && !crate::model::strip_stack::lands_in_view(placed.frame, display_bounds.size.width)
        {
            tracing::debug!(
                idx = target.idx.get(),
                x = placed.frame.origin.x,
                width = display_bounds.size.width,
                "the switch target is off the destination row's viewport and will not be revealed"
            );
        }

        self.publish_animation_display_for(Some(space));
        _ = tx.send(crate::actor::workspace_animation::Event::AnimateStrip {
            windows,
            from_offset,
            to_offset,
            final_frames,
            focus: self.layout_manager.layout_engine.focused_window(),
            duration,
        });
        // Every workspace, so the next switch in any direction has both strips drawn. Stays here:
        // the animation actor defers it until the flight lifts (`docs/animation-smoothness.md`).
        self.warm_all_workspaces(space);
        true
    }

    /// Asks for fresh pictures of the window gaining focus and the one losing it.
    ///
    /// A window's appearance depends on focus and none of the difference is a size change: measured on a 1pt
    /// window border, 65 of 255 focused against 42 unfocused. The ordinary warm only recaptures a window
    /// whose picture no longer FITS, so an unfocused capture stayed forever and its tile popped to the
    /// focused rendering when the overlay lifted. Background work, so this costs nothing on the way in.
    fn refresh_focus_pictures(&mut self, gaining: WindowId) {
        let Some(tx) = &self.communication_manager.workspace_animation_tx else { return };
        let losing = self
            .layout_manager
            .layout_engine
            .focused_window()
            .filter(|previous| *previous != gaining);
        for window in [Some(gaining), losing].into_iter().flatten() {
            let Some(state) = self.state.windows.window(window) else { continue };
            let Some(server_id) = state.info.sys_id else { continue };
            _ = tx.send(crate::actor::workspace_animation::Event::RefreshFocus(
                crate::ui::snapshot_service::SnapshotTarget {
                    window,
                    server_id,
                    size: state.frame_monotonic.size,
                },
            ));
        }
    }

    /// How far the ACTIVE workspace's strip has moved since a pass last looked at that strip, and remember
    /// where it is now.
    ///
    /// `None` when the space has no strip. `Some(0)` is meaningful and different: it says the strip did not
    /// move, so whatever else the layout did is not a pan. Windows move OPPOSITE to the viewport, so the
    /// sign is negated: scrolling the viewport further along the strip carries the windows left.
    ///
    /// The first look at a workspace reports no movement, which is what a switch onto it needs: the switch
    /// itself is vertical, and the destination's own scroll is drawn into the strip surface rather than panned to.
    pub(crate) fn take_strip_movement(&mut self, space: SpaceId) -> Option<CGPoint> {
        let workspace = self.layout_manager.layout_engine.active_workspace(space)?;
        let now = self.layout_manager.layout_engine.strip_scroll_offset(space)?;
        let before = self.last_strip_offset.insert((space, workspace), now);
        tracing::debug!(
            now,
            before = before.unwrap_or(now),
            focused = format!("{:?}", self.layout_manager.layout_engine.focused_window()),
            "strip movement"
        );
        Some(CGPoint::new(-(now - before.unwrap_or(now)), 0.0))
    }

    pub(crate) fn publish_animation_display(&self) {
        self.publish_animation_display_for(None);
    }

    /// Points the animation overlay at the display holding `space`.
    ///
    /// There is one overlay, so it has to sit on the display whose windows are about to move. Choosing the
    /// ACTIVE display instead put a built-in workspace switch on the external screen whenever the cursor
    /// was over there: the external showed the built-in's windows sliding while the built-in's own windows
    /// snapped with no animation. `space` is `None` only for the debug commands and a config reload, which
    /// have no particular display in mind.
    pub(crate) fn publish_animation_display_for(&self, space: Option<SpaceId>) {
        let Some(tx) = &self.communication_manager.workspace_animation_tx else {
            return;
        };
        let wanted = space.or_else(|| self.active_display_space());
        let Some(screen) = self
            .space_state
            .screens
            .iter()
            .find(|screen| screen.space == wanted)
            .or_else(|| self.space_state.screens.first())
        else {
            return;
        };
        _ = tx.send(crate::actor::workspace_animation::Event::SetDisplay {
            id: screen.id.as_u32(),
            frame: objc2_core_graphics::CGDisplayBounds(screen.id.as_u32()),
            // Backing scale is not carried on ScreenInfo. Every display rini has been run on is
            // Retina, and a wrong scale only affects bitmap crispness rather than geometry, so 2.0
            // is a safe default until there is a reason to plumb the real value through.
            scale: 2.0,
        });
    }

    pub fn warp_mouse(&mut self, point: CGPoint) {
        let Some(event_tap_tx) = self.communication_manager.event_tap_tx.clone() else {
            return;
        };
        _ = event_tap_tx.send(crate::actor::event_tap::Request::Warp(point));
    }

    fn warp_mouse_to_space_center(&mut self, space: SpaceId) -> bool {
        let Some(screen) = self.space_state.screen_by_space(space) else {
            return false;
        };
        self.warp_mouse(screen.frame.mid());
        true
    }

    fn try_focus_or_warp_without_raise(
        &mut self,
        warp_space: Option<SpaceId>,
        focus_window: &mut Option<WindowId>,
    ) -> bool {
        if let Some(wid) = self.window_id_under_cursor() {
            *focus_window = Some(wid);
            return false;
        }
        if self.focus_untracked_window_under_cursor() {
            return true;
        }
        self.config.settings.mouse_follows_focus
            && warp_space.is_some_and(|space| self.warp_mouse_to_space_center(space))
    }

    fn insert_app_handle_for_window(
        &self,
        app_handles: &mut HashMap<pid_t, AppThreadHandle>,
        wid: WindowId,
    ) {
        if let Some(app) = self.app_manager.apps.get(&wid.pid) {
            app_handles.insert(wid.pid, app.handle.clone());
        }
    }

    fn expose_all_spaces(&mut self) {
        let spaces: Vec<SpaceId> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.space)
            .filter(|space| self.is_space_active(*space))
            .collect();
        for space in spaces {
            self.expose_space_if_known(space);
        }
    }

    #[cfg(test)]
    pub(crate) fn window_is_standard_for_test(&self, id: WindowId) -> bool {
        self.window_is_standard(id)
    }

    fn window_is_standard(&self, id: WindowId) -> bool {
        self.state
            .windows
            .window(id)
            .is_some_and(|window| window.matches_filter(WindowFilter::EffectivelyManageable))
    }

    pub(crate) fn visible_spaces_for_layout(
        &self,
        include_inactive: bool,
    ) -> (Vec<SpaceId>, HashMap<SpaceId, CGPoint>) {
        let visible_spaces_input: Vec<(SpaceId, CGPoint)> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| {
                let space = screen.space?;
                if !include_inactive && !self.is_space_active(space) {
                    return None;
                }
                Some((space, screen.frame.mid()))
            })
            .collect();

        let mut visible_space_centers = HashMap::default();
        for (space, center) in &visible_spaces_input {
            visible_space_centers.insert(*space, *center);
        }

        let visible_spaces = order_visible_spaces_by_position(visible_spaces_input.iter().cloned());

        (visible_spaces, visible_space_centers)
    }

    fn send_layout_event(&mut self, event: LayoutEvent) {
        let focus_changed = matches!(
            &event,
            LayoutEvent::WindowFocused(_, window)
                if self.layout_manager.layout_engine.focused_window() != Some(*window)
        );
        let event_space = match &event {
            LayoutEvent::WindowFocused(space, _) => Some(*space),
            _ => None,
        };
        let focus_desktop = matches!(
            event,
            LayoutEvent::WindowRemoved(wid)
                if self.layout_manager.layout_engine.focused_window() == Some(wid)
        );
        let event_clone = event.clone();
        let layout_outcome =
            self.layout_manager.layout_engine.handle_event(&mut self.state.windows, event);
        let mut response = layout_outcome.response;
        let (placements, resizes, workspace_focus) = layout_outcome.app_rules.into_parts();
        self.apply_app_rule_placements(placements);
        self.apply_app_rule_resizes(resizes);
        let workspace_switch_space = workspace_focus.map(|request| request.space);
        if let Some(request) = workspace_focus {
            self.store_current_floating_positions(request.space);
            self.workspace_switch_manager
                .start_workspace_switch(WorkspaceSwitchOrigin::Auto);
            response = self.layout_manager.layout_engine.switch_to_workspace_with_focus(
                &self.state.windows,
                request.space,
                request.workspace_index,
                request.window,
            );
        }
        if focus_changed && let Some(event_tap_tx) = &self.communication_manager.event_tap_tx {
            _ = event_tap_tx.send(crate::actor::event_tap::Request::HideOnFocus);
        }
        let geometry_changed = response.changed;
        self.prepare_refocus_after_layout_event(&event_clone);
        self.handle_layout_response(response, workspace_switch_space);
        if geometry_changed {
            let layout_changed = self.update_layout_or_warn(
                false,
                workspace_switch_space.is_some(),
                workspace_switch_space.or(event_space),
            );
            // A focus report onto a parked window (cmd-tab) scrolls it in here; if it was behind a
            // floating window it has to be lifted now that it is on screen.
            if layout_changed {
                self.regroup_after_layout();
            }
        }
        if focus_desktop && let Some(space) = self.workspace_command_space() {
            self.focus_desktop_if_active_workspace_empty(space);
        }
        for space in self.space_state.iter_known_spaces() {
            self.layout_manager.layout_engine.debug_tree_desc(space, "after event", false);
        }
    }

    fn apply_app_rule_placements(
        &mut self,
        placements: Vec<crate::model::app_rules::AppRulePlacement>,
    ) {
        for placement in placements {
            let Some(window) = self.state.windows.window(placement.window) else {
                continue;
            };
            let frame = if placement.position.is_some() {
                let Some(screen) = self.space_state.screen_by_space(placement.space) else {
                    warn!(
                        window = ?placement.window,
                        space = ?placement.space,
                        "could not apply app-rule position without screen geometry"
                    );
                    continue;
                };
                placement.resolve_frame(window.frame_monotonic, screen.frame)
            } else {
                placement.resolve_frame(window.frame_monotonic, CGRect::default())
            };

            let window_server_id = window.info.sys_id;
            let transaction = if let Some(window_server_id) = window_server_id {
                let transaction = self.transaction_manager.generate_next_txid(window_server_id);
                self.transaction_manager.store_txid(window_server_id, transaction, frame);
                transaction
            } else {
                TransactionId::default()
            };
            if let Some(app) = self.app_manager.apps.get(&placement.window.pid)
                && let Err(error) = app.handle.send(Request::SetWindowFrame(
                    placement.window,
                    frame,
                    transaction,
                    true,
                ))
            {
                warn!(window = ?placement.window, %error, "failed to apply app-rule placement");
            }
        }
    }

    fn apply_app_rule_resizes(&mut self, resizes: Vec<crate::model::app_rules::AppRuleResize>) {
        for resize in resizes {
            let Some(window) = self.state.windows.window(resize.window) else {
                continue;
            };
            let Some(screen) = self.space_state.screen_by_space(resize.space) else {
                warn!(
                    window = ?resize.window,
                    space = ?resize.space,
                    "could not apply app-rule resize without screen geometry"
                );
                continue;
            };
            let old_frame = window.frame_monotonic;
            let mut new_frame = old_frame;
            if let Some(width) = resize.size.w {
                new_frame.size.width = width;
            }
            if let Some(height) = resize.size.h {
                new_frame.size.height = height;
            }
            self.layout_manager.layout_engine.apply_app_rule_resize(
                resize,
                old_frame,
                new_frame,
                screen.frame,
                Some(screen.display_uuid.as_str()),
            );
        }
    }

    // Returns true if the window should be raised on mouse over considering
    // active workspace membership and potential occlusion of floating windows above it.
    pub(crate) fn should_raise_on_mouse_over(&self, wid: WindowId) -> bool {
        let Some(window) = self.state.windows.window(wid) else {
            return false;
        };

        if !window.matches_filter(WindowFilter::EffectivelyManageable)
            && !self.layout_manager.layout_engine.is_window_floating(wid)
        {
            return false;
        }

        let candidate_frame = window.frame_monotonic;

        if matches!(self.menu_manager.menu_state, MenuState::Open(_)) {
            trace!(?wid, "Skipping autoraise while menu open");
            return false;
        }

        let Some(space) = self.best_space_for_window(&candidate_frame, window.info.sys_id) else {
            return false;
        };
        if !self.is_space_active(space) {
            return false;
        }

        if !self.layout_manager.layout_engine.is_window_in_active_workspace(
            &self.state.windows,
            space,
            wid,
        ) {
            trace!("Ignoring mouse over window {:?} - not in active workspace", wid);
            return false;
        }

        let Some(candidate_wsid) = window.info.sys_id else {
            return true;
        };

        let order = {
            let space_id = space.get();
            rini_macos::window_server::space_window_list_for_connection(&[space_id], 0, false)
        };
        let candidate_u32 = candidate_wsid.as_u32();
        let candidate_level = window_level(candidate_u32);
        let candidate_sub_level = window_sub_level(candidate_u32);

        for above_u32 in order {
            if above_u32 == candidate_u32 {
                break;
            }

            let above_wsid = WindowServerId::new(above_u32);
            let Some(above_wid) = self.state.windows.tracked_window_id(above_wsid) else {
                continue;
            };

            if !self.layout_manager.layout_engine.is_window_floating(above_wid) {
                continue;
            }

            let Some(above_state) = self.state.windows.window(above_wid) else {
                continue;
            };
            let above_frame = above_state.frame_monotonic;
            if !candidate_frame.contains_rect(above_frame) {
                continue;
            }

            let above_level = window_level(above_u32);
            let above_sub_level = window_sub_level(above_u32);
            if candidate_level
                .zip(above_level)
                .is_some_and(|(candidate, above)| candidate == above)
                && candidate_sub_level == above_sub_level
            {
                return false;
            }
        }

        true
    }

    fn process_windows_for_app_rules(
        &mut self,
        pid: pid_t,
        window_ids: Vec<WindowId>,
        app_info: AppInfo,
    ) {
        if window_ids.is_empty() {
            return;
        }

        let mut windows_by_space: BTreeMap<SpaceId, Vec<WindowId>> = BTreeMap::new();
        for &wid in &window_ids {
            let Some(state) = self.state.windows.window(wid) else {
                continue;
            };
            if !state.matches_filter(WindowFilter::Manageable) {
                continue;
            }
            let Some(space) = self.best_space_for_window_id(wid) else {
                continue;
            };
            windows_by_space.entry(space).or_default().push(wid);
        }

        for (space, wids) in windows_by_space {
            if !self.is_space_active(space) {
                continue;
            }
            let mut windows_needing_layout_refresh: Vec<WindowId> = Vec::new();

            for wid in &wids {
                let (was_assigned, was_floating, was_ignored) = {
                    let engine = &self.layout_manager.layout_engine;
                    (
                        engine
                            .virtual_workspace_manager()
                            .workspace_for_window(&self.state.windows, space, *wid)
                            .is_some(),
                        engine.is_window_floating(*wid),
                        self.state
                            .windows
                            .window(*wid)
                            .map(|window| window.ignore_app_rule)
                            .unwrap_or(false),
                    )
                };
                let assign_result = {
                    let window_metadata = self.state.windows.window(*wid).map(|window| {
                        (
                            window.info.title.clone(),
                            window.info.ax_role.clone(),
                            window.info.ax_subrole.clone(),
                            window.info.is_modal,
                        )
                    });
                    self.layout_manager.layout_engine.assign_window_with_app_info(
                        &mut self.state.windows,
                        *wid,
                        space,
                        app_info.bundle_id.as_deref(),
                        app_info.localized_name.as_deref(),
                        window_metadata.as_ref().map(|metadata| metadata.0.as_str()),
                        window_metadata.as_ref().and_then(|metadata| metadata.1.as_deref()),
                        window_metadata.as_ref().and_then(|metadata| metadata.2.as_deref()),
                        window_metadata.as_ref().is_some_and(|metadata| metadata.3),
                    )
                };

                match assign_result {
                    Ok(AppRuleResult::Managed(assignment)) => {
                        if let Some(window) = self.state.windows.window_mut(*wid) {
                            window.ignore_app_rule = false;
                        }

                        let effective_floating =
                            assignment.floating || (!assignment.prev_rule_decision && was_floating);
                        let needs_layout_refresh =
                            !was_assigned || was_floating != effective_floating || was_ignored;
                        if needs_layout_refresh {
                            windows_needing_layout_refresh.push(*wid);
                        }
                    }
                    Ok(AppRuleResult::Unmanaged) => {
                        if let Some(window) = self.state.windows.window_mut(*wid) {
                            window.ignore_app_rule = true;
                        }

                        let needs_removal = {
                            let engine = &self.layout_manager.layout_engine;
                            engine
                                .virtual_workspace_manager()
                                .workspace_for_window(&self.state.windows, space, *wid)
                                .is_some()
                                || engine.is_window_floating(*wid)
                        };
                        if needs_removal {
                            self.send_layout_event(LayoutEvent::WindowRemoved(*wid));
                        }
                    }
                    Err(e) => {
                        warn!("Failed to assign window {:?} to workspace: {:?}", wid, e);
                        if let Some(window) = self.state.windows.window_mut(*wid) {
                            window.ignore_app_rule = false;
                        }

                        if !was_assigned || was_ignored {
                            windows_needing_layout_refresh.push(*wid);
                        }
                    }
                }
            }

            if windows_needing_layout_refresh.is_empty() {
                continue;
            }

            let windows_with_titles: Vec<(
                WindowId,
                Option<String>,
                Option<String>,
                Option<String>,
                bool,
                bool,
                CGSize,
                Option<CGSize>,
                Option<CGSize>,
            )> = windows_needing_layout_refresh
                .iter()
                .map(|&wid| {
                    let window = self.state.windows.window(wid);
                    let title_opt = window.map(|w| w.info.title.clone());
                    let ax_role = window.and_then(|w| w.info.ax_role.clone());
                    let ax_subrole = window.and_then(|w| w.info.ax_subrole.clone());
                    let is_modal = window.is_some_and(|w| w.info.is_modal);
                    let is_resizable = window.map_or(true, |w| w.info.is_resizable);
                    let size_hint =
                        window.map_or(CGSize::new(0.0, 0.0), |w| w.frame_monotonic.size);
                    let min_size = window.and_then(|w| w.info.min_size);
                    let max_size = window.and_then(|w| w.info.max_size);
                    (
                        wid,
                        title_opt,
                        ax_role,
                        ax_subrole,
                        is_modal,
                        is_resizable,
                        size_hint,
                        min_size,
                        max_size,
                    )
                })
                .collect();

            self.send_layout_event(LayoutEvent::WindowsOnScreenUpdated(
                space,
                pid,
                windows_with_titles,
                Some(app_info.clone()),
            ));
        }
    }

    fn handle_app_activation_workspace_switch(&mut self, pid: pid_t) -> EventOutcome {
        if self.workspace_switch_manager.active_workspace_switch.is_some() {
            trace!(
                "Skipping auto workspace switch for pid {} because a workspace switch is in progress",
                pid
            );
            return EventOutcome::no_change();
        }

        if self.workspace_switch_manager.manual_switch_in_progress() {
            debug!(
                "Skipping auto workspace switch for pid {} because a manual switch is in progress",
                pid
            );
            return EventOutcome::no_change();
        }

        if let Some(active_space) = self.raw_command_space()
            && self.is_fullscreen_space(active_space)
        {
            debug!(
                "Skipping auto workspace switch for pid {} because the active space is fullscreen",
                pid
            );
            return EventOutcome::no_change();
        }

        if let Some(wsid) = self.activation_from_unmanageable_window(pid) {
            debug!(
                ?wsid,
                "Skipping auto workspace switch for pid {} because the activated window is not manageable",
                pid
            );
            return EventOutcome::no_change();
        }

        let Some(bundle_id_str) =
            self.app_manager.apps.get(&pid).and_then(|app| app.info.bundle_id.clone())
        else {
            return EventOutcome::no_change();
        };

        if self.config.settings.auto_focus_blacklist.contains(&bundle_id_str) {
            debug!(
                "App {} is blacklisted for auto-focus workspace switching, ignoring activation",
                bundle_id_str
            );
            return EventOutcome::no_change();
        }

        debug!(
            "App activation detected: {} (pid: {}), checking for workspace switch",
            bundle_id_str, pid
        );

        // Carbon activation is reconciled by the app thread before this runs,
        // so a missing main window means there is no authoritative switch
        // target. Picking an arbitrary window for the process is especially
        // unsafe for apps whose windows span multiple virtual workspaces.
        let app_window =
            self.main_window().filter(|wid| wid.pid == pid && self.window_is_standard(*wid));

        let Some(app_window_id) = app_window else {
            return EventOutcome::no_change();
        };

        let Some(window_space) = self.best_space_for_window_id(app_window_id) else {
            return EventOutcome::no_change();
        };

        self.maybe_auto_switch_to_window_workspace(pid, app_window_id, window_space)
    }

    /// Sends focus back to the window the app was in, when an activation picked a parked one.
    ///
    /// macOS chooses the window on cmd-tab, and it chose a window rini had parked off screen for a
    /// workspace it was not showing. Following that switched the parked window's workspace, animated a
    /// display the user had not asked to move, and landed them in the wrong terminal. Only ever avoids a
    /// switch: it acts when the pick is parked and the remembered window is visible.
    fn activation_redirect(&mut self, picked: WindowId, picked_space: SpaceId) -> Option<EventOutcome> {
        // Peeked, not taken: when this does not redirect, the workspace switch below may still
        // use the remembered window (`maybe_auto_switch_to_window_workspace`).
        let remembered = self.main_window_tracker.peek_activation_target(picked.pid)?;
        let target = main_window::activation_focus_target(
            picked,
            self.window_is_in_active_workspace(picked, Some(picked_space)),
            Some(remembered),
            self.window_is_standard(remembered)
                && self.window_is_in_active_workspace(remembered, None),
        )?;
        self.main_window_tracker.take_activation_target(picked.pid);
        debug!(
            ?picked,
            ?target,
            "app activation picked a parked window; focusing the one it was in instead"
        );
        let mut app_handles = HashMap::default();
        if let Some(app) = self.app_manager.apps.get(&target.pid) {
            app_handles.insert(target.pid, app.handle.clone());
        }
        let space = self.best_space_for_window_id(target);
        let mut outcome =
            EventOutcome::default().with_raise_request(crate::actor::raise_manager::Event::RaiseRequest(
                crate::actor::raise_manager::RaiseRequest {
                    raise_windows: vec![vec![target]],
                    focus_window: Some((target, None)),
                    app_handles,
                    focus_quiet: Quiet::No,
                },
            ));
        if let Some(space) = space {
            outcome = outcome.with_layout_event(LayoutEvent::WindowFocused(space, target));
        }
        Some(outcome)
    }

    /// Whether `window` is in the active workspace of the space it belongs to.
    fn window_is_in_active_workspace(&self, window: WindowId, space: Option<SpaceId>) -> bool {
        let Some(space) = space.or_else(|| self.best_space_for_window_id(window)) else {
            return false;
        };
        self.is_space_active(space)
            && self.layout_manager.layout_engine.is_window_in_active_workspace(
                &self.state.windows,
                space,
                window,
            )
    }

    fn maybe_auto_switch_to_window_workspace(
        &mut self,
        pid: pid_t,
        app_window_id: WindowId,
        window_space: SpaceId,
    ) -> EventOutcome {
        let workspace_state = self.layout_manager.layout_engine.virtual_workspace_manager();
        let Some(window_workspace) =
            workspace_state.workspace_for_window(&self.state.windows, window_space, app_window_id)
        else {
            return EventOutcome::no_change();
        };
        // macOS picks the app's main window on cmd-tab; the user was in another window of the app
        // on the same workspace (Kiro, two windows on one row: the pick scrolled the strip to the
        // far one). The switch is right, the window is the remembered one.
        let remembered = self.main_window_tracker.take_activation_target(pid).filter(|wid| {
            self.window_is_standard(*wid)
                && workspace_state.workspace_for_window(&self.state.windows, window_space, *wid)
                    == Some(window_workspace)
        });
        let picked = app_window_id;
        let app_window_id = switch_focus_within_workspace(app_window_id, remembered);
        if app_window_id != picked {
            debug!(
                ?picked,
                target = ?app_window_id,
                "app activation picked another of its windows on the same workspace; focusing the one the user was in"
            );
        }

        let Some(current_workspace) =
            self.layout_manager.layout_engine.active_workspace(window_space)
        else {
            return EventOutcome::no_change();
        };

        if window_workspace != current_workspace {
            let workspaces = self
                .layout_manager
                .layout_engine
                .virtual_workspace_manager_mut()
                .list_workspaces(window_space);
            if let Some((workspace_index, _)) =
                workspaces.iter().enumerate().find(|(_, (ws_id, _))| *ws_id == window_workspace)
            {
                debug!(
                    "Auto-switching to workspace {} for activated app (pid: {})",
                    workspace_index, pid
                );

                self.store_current_floating_positions(window_space);
                self.workspace_switch_manager
                    .start_workspace_switch(WorkspaceSwitchOrigin::Auto);

                let response = self.layout_manager.layout_engine.switch_to_workspace_with_focus(
                    &self.state.windows,
                    window_space,
                    workspace_index,
                    app_window_id,
                );
                return EventOutcome::layout_changed(false)
                    .with_layout_response(response, Some(window_space));
            }
        }

        EventOutcome::no_change()
    }

    fn handle_layout_response(
        &mut self,
        response: layout::EventResponse,
        workspace_switch_space: Option<SpaceId>,
    ) {
        if self.is_in_drag() {
            self.workspace_switch_manager.mark_workspace_switch_inactive();
            return;
        }

        let mut pending_refocus_space =
            match std::mem::replace(&mut self.refocus_manager.refocus_state, RefocusState::None) {
                RefocusState::Pending(space) => Some(space),
                RefocusState::None => None,
            };
        let layout::EventResponse {
            changed,
            raise_windows,
            mut focus_window,
            boundary_hit,
            edge_hit,
        } = response;

        // The command ran into an end of the strip or of the workspace stack. The view bounces so
        // the stop reads as an edge, not a dropped keypress. A blocked workspace step changed
        // nothing and names no window, so it ends here: the switch-active focus fallbacks below
        // would otherwise re-focus whatever sits under the cursor.
        if let Some(direction) = edge_hit {
            let space = workspace_switch_space.or_else(|| self.command_context_space());
            if let Some(space) = space {
                self.start_edge_bounce(space, direction);
            }
            if !changed && raise_windows.is_empty() && focus_window.is_none() {
                self.workspace_switch_manager.mark_workspace_switch_inactive();
                return;
            }
        }

        // The window the switch is FOR: it is parked until the layout lands, so the window server
        // may well report it off screen right now. The visibility filters below must not trade it
        // for whichever window of the workspace happens to be visible (an auto-switch onto Kiro
        // landed on a Ghostty window that way, and remembered Ghostty for the next time).
        let mut switch_target: Option<WindowId> = None;
        if let Some(space) = workspace_switch_space
            && matches!(
                self.workspace_switch_manager.workspace_switch_state,
                WorkspaceSwitchState::Active
            )
        {
            focus_window = self.visible_focus_candidate_in_active_workspace(space, focus_window);
            switch_target = focus_window;
        }

        if let Some(dir) = boundary_hit
            && self.config.settings.layout.scrolling.gestures.propagate_to_workspace_swipe
        {
            let skip_empty = self.config.settings.gestures.skip_empty;
            let invert_horizontal =
                self.config.settings.layout.scrolling.gestures.invert_horizontal;
            let cmd = if invert_horizontal {
                match dir {
                    Direction::Left => Some(layout::LayoutCommand::NextWorkspace(Some(skip_empty))),
                    Direction::Right => {
                        Some(layout::LayoutCommand::PrevWorkspace(Some(skip_empty)))
                    }
                    _ => None,
                }
            } else {
                match dir {
                    Direction::Left => Some(layout::LayoutCommand::PrevWorkspace(Some(skip_empty))),
                    Direction::Right => {
                        Some(layout::LayoutCommand::NextWorkspace(Some(skip_empty)))
                    }
                    _ => None,
                }
            };
            if let Some(cmd) = cmd {
                let space = workspace_switch_space.or_else(|| self.command_context_space());
                if let Some(space) = space {
                    let resp = self.layout_manager.layout_engine.handle_virtual_workspace_command(
                        &mut self.state.windows,
                        space,
                        &cmd,
                    );

                    if self.config.settings.gestures.haptics_enabled {
                        let _ = rini_macos::haptics::perform_haptic(
                            self.config.settings.gestures.haptic_pattern,
                        );
                    }

                    // Recurse to handle the new response (e.g. focus window on the new workspace)
                    self.handle_layout_response(resp, Some(space));
                    return;
                }
            }
        }

        let original_focus = focus_window;

        let focus_quiet = workspace_switch_space.map_or(Quiet::No, |_| Quiet::Yes);

        let handled_without_raise = if raise_windows.is_empty() && focus_window.is_none() {
            if matches!(
                self.workspace_switch_manager.workspace_switch_state,
                WorkspaceSwitchState::Active
            ) && !self.is_in_drag()
            {
                if let Some(wid) = self.window_id_under_cursor() {
                    // Avoid duplicate focus events for the already focused window.
                    if self.main_window() != Some(wid) {
                        focus_window = Some(wid);
                    }
                    false
                } else {
                    let skip_center_warp = workspace_switch_space
                        .map(|space| {
                            self.layout_manager
                                .layout_engine
                                .windows_in_active_workspace(&self.state.windows, space)
                                .is_empty()
                        })
                        .unwrap_or(false);
                    if skip_center_warp {
                        workspace_switch_space.is_some_and(|space| {
                            self.focus_desktop_if_active_workspace_empty(space)
                        })
                    } else {
                        let space = workspace_switch_space.or_else(|| self.command_context_space());
                        self.try_focus_or_warp_without_raise(space, &mut focus_window)
                    }
                }
            } else if let Some(space) = pending_refocus_space.take() {
                if let Some(wid) = self.last_focused_window_in_space(space) {
                    focus_window = Some(wid);
                    false
                } else if !self.is_in_drag() {
                    self.try_focus_or_warp_without_raise(Some(space), &mut focus_window)
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        if let Some(wid) = focus_window
            && let Some(state) = self.state.windows.window(wid)
            && let Some(wsid) = state.info.sys_id
        {
            let is_visible =
                self.state.windows.is_window_visible(wsid) || switch_target == Some(wid);
            let best_space = self.best_space_for_window_state(state);
            if !is_visible {
                focus_window = None;
                if let Some(space) = workspace_switch_space
                    && !self.is_in_drag()
                {
                    let _ = self.try_focus_or_warp_without_raise(Some(space), &mut focus_window);
                }
            } else if !best_space.is_some_and(|space| self.is_space_active(space)) {
                focus_window = None;
            }
        }

        if raise_windows.is_empty() && focus_window.is_none() {
            if handled_without_raise {
                self.workspace_switch_manager.mark_workspace_switch_inactive();
            }
            if handled_without_raise
                || matches!(
                    self.workspace_switch_manager.workspace_switch_state,
                    WorkspaceSwitchState::Inactive
                )
            {
                return;
            }
        }

        if let Some(space) = pending_refocus_space {
            // Preserve the pending refocus request if it was not consumed above.
            if matches!(self.refocus_manager.refocus_state, RefocusState::None) {
                self.refocus_manager.refocus_state = RefocusState::Pending(space);
            }
        }

        let mut app_handles = HashMap::default();
        for &wid in raise_windows.iter() {
            self.insert_app_handle_for_window(&mut app_handles, wid);
        }

        if let Some(wid) = original_focus {
            self.insert_app_handle_for_window(&mut app_handles, wid);
        }

        let mut raise_windows: Vec<WindowId> = raise_windows
            .into_iter()
            .filter(|wid| self.is_window_on_active_space(*wid))
            .collect();

        // Drop scrolled-away columns from the raise list entirely.
        //
        // A scrolling layout parks off-strip columns just past the screen edge,
        // leaving a 1pt sliver (macOS refuses to place a window entirely outside
        // every display). Raising those slivers is pointless — nothing of them is
        // visible — and it actively hurts: each one is an extra AX round-trip per
        // focus change, and any raise issued after the on-screen windows puts a
        // sliver in front of them.
        //
        // An earlier attempt SORTED parked windows to the front of the list instead,
        // on the theory that raising is last-wins so raising them first would leave
        // them behind. That worked for stacking but caused visible flicker, because
        // `wids.first()` is not just "the first raise": handle_raise_request treats
        // it as the PRIMARY window and uses it for make-key, the is_standard check
        // and the activation wait (app.rs). Sorting a parked, off-screen window into
        // that slot made macOS activate an invisible window and then raise the real
        // one immediately after.
        //
        // Not raising them at all avoids both problems and is strictly less work.
        // They keep whatever relative z-order they already had, which is invisible
        // by definition while they are parked, and they get raised normally the
        // moment they scroll back into view.
        //
        // Guarded: if EVERY window in the list is parked, keep the list as-is rather
        // than emptying it. An empty raise list is a different code path above (it
        // can skip the raise and the focus entirely), and the caller asked for these
        // windows for a reason — e.g. a workspace switch where the layout has not
        // been applied yet, so the frames still describe the old positions.
        if raise_windows.iter().any(|wid| !self.is_window_parked_offscreen(*wid)) {
            raise_windows.retain(|wid| !self.is_window_parked_offscreen(*wid));
        }

        // The strip goes up as one group. A focus move raises only the window it lands on, which leaves a
        // floating window in front of the columns beside it. See `model::z_group`. The regroup's order
        // (back to front, focused last) leads; whatever else the layout asked for follows.
        if let Some(target) = focus_window
            && let Some(space) = self.best_space_for_window_id(target)
        {
            let regroup = self.strip_group_to_lift(space, target);
            if !regroup.is_empty() {
                debug!(
                    focused = target.idx.get(),
                    windows = regroup.len(),
                    "a floating window was in front of the strip; lifting the whole strip over it"
                );
                for wid in &regroup {
                    self.insert_app_handle_for_window(&mut app_handles, *wid);
                }
                self.regroup_raised = regroup.clone();
                let rest: Vec<WindowId> =
                    raise_windows.into_iter().filter(|wid| !regroup.contains(wid)).collect();
                raise_windows = regroup;
                raise_windows.extend(rest);
            }
        }

        let focus_window = focus_window.filter(|wid| self.is_window_on_active_space(*wid));
        if let Some(space) = workspace_switch_space {
            self.layout_manager.layout_engine.commit_workspace_focus(
                &mut self.state.windows,
                space,
                focus_window,
            );
        }
        let mut windows_by_app_and_screen = HashMap::default();
        for &wid in &raise_windows {
            windows_by_app_and_screen
                .entry((wid.pid, self.best_space_for_window_id(wid)))
                .or_insert(vec![])
                .push(wid);
        }
        let focus_window_with_warp = focus_window.map(|wid| {
            let warp = if self.config.settings.mouse_follows_focus {
                if self.workspace_switch_manager.workspace_switch_state
                    == WorkspaceSwitchState::Active
                {
                    // During workspace switches, defer mouse warping until after layout completes.
                    self.workspace_switch_manager.pending_workspace_mouse_warp = Some(wid);
                    None
                } else {
                    self.window_center_on_known_screen(wid)
                }
            } else {
                None
            };
            (wid, warp)
        });

        self.dispatch_raise(raise_manager::Event::RaiseRequest(RaiseRequest {
            raise_windows: windows_by_app_and_screen.into_values().collect(),
            focus_window: focus_window_with_warp,
            app_handles,
            focus_quiet,
        }));
    }

    /// Which z-order group a window belongs to.
    fn stack_group_of(&self, window: WindowId) -> crate::model::z_group::StackGroup {
        if self.layout_manager.layout_engine.is_window_floating(window) {
            crate::model::z_group::StackGroup::Floating
        } else {
            crate::model::z_group::StackGroup::Strip
        }
    }

    /// Puts the strip back in front of the windows that are not on it, after focus lands on the strip.
    ///
    /// macOS raises the one window that was clicked, so clicking one half of a 50/50 pair lifts it over a
    /// floating window and leaves the other half behind it. The strip is one surface and has to be one
    /// group. Empty when the order already obeys the rule, which is the common case and costs nothing:
    /// putting it back costs an Accessibility raise per strip window in the workspace.
    ///
    /// Only the on-screen windows are judged and raised. Raising the whole strip was tried and measured:
    /// 19 windows across 10 apps take seconds (one activation wait per app, serialized), every raise
    /// echoes as a focus report, and echoes past the 400ms `RaiseEcho` window read as the user moving,
    /// so the strip scrolled to random parked windows and regrouped again, 13 rounds per press. A parked
    /// column's place cannot be seen; it is judged again by `regroup_after_layout` once a layout pass
    /// brings it on screen, since nothing else raises a column that scrolls back into view.
    fn strip_group_to_lift(&mut self, space: SpaceId, focused: WindowId) -> Vec<WindowId> {
        let depths = rini_macos::window_server::front_to_back_depths();
        let mut order: Vec<StackedWindow> = self
            .layout_manager
            .layout_engine
            .windows_in_active_workspace(&self.state.windows, space)
            .into_iter()
            .filter(|wid| !self.is_window_parked_offscreen(*wid))
            .filter_map(|wid| {
                let server_id = self.state.windows.window(wid)?.info.sys_id?;
                let depth = depths.get(&server_id.as_u32()).copied()?;
                Some(StackedWindow { window: wid, depth, group: self.stack_group_of(wid) })
            })
            .collect();
        order.sort_by_key(|stacked| stacked.depth);

        let to_raise = strip_group_to_lift_for(&order, focused, self.stack_group_of(focused));
        debug!(
            focused = focused.idx.get(),
            order = ?order.iter().map(|s| (s.window.idx.get(), s.group)).collect::<Vec<_>>(),
            lifting = to_raise.len(),
            "strip regroup judged"
        );
        to_raise
    }

    /// The regroup for a focus that has landed and a layout that has been applied: the strip windows now
    /// on screen are lifted over any floating window in front of one of them, the focused window last.
    ///
    /// Two cases end here. A click is raised by macOS, not by rini, and produces only a focus report,
    /// so nothing else in that path fixes the order. A focus move raises before the strip scrolls
    /// (`handle_layout_response`), so a column that was parked behind a floating window comes into view
    /// behind it and the floating window sits between it and the focused column; `animate_layout` has
    /// set every moved window's `frame_monotonic` by now, so the on-screen set is the post-layout one
    /// while the window server's order is still the old one. Skipped when every window it would raise
    /// was already raised by this event's earlier regroup: that raise is still in flight and the order
    /// it will produce is the one wanted.
    fn regroup_after_layout(&mut self) {
        let already = std::mem::take(&mut self.regroup_raised);
        let Some(focused) = self.layout_manager.layout_engine.focused_window() else {
            return;
        };
        // The layout's focus is stale when the window that really has focus is one the layout
        // does not hold: Zoom's meeting toolbar (not manageable) took focus when the user
        // switched to the call, the layout kept Kiro as focused, and the regroup lifted the strip
        // over the call and re-focused Kiro. Nothing to put right in that case: macOS ordered
        // the windows for the app the user chose.
        if self.main_window().is_some_and(|front| front != focused) {
            debug!(
                focused = focused.idx.get(),
                front = ?self.main_window().map(|w| w.idx.get()),
                "regroup skipped: focus is on a window outside the layout"
            );
            return;
        }
        let Some(space) = self.best_space_for_window_id(focused) else {
            return;
        };
        let to_raise = self.strip_group_to_lift(space, focused);
        if to_raise.is_empty() || to_raise.iter().all(|wid| already.contains(wid)) {
            return;
        }
        if let Some(request) = self.regroup_raise(&to_raise, focused) {
            debug!(
                focused = focused.idx.get(),
                windows = to_raise.len(),
                "a floating window was in front of the strip; lifting the whole strip over it"
            );
            self.dispatch_raise(request);
        }
    }

    /// One quiet raise of `to_raise` in order, focusing `focused` (which is expected to be last).
    fn regroup_raise(
        &mut self,
        to_raise: &[WindowId],
        focused: WindowId,
    ) -> Option<raise_manager::Event> {
        let mut app_handles = HashMap::default();
        let mut by_app: HashMap<(pid_t, Option<SpaceId>), Vec<WindowId>> = HashMap::default();
        for wid in to_raise {
            self.insert_app_handle_for_window(&mut app_handles, *wid);
            by_app.entry((wid.pid, self.best_space_for_window_id(*wid))).or_default().push(*wid);
        }
        if by_app.is_empty() {
            return None;
        }
        self.insert_app_handle_for_window(&mut app_handles, focused);
        self.regroup_raised = to_raise.to_vec();
        Some(raise_manager::Event::RaiseRequest(RaiseRequest {
            raise_windows: by_app.into_values().collect(),
            focus_window: Some((focused, None)),
            app_handles,
            // rini's own doing, so the activation it provokes must not be read as the user moving.
            focus_quiet: Quiet::Yes,
        }))
    }

    /// Sends a raise request, remembering the focus reports it is about to provoke.
    ///
    /// The single place raises leave the reactor, so nothing can issue one without its echo being recorded.
    /// See [`main_window::RaiseEcho`].
    fn dispatch_raise(&mut self, request: raise_manager::Event) {
        if let raise_manager::Event::RaiseRequest(details) = &request {
            let touched: Vec<WindowId> = details.raise_windows.iter().flatten().copied().collect();
            self.raise_echo.expect(
                touched.into_iter(),
                details.focus_window.map(|(wid, _)| wid),
                std::time::Instant::now(),
            );
        }
        if let Err(error) = self.communication_manager.raise_manager_tx.try_send(request) {
            warn!(%error, "failed to send raise request");
        }
    }

    fn collect_drag_swap_candidates(
        &self,
        wid: WindowId,
        space: SpaceId,
    ) -> Vec<(WindowId, CGRect)> {
        self.state
            .windows
            .iter_windows()
            .filter_map(|(other_wid, other_state)| {
                if other_wid == wid {
                    return None;
                }
                let other_space = self.best_space_for_window_state(other_state)?;
                if other_space != space
                    || !self.layout_manager.layout_engine.is_window_in_active_workspace(
                        &self.state.windows,
                        space,
                        other_wid,
                    )
                    || self.layout_manager.layout_engine.is_window_floating(other_wid)
                {
                    return None;
                }
                Some((other_wid, other_state.frame_monotonic))
            })
            .collect()
    }

    fn maybe_swap_on_drag(&mut self, wid: WindowId, new_frame: CGRect) {
        if !self.is_in_drag() {
            trace!(?wid, "Skipping swap: not in drag (mouse up received)");
            return;
        }

        // A floating window has no swap semantics: it is not in the tiling strip, so there
        // is nothing for it to trade places with. Returning before the fall-through at the
        // end of this function is the point — that path clears
        // `skip_layout_for_window`, and clearing it MID-GESTURE lets the next layout pass
        // reassert the window's stored frame while the user is still holding the mouse.
        //
        // Measured on System Settings: the reported `old_frame` rewound repeatedly during a
        // single drag (695,188 -> 832,167 -> 927,146, then back to 350,212), because rini
        // kept writing the stale stored position underneath the drag. The window ended up
        // wherever the last tug-of-war left it, which reads as snapping back part of the
        // way — roughly a third of the distance, in the reported case.
        if self.layout_manager.layout_engine.is_window_floating(wid) {
            trace!(
                ?wid,
                "Skipping swap: floating windows do not participate in strip swaps"
            );
            return;
        }

        let server_id = {
            let Some(window) = self.state.windows.window(wid) else {
                return;
            };
            window.info.sys_id
        };

        let Some(space) = self
            .get_active_drag_session()
            .and_then(|session| session.settled_space)
            .or_else(|| self.best_space_for_window(&new_frame, server_id))
        else {
            return;
        };

        let origin_space_hint = self
            .get_active_drag_session()
            .and_then(|session| session.origin_space)
            .or_else(|| {
                self.drag_manager
                    .origin_frame()
                    .and_then(|frame| self.best_space_for_window(&frame, server_id))
            });

        if let Some(origin_space) = origin_space_hint
            && origin_space != space
        {
            if let Some((pending_wid, pending_target)) = self.get_pending_drag_swap()
                && pending_wid == wid
            {
                trace!(
                    ?wid,
                    ?pending_target,
                    ?origin_space,
                    ?space,
                    "Clearing pending drag swap; dragged window entered new space"
                );
                self.drag_manager.drag_state = DragState::Inactive;
            }
            trace!(
                ?wid,
                ?origin_space,
                ?space,
                "Resetting drag swap tracking after space change"
            );
            self.drag_manager.drag_swap_manager.reset();
            return;
        }

        if !self.layout_manager.layout_engine.is_window_in_active_workspace(
            &self.state.windows,
            space,
            wid,
        ) {
            return;
        }

        let candidates = self.collect_drag_swap_candidates(wid, space);

        let previous_pending = self.get_pending_drag_swap();
        let new_candidate =
            self.drag_manager.drag_swap_manager.on_frame_change(wid, new_frame, &candidates);
        let active_target = self.drag_manager.drag_swap_manager.last_target();
        if let Some(target_wid) = active_target {
            if new_candidate.is_some() || previous_pending != Some((wid, target_wid)) {
                trace!(
                    ?wid,
                    ?target_wid,
                    "Detected swap candidate; deferring until MouseUp"
                );
            }

            if let Some(session) = self.take_active_drag_session() {
                self.drag_manager.drag_state =
                    DragState::PendingSwap { session, target: target_wid };
            } else {
                trace!(
                    ?wid,
                    ?target_wid,
                    "Skipping pending swap; no active drag session"
                );
                self.drag_manager.drag_state = DragState::Inactive;
                self.drag_manager.skip_layout_for_window = None;
                return;
            }

            self.drag_manager.skip_layout_for_window = Some(wid);
            return;
        }

        if let Some((pending_wid, pending_target)) = previous_pending
            && pending_wid == wid
        {
            trace!(
                ?wid,
                ?pending_target,
                "Clearing pending drag swap; overlap ended before MouseUp"
            );
            if let Some(session) = self.take_active_drag_session() {
                self.drag_manager.drag_state = DragState::Active { session };
            } else {
                self.drag_manager.drag_state = DragState::Inactive;
            }
        }

        if self.drag_manager.skip_layout_for_window == Some(wid) {
            self.drag_manager.skip_layout_for_window = None;
        }
        // wait for mouse::up before doing *anything*
    }

    pub(crate) fn window_id_under_cursor(&self) -> Option<WindowId> {
        self.tracked_window_under_cursor().map(|(_, wid)| wid)
    }

    fn window_server_id_under_cursor(&self) -> Option<WindowServerId> {
        window_server::window_under_cursor()
    }

    fn tracked_window_under_cursor(&self) -> Option<(WindowServerId, WindowId)> {
        let wsid = self.window_server_id_under_cursor()?;
        let wid = self.state.windows.tracked_window_id(wsid)?;
        Some((wsid, wid))
    }

    fn activation_from_unmanageable_window(&self, pid: pid_t) -> Option<WindowServerId> {
        let (wsid, wid) = self.tracked_window_under_cursor()?;
        let window = self.state.windows.window(wid)?;
        (wid.pid == pid && !window.matches_filter(WindowFilter::EffectivelyManageable))
            .then_some(wsid)
    }

    fn focus_untracked_window_under_cursor(&mut self) -> bool {
        let Some(wsid) = self.window_server_id_under_cursor() else {
            return false;
        };
        if self.state.windows.tracked_window_id(wsid).is_some() {
            return false;
        }

        let window_info = self
            .state
            .windows
            .get_window_server_info(wsid)
            .or_else(|| window_server::get_window(wsid));

        let Some(info) = window_info else { return false };
        window_server::make_key_window(info.pid, wsid).is_ok()
    }

    fn focus_desktop_if_active_workspace_empty(&mut self, space: SpaceId) -> bool {
        if !self.is_space_active(space)
            || !self
                .layout_manager
                .layout_engine
                .windows_in_active_workspace(&self.state.windows, space)
                .is_empty()
        {
            return false;
        }
        let Some(screen) = self.space_state.screen_by_space(space) else {
            return false;
        };
        if !window_server::focus_desktop_window(screen) {
            return false;
        }

        self.layout_manager.layout_engine.commit_workspace_focus(
            &mut self.state.windows,
            space,
            None,
        );
        true
    }

    fn last_focused_window_in_space(&self, space: SpaceId) -> Option<WindowId> {
        let active_workspace = self.layout_manager.layout_engine.active_workspace(space)?;
        let wid = self
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .last_focused_window(space, active_workspace)?;
        let window = self.state.windows.window(wid)?;

        if self.best_space_for_window_id(wid)? != space {
            return None;
        }
        if window
            .info
            .sys_id
            .is_some_and(|wsid| !self.state.windows.is_window_visible(wsid))
        {
            return None;
        }
        Some(wid)
    }

    /// The window a workspace switch focuses. The preferred window (the one the switch is for)
    /// only has to belong to the workspace: it is parked until the layout lands, so the window
    /// server may report it off screen. The fallbacks must be visible. See
    /// `choose_switch_focus`.
    fn visible_focus_candidate_in_active_workspace(
        &self,
        space: SpaceId,
        preferred: Option<WindowId>,
    ) -> Option<WindowId> {
        let in_space = |wid: WindowId| {
            self.state.windows.window(wid).is_some_and(|w| w.info.sys_id.is_some())
                && self.best_space_for_window_id(wid) == Some(space)
                && self.layout_manager.layout_engine.is_window_in_active_workspace(
                    &self.state.windows,
                    space,
                    wid,
                )
        };
        let is_visible_in_space = |wid: WindowId| {
            in_space(wid)
                && self
                    .state
                    .windows
                    .window(wid)
                    .and_then(|w| w.info.sys_id)
                    .is_some_and(|wsid| self.state.windows.is_window_visible(wsid))
        };
        let remembered =
            self.last_focused_window_in_space(space).filter(|wid| is_visible_in_space(*wid));
        let first_visible = self
            .layout_manager
            .layout_engine
            .windows_in_active_workspace(&self.state.windows, space)
            .into_iter()
            .find(|wid| is_visible_in_space(*wid));
        choose_switch_focus(preferred, preferred.is_some_and(in_space), remembered, first_visible)
    }

    fn request_refocus_if_hidden(&mut self, space: SpaceId, window_id: WindowId) {
        if self.window_in_non_active_workspace(space, window_id) {
            self.refocus_manager.refocus_state = RefocusState::Pending(space);
        }
    }

    fn window_in_non_active_workspace(&self, space: SpaceId, window_id: WindowId) -> bool {
        let Some(active_workspace) = self.layout_manager.layout_engine.active_workspace(space)
        else {
            return false;
        };
        self.layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(&self.state.windows, space, window_id)
            .is_some_and(|window_workspace| window_workspace != active_workspace)
    }

    fn prepare_refocus_after_layout_event(&mut self, event: &LayoutEvent) {
        match event {
            LayoutEvent::WindowAdded(space, wid) => {
                self.request_refocus_if_hidden(*space, *wid);
            }
            LayoutEvent::WindowsOnScreenUpdated(space, _, windows, _) => {
                let hidden_exists = windows.iter().any(|(wid, _, _, _, _, _, _, _, _)| {
                    self.window_in_non_active_workspace(*space, *wid)
                });
                if hidden_exists {
                    self.refocus_manager.refocus_state = RefocusState::Pending(*space);
                }
            }
            _ => {}
        }
    }

    #[instrument(skip(self))]
    fn clear_menu_state_for_pid(&mut self, pid: pid_t) {
        if matches!(self.menu_manager.menu_state, MenuState::Open(owner) if owner == pid) {
            debug!(pid, "Clearing menu-open state for deactivated app");
            self.menu_manager.menu_state = MenuState::Closed;
            self.update_focus_follows_mouse_state();
        }
    }

    fn clear_menu_state_for_non_owner(&mut self, pid: pid_t) {
        if matches!(self.menu_manager.menu_state, MenuState::Open(owner) if owner != pid) {
            debug!(pid, "Clearing stale menu-open state after app focus changed");
            self.menu_manager.menu_state = MenuState::Closed;
            self.update_focus_follows_mouse_state();
        }
    }

    fn set_focus_follows_mouse_enabled(&self, enabled: bool) {
        if let Some(event_tap_tx) = self.communication_manager.event_tap_tx.as_ref() {
            event_tap_tx.send(event_tap::Request::SetFocusFollowsMouseEnabled(enabled));
        }
    }

    fn update_focus_follows_mouse_state(&mut self) {
        let should_enable = self.config.settings.focus_follows_mouse
            && matches!(self.menu_manager.menu_state, MenuState::Closed)
            && !self.is_mission_control_active();
        self.set_focus_follows_mouse_enabled(should_enable);
    }



    fn refresh_windows_after_mission_control(&mut self) {
        debug!("Refreshing window state after Mission Control");
        // Skip when on a fullscreen space: kAXWindowsAttribute is space-filtered, so
        // apps omit their Desktop windows. check_for_new_windows sends an untracked
        // GetVisibleWindows whose response bypasses pending_mission_control_refresh,
        // causing those Desktop windows to be dropped from the layout, and other
        // windows in the layout to be incorrecctly resized.
        if !self.has_user_space_context() {
            return;
        }
        let active_windows = self.authoritative_active_space_windows();
        self.refresh_windows_after_mission_control_with_active_windows(active_windows);
    }

    fn refresh_windows_after_mission_control_with_active_windows(
        &mut self,
        active_windows: Vec<(WindowServerId, Option<SpaceId>)>,
    ) {
        if self.refreshes_blocked() {
            self.defer_visible_refresh(true);
            return;
        }

        // Mission Control can move windows between native spaces without emitting a
        // matching destroy/appear pair for the origin space. Reconcile the active
        // spaces from the same space-aware WS-id list used everywhere else so we do
        // not depend on the global CG on-screen window list during recovery.
        self.reconcile_authoritative_active_window_snapshot(active_windows, false);
        self.mission_control_manager.pending_mission_control_refresh.clear();
        self.force_refresh_all_windows();
        self.check_for_new_windows();
        self.update_layout_or_warn(false, false, None);
    }

    // Uses the same "pending refresh" path as Mission Control recovery so a bulk
    // visibility rediscovery can reconcile tracked windows without treating a
    // transient empty AX window list as authoritative removal.
    fn force_refresh_all_windows(&mut self) {
        self.request_visible_windows_for_apps(true);
    }

    fn has_user_space_context(&self) -> bool {
        self.raw_command_space().is_some_and(|space| !self.is_fullscreen_space(space))
    }

    fn request_close_window(&mut self, pid: pid_t, window_server_id: Option<WindowServerId>) {
        if let Some(app) = self.app_manager.apps.get(&pid) {
            if let Err(err) = app.handle.send(Request::CloseWindow(window_server_id)) {
                warn!(
                    pid,
                    ?window_server_id,
                    "Failed to send close window request: {}",
                    err
                );
            }
        }
    }

    pub(crate) fn main_window(&self) -> Option<WindowId> {
        self.main_window_tracker.main_window()
    }

    fn main_window_space(&self) -> Option<SpaceId> {
        // TODO: Optimize this with a cache or something.
        let wid = self.main_window()?;
        self.best_space_for_window_id(wid)
    }

    /// Window discovery is scoped to one application. It may restore that
    /// application's current focus after its windows have been inserted into
    /// the layout, but it must never replay another application's global main
    /// window. Requiring the command space also prevents a refresh racing an
    /// active-display change from restoring focus on the display being left.
    fn focused_window_for_discovery(&self, pid: pid_t) -> Option<(SpaceId, WindowId)> {
        let window = self.main_window().filter(|window| window.pid == pid)?;
        let space = self.main_window_space()?;
        (self.workspace_command_space() == Some(space)).then_some((space, window))
    }

    fn raw_command_space(&self) -> Option<SpaceId> {
        self.space_state.command_space
    }

    fn active_display_space(&self) -> Option<SpaceId> {
        self.raw_command_space()
            .filter(|space| {
                self.space_state.active_spaces.contains(space)
                    && self.space_state.screens.iter().any(|screen| screen.space == Some(*space))
            })
            .or_else(|| {
                self.space_state
                    .screens
                    .iter()
                    .filter_map(|screen| screen.space)
                    .find(|space| self.space_state.active_spaces.contains(space))
            })
    }

    fn workspace_command_space(&self) -> Option<SpaceId> {
        self.active_display_space().filter(|space| self.is_space_active(*space))
    }

    fn command_context_space(&self) -> Option<SpaceId> {
        self.workspace_command_space().or_else(|| {
            self.layout_manager
                .layout_engine
                .focused_window()
                .and_then(|wid| {
                    self.assigned_space_for_window_id(wid)
                        .or_else(|| self.best_space_for_window_id(wid))
                })
                .filter(|space| self.is_space_active(*space))
                .or_else(|| self.main_window_space().filter(|space| self.is_space_active(*space)))
        })
    }

    fn screen_for_point(&self, point: CGPoint) -> Option<&ScreenInfo> {
        self.space_state.screens.iter().find(|screen| screen.frame.contains(point))
    }

    fn current_screen_center(&self) -> Option<CGPoint> {
        if let Some(space) = self.raw_command_space() {
            if let Some(screen) = self.space_state.screen_by_space(space) {
                return Some(screen.frame.mid());
            }
        }

        self.space_state.screens.first().map(|screen| screen.frame.mid())
    }

    fn screen_for_direction_from_point(
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

        for screen in &self.space_state.screens {
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

    fn screen_for_selector(
        &self,
        selector: &DisplaySelector,
        origin_override: Option<CGPoint>,
    ) -> Option<&ScreenInfo> {
        match selector {
            DisplaySelector::Direction(direction) => {
                let origin = origin_override.or_else(|| self.current_screen_center())?;
                self.screen_for_direction_from_point(origin, *direction)
            }
            DisplaySelector::Index(index) => self.screens_in_physical_order().get(*index).copied(),
            DisplaySelector::Uuid(uuid) => {
                self.space_state.screens.iter().find(|screen| screen.display_uuid == *uuid)
            }
        }
    }

    fn screens_in_physical_order(&self) -> Vec<&ScreenInfo> {
        let mut screens: Vec<&ScreenInfo> = self.space_state.screens.iter().collect();
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

    fn store_current_floating_positions(&mut self, space: SpaceId) {
        let floating_windows_in_workspace = self
            .layout_manager
            .layout_engine
            .windows_in_active_workspace(&self.state.windows, space)
            .into_iter()
            .filter(|&wid| self.layout_manager.layout_engine.is_window_floating(wid))
            .filter_map(|wid| {
                self.state
                    .windows
                    .window(wid)
                    .map(|window_state| (wid, window_state.frame_monotonic))
            })
            .collect::<Vec<_>>();

        if !floating_windows_in_workspace.is_empty() {
            self.layout_manager
                .layout_engine
                .store_floating_window_positions(space, &floating_windows_in_workspace);
        }
    }

    pub(crate) fn update_layout_or_warn(
        &mut self,
        is_resize: bool,
        is_workspace_switch: bool,
        space_scope: Option<SpaceId>,
    ) -> bool {
        self.update_layout_or_warn_with(
            is_resize,
            is_workspace_switch,
            space_scope,
            "Layout update failed",
        )
    }

    pub(crate) fn update_layout_or_warn_with(
        &mut self,
        is_resize: bool,
        is_workspace_switch: bool,
        space_scope: Option<SpaceId>,
        context: &'static str,
    ) -> bool {
        let changed =
            LayoutManager::update_layout(self, is_resize, is_workspace_switch, space_scope)
                .unwrap_or_else(|e| {
                    warn!(error = ?e, "{}", context);
                    false
                });
        if changed {
            // Re-observe affinity before saving, so the file records the arrangement that
            // was just applied. This is the path that catches a window dragged between
            // displays, a new window opened, and any reshuffle of the strip — none of which
            // produce a display snapshot, so none of which used to update affinity at all.
            self.sync_display_affinity_from_live_layout();
            self.autosave_layout();
        }
        changed
    }

    /// Persist the layout after a change, at most once every AUTOSAVE_INTERVAL.
    ///
    /// Saving used to be entirely manual — the menu bar item and `rini-cli save-layout`
    /// were the only writers — so a restart or redeploy lost every window's
    /// workspace, size and strip position, and windows fell back to the default
    /// column width. `--restore` existed but had nothing to read.
    ///
    /// Debounced because update_layout runs on every focus change, resize and
    /// workspace switch, and serialising the whole engine plus an fsync on each one
    /// would be wasteful. The interval is short enough that at most a few seconds of
    /// arrangement is at risk, and the save is also forced on shutdown and on
    /// display reconfiguration, which are the moments that actually matter.
    fn autosave_layout(&mut self) {
        const AUTOSAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

        let now = std::time::Instant::now();
        if let Some(last) = self.last_autosave {
            if now.duration_since(last) < AUTOSAVE_INTERVAL {
                self.autosave_pending = true;
                return;
            }
        }
        self.save_layout_now();
    }

    /// Write the layout file immediately, bypassing the debounce.
    pub(crate) fn save_layout_now(&mut self) {
        let Some(path) = self.autosave_path.clone() else {
            return;
        };
        let active_space = self.workspace_command_space();
        // Recorded here rather than by hooks on every move and resize: the memory is a projection of
        // where the windows are now, and this is the moment it gets written down.
        let connected: Vec<String> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.display_uuid_owned())
            .collect();
        self.layout_manager
            .layout_engine
            .remember_launch_slots(&self.state.windows, &connected);
        // autosave_current_layout, NOT save_current_layout: the latter also
        // normalizes floating-versus-tiled ownership and rewrites stored floating
        // frames, which are mutations to live state. Running them on every layout
        // change broke un-fullscreening a floating window, because the frame it
        // should return to had already been overwritten.
        if let Err(e) = self.layout_manager.layout_engine.autosave_current_layout(
            path.clone(),
            &self.state.windows,
            active_space,
        ) {
            // Not fatal: a failed autosave costs the last few seconds of
            // arrangement, not correctness of the running layout.
            debug!(error = ?e, path = %path.display(), "Autosave failed");
            return;
        }
        self.last_autosave = Some(std::time::Instant::now());
        self.autosave_pending = false;
        trace!(path = %path.display(), "Autosaved layout");
    }
}

/// The window an activation-driven switch focuses: the window the user was last in when it sits on
/// the same workspace as macOS's pick, else the pick.
pub(crate) fn switch_focus_within_workspace<W: Copy + PartialEq>(
    picked: W,
    remembered_on_same_workspace: Option<W>,
) -> W {
    remembered_on_same_workspace.unwrap_or(picked)
}

/// Which window a workspace switch focuses: the window the switch is for whenever it belongs to the
/// workspace (visible or not: it is parked until the layout lands), else the workspace's remembered
/// window if visible, else the first visible one.
pub(crate) fn choose_switch_focus<W: Copy>(
    preferred: Option<W>,
    preferred_in_workspace: bool,
    remembered_visible: Option<W>,
    first_visible: Option<W>,
) -> Option<W> {
    preferred.filter(|_| preferred_in_workspace).or(remembered_visible).or(first_visible)
}

/// One on-screen window of the active workspace as the window server stacks it, for the strip regroup.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct StackedWindow {
    pub(crate) window: WindowId,
    /// Front-to-back position, 0 being frontmost.
    pub(crate) depth: usize,
    pub(crate) group: crate::model::z_group::StackGroup,
}

/// The windows to raise so the strip is one group in front of the floating windows, in raise order:
/// back to front, the focused window last. Empty when a floating window takes focus (macOS already
/// put it in front, which is the rule) or when the order already obeys the rule.
///
/// `order` is front to back, as `front_to_back_depths` reports it. The focused window goes last
/// whatever its depth: a raise sequence ends with the window that has to be frontmost, and after a
/// keyboard move the target has not been raised yet.
pub(crate) fn strip_group_to_lift_for(
    order: &[StackedWindow],
    focused: WindowId,
    focused_group: crate::model::z_group::StackGroup,
) -> Vec<WindowId> {
    use crate::model::z_group::StackGroup;

    if focused_group == StackGroup::Floating {
        return Vec::new();
    }
    let groups: Vec<(WindowId, StackGroup)> = order.iter().map(|s| (s.window, s.group)).collect();
    let back_to_front = crate::model::z_group::strip_regroup(&groups);
    let mut raise: Vec<WindowId> =
        back_to_front.iter().copied().filter(|wid| *wid != focused).collect();
    if back_to_front.contains(&focused) {
        raise.push(focused);
    }
    raise
}
