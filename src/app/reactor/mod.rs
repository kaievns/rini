//! The Reactor's job is to maintain coherence between the system and model state.
//!
//! It takes events from the rest of the system and builds a coherent picture of
//! what is going on. It shares this with the layout actor, and reacts to layout
//! changes by sending requests out to the other actors in the system.

mod activation;
mod active_spaces;
mod animation;
mod app_rules;
mod autosave;
mod bar;
mod command_space;
mod commands;
mod decorations;
mod diagnostics;
mod dispatch;
mod display_homes;
mod drag_swap;
mod events;
mod focus_heuristics;
mod layout_pass;
mod managers;
mod observations;
mod present;
mod quarantine;
mod query;
mod regroup;
mod space_affinity;
mod space_resolution;
mod space_snapshot;
pub mod state;
mod strip_motion;
mod switcher;
mod window_refresh;

#[cfg(test)]
mod testing;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::thread;

use events::{EventOutcome, WindowFrameWriteRequest, focus as focus_service};
use objc2_core_foundation::CGRect;
use rini_core::ids::{SpaceId, WindowId, WindowServerId, pid_t};
use rini_geometry::space::{FromCg, ScreenRect};
use rini_runloop::executor::Executor;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use space_affinity::SpaceAffinity;
use switcher::LiveSwitch;
use tracing::{debug, instrument, trace, warn};

use crate::app::channels;
use crate::app::config::Config;
use crate::app::reactor::state::RiniState;
use crate::app::system_reads::SystemReads;
pub use crate::displays::domain::screen::ScreenInfo;
use crate::displays::domain::space_activation::SpaceActivationPolicy;
use crate::displays::domain::topology::{ForwardedSpaceState, SpaceEventKind};
use crate::input::platform::input_tap as event_tap;
use crate::transitions::domain::request::Cause;
use crate::windows::domain::focus::{FocusEvent, MainWindowTracker};
use crate::windows::domain::info::{AppInfo, WindowInfo, WindowServerInfo};
use crate::windows::domain::raise::RaiseManager;
use crate::windows::domain::request::{AppThreadHandle, Quiet, Request};
use crate::windows::domain::transaction::{TransactionId, WindowTxStore};
use crate::windows::platform::mouse::MouseState;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::{Direction, LayoutEngine, LayoutEvent};

pub type Sender = channels::Sender<Event>;
type Receiver = channels::Receiver<Event>;
pub use query::ReactorQueryHandle;
pub use rini_ipc::protocol::Command;

pub(crate) use crate::app::reactor::state::AppState;
pub use crate::app::reactor::state::{
    DisplaySelector, DragSession, DragState, MenuState, MissionControlState, ReactorCommand,
    RefocusState, StaleCleanupState, WorkspaceSwitchOrigin, WorkspaceSwitchState,
};
pub(crate) use crate::windows::domain::state::WindowFilter;
pub use crate::windows::domain::transaction::Requested;

#[derive(Clone)]
pub struct ReactorHandle {
    sender: Sender,
    queries: ReactorQueryHandle,
}

impl ReactorHandle {
    pub fn new(sender: Sender, queries: ReactorQueryHandle) -> Self { Self { sender, queries } }

    pub fn sender(&self) -> Sender { self.sender.clone() }

    #[allow(clippy::result_large_err, reason = "SendError carries the event")]
    pub fn try_send(
        &self,
        event: Event,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<(tracing::Span, Event)>> {
        self.sender.try_send(event)
    }
}

impl std::ops::Deref for ReactorHandle {
    type Target = ReactorQueryHandle;

    fn deref(&self) -> &Self::Target { &self.queries }
}

#[derive(Debug, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum Event {
    /// Place these windows at these frames now, with no animation.
    ///
    /// Sent by the flight engine once its flight is far enough along that the real
    /// windows are safely hidden behind it. Applying them any earlier let the windows visibly jump
    /// into place BEFORE the overlay appeared, so a switch read as a flicker followed by a slide.
    ApplyOverlayFrames(Vec<(WindowId, CGRect)>),
    SpaceStateChanged(ForwardedSpaceState),
    ActiveDisplayChanged {
        menu_bar_space: Option<SpaceId>,
        command_space: Option<SpaceId>,
    },
    /// An application was launched. This event is also sent for every running
    /// application on startup.
    ///
    /// Both WindowInfo (accessibility) and WindowServerInfo are collected for
    /// any already-open windows when the launch event is sent.
    ///
    /// NOT ordered against the Space events, so it can arrive naming a space rini has just switched
    /// off, and the same is true of window creation. That is a property of two independent macOS
    /// streams and cannot be fixed by ordering them; what handles it is that a discovered window's
    /// space is RESOLVED rather than taken from the event —
    /// `space_resolution::discovery` prefers a space that is actually showing over one inherited
    /// from a stale report. If a window ever lands on the space you just left, that rule is where to
    /// look, not here.
    ApplicationLaunched {
        pid: pid_t,
        info: AppInfo,
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
    WindowServerDestroyed(rini_core::ids::WindowServerId, SpaceId, SpaceEventKind),
    WindowServerAppeared(rini_core::ids::WindowServerId, SpaceId, SpaceEventKind),
    SpaceCreated(SpaceId),
    SpaceDestroyed(SpaceId),
    WindowMinimized(WindowId),
    WindowDeminiaturized(WindowId),
    WindowFrameChanged(
        WindowId,
        CGRect,
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
    /// Can interleave wrongly with the `MouseState` carried on app-actor frame events: this arrives
    /// on the input thread while those arrive per application, so a frame report generated while the
    /// button was down can be handled after the release that was meant to unsuppress it. Tracked in
    /// `roadmap.md` under known bugs; acting on it needs a measured case, because the visible
    /// symptom is a layout pass that does not happen rather than one that goes wrong.
    MouseUp,
    /// The left button went down. Straight from the input thread, so it is handled before any frame
    /// report the app sends in answer; see [`Event::left_press_sink`]. A double-click on a column's
    /// title bar is the evidence that the resize after it is the app zooming the window.
    LeftMouseDown(crate::input::event::LeftPress),
    /// A switcher session opened, moved, committed or was cancelled.
    Switch(crate::input::domain::switch_session::Signal),
    /// A row of the switcher popup was clicked: select that window and commit.
    SwitchPicked(WindowId),
    /// Something on a bar was clicked.
    BarAction(crate::bar::domain::model::Action),
    /// Sent by the event tap only when the cursor enters a different window.
    /// Window resolution and transition deduplication stay on the input
    /// thread; the reactor only applies the model-dependent focus/raise work.
    MouseMoved(WindowServerId),
    /// Forwarded by the spaces actor after wake has been observed.
    ///
    /// The spaces actor is the authority for sleep/lock/display lifecycle.
    /// The reactor uses this only to lift the refresh quarantine and resubscribe
    /// WindowServer notifications once the topology authority says wake
    /// processing has advanced.
    SystemWoke,
    /// The system clock was set, or the time zone changed.
    ClockChanged,
    SystemWillSleep,
    SessionDidResignActive,
    SessionDidBecomeActive,

    DisplayChurnBegin,

    MissionControlNativeEntered,
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

    /// The raise manager has nothing running or waiting. See `RaiseEcho::idle`.
    RaisesIdle {
        requests_seen: u64,
    },

    /// The applications asking for attention changed.
    AttentionChanged(crate::windows::domain::attention::Attention),

    Query(query::QueryRequest),

    Command(Command),

    RegisterWmSender(crate::app::hotkeys::Sender),

    ConfigUpdated(Box<Config>),

    /// The config file was read, and whether it could be used. While it cannot, every bar is marked.
    ConfigFileRead {
        usable: bool,
    },
}

/// The displays context's events, one for one; the topology snapshot arrives as `SpaceStateChanged`.
impl From<crate::displays::event::Event> for Event {
    fn from(event: crate::displays::event::Event) -> Self {
        use crate::displays::event::Event as D;
        match event {
            D::SpaceStateUpdated(state, _converter) => Event::SpaceStateChanged(*state),
            D::ActiveDisplayChanged { menu_bar_space, command_space } => {
                Event::ActiveDisplayChanged { menu_bar_space, command_space }
            }
            D::SpaceCreated(space) => Event::SpaceCreated(space),
            D::SpaceDestroyed(space) => Event::SpaceDestroyed(space),
            D::WindowServerAppeared(wsid, space, kind) => {
                Event::WindowServerAppeared(wsid, space, kind)
            }
            D::WindowServerDestroyed(wsid, space, kind) => {
                Event::WindowServerDestroyed(wsid, space, kind)
            }
            D::SystemWillSleep => Event::SystemWillSleep,
            D::SystemWoke => Event::SystemWoke,
            D::SessionDidResignActive => Event::SessionDidResignActive,
            D::SessionDidBecomeActive => Event::SessionDidBecomeActive,
            D::DisplayChurnBegin => Event::DisplayChurnBegin,
            D::MissionControlEntered => Event::MissionControlNativeEntered,
            D::MissionControlExited => Event::MissionControlNativeExited,
            D::WindowServerFocusChanged(window, space) => {
                Event::WindowServerFocusChanged(window, space)
            }
        }
    }
}

/// The windows context's events, one for one. The per-app actor sends
/// `crate::windows::event::Event` into the reactor's channel through this.
impl From<crate::windows::event::Event> for Event {
    fn from(event: crate::windows::event::Event) -> Self {
        use crate::windows::event::Event as W;
        match event {
            W::ApplicationLaunched {
                pid,
                info,
                handle,
                is_frontmost,
                main_window,
                visible_windows,
                window_server_info,
            } => Event::ApplicationLaunched {
                pid,
                info,
                handle,
                is_frontmost,
                main_window,
                visible_windows,
                window_server_info,
            },
            W::ApplicationThreadTerminated(pid) => Event::ApplicationThreadTerminated(pid),
            W::ApplicationActivated(pid, quiet) => Event::ApplicationActivated(pid, quiet),
            W::ApplicationDeactivated(pid) => Event::ApplicationDeactivated(pid),
            W::ApplicationMainWindowChanged(pid, window, quiet) => {
                Event::ApplicationMainWindowChanged(pid, window, quiet)
            }
            W::WindowsDiscovered { pid, new, known_visible } => {
                Event::WindowsDiscovered { pid, new, known_visible }
            }
            W::WindowCreated(wid, info, server_info, mouse) => {
                Event::WindowCreated(wid, info, server_info, mouse)
            }
            W::WindowDestroyed(wid) => Event::WindowDestroyed(wid),
            W::WindowMinimized(wid) => Event::WindowMinimized(wid),
            W::WindowDeminiaturized(wid) => Event::WindowDeminiaturized(wid),
            W::WindowFrameChanged(wid, frame, txid, requested, mouse) => {
                Event::WindowFrameChanged(wid, frame, txid, requested, mouse)
            }
            W::WindowTitleChanged(wid, title) => Event::WindowTitleChanged(wid, title),
            W::MenuOpened(pid) => Event::MenuOpened(pid),
            W::MenuClosed(pid) => Event::MenuClosed(pid),
            W::RaiseCompleted { window_id, sequence_id } => {
                Event::RaiseCompleted { window_id, sequence_id }
            }
            W::RaiseTimeout { sequence_id } => Event::RaiseTimeout { sequence_id },
            W::RaisesIdle { requests_seen } => Event::RaisesIdle { requests_seen },
            W::AttentionChanged(attention) => Event::AttentionChanged(attention),
        }
    }
}

impl Event {
    /// What the input tap's left presses are wired to: the reactor's own channel, not the main
    /// thread. The tap holds each press until this returns, and the app it lands on answers from its
    /// own thread, so the press is ahead of the answer on this channel.
    pub fn left_press_sink(events: Sender) -> crate::input::event::OnLeftPress {
        Box::new(move |press| events.send(Event::LeftMouseDown(press)))
    }

    /// What the event was, as a flight's journal names its press: the variant, and for a layout or
    /// reactor command the command, as the IPC names it.
    fn what(&self) -> &'static str {
        match self {
            Event::Command(Command::Layout(command)) => command.into(),
            Event::Command(Command::Reactor(command)) => command.into(),
            event => event.into(),
        }
    }

    /// The focus edge this event carries, if any. See [`FocusEvent`].
    fn focus_event(&self) -> Option<FocusEvent> {
        Some(match self {
            &Event::ApplicationLaunched {
                pid, is_frontmost, main_window, ..
            } => FocusEvent::ApplicationLaunched { pid, is_frontmost, main_window },
            &Event::ApplicationThreadTerminated(pid) => {
                FocusEvent::ApplicationThreadTerminated(pid)
            }
            &Event::WindowDestroyed(wid) => FocusEvent::WindowDestroyed(wid),
            &Event::ApplicationActivated(pid, quiet) => {
                FocusEvent::ApplicationActivated(pid, quiet)
            }
            &Event::ApplicationDeactivated(pid) => FocusEvent::ApplicationDeactivated(pid),
            &Event::ApplicationGloballyActivated(pid) => {
                FocusEvent::ApplicationGloballyActivated(pid)
            }
            &Event::ApplicationGloballyDeactivated(pid) => {
                FocusEvent::ApplicationGloballyDeactivated(pid)
            }
            &Event::ApplicationMainWindowChanged(pid, wid, quiet) => {
                FocusEvent::ApplicationMainWindowChanged(pid, wid, quiet)
            }
            &Event::WindowServerFocusChanged(wid, _) => FocusEvent::WindowServerFocusChanged(wid),
            Event::ApplyOverlayFrames(_)
            | Event::SpaceStateChanged(_)
            | Event::ActiveDisplayChanged { .. }
            | Event::ApplicationTerminated(_)
            | Event::WindowsDiscovered { .. }
            | Event::WindowCreated(..)
            | Event::WindowServerDestroyed(..)
            | Event::WindowServerAppeared(..)
            | Event::SpaceCreated(_)
            | Event::SpaceDestroyed(_)
            | Event::WindowMinimized(_)
            | Event::WindowDeminiaturized(_)
            | Event::WindowFrameChanged(..)
            | Event::WindowTitleChanged(..)
            | Event::MenuOpened(_)
            | Event::MenuClosed(_)
            | Event::MouseUp
            | Event::LeftMouseDown(_)
            | Event::Switch(_)
            | Event::SwitchPicked(_)
            | Event::BarAction(_)
            | Event::MouseMoved(_)
            | Event::SystemWoke
            | Event::ClockChanged
            | Event::SystemWillSleep
            | Event::SessionDidResignActive
            | Event::SessionDidBecomeActive
            | Event::DisplayChurnBegin
            | Event::MissionControlNativeEntered
            | Event::MissionControlNativeExited
            | Event::RaiseCompleted { .. }
            | Event::RaiseTimeout { .. }
            | Event::RaisesIdle { .. }
            | Event::AttentionChanged(_)
            | Event::Query(_)
            | Event::Command(_)
            | Event::RegisterWmSender(_)
            | Event::ConfigUpdated(_)
            | Event::ConfigFileRead { .. } => return None,
        })
    }
}
pub struct Reactor {
    pub config: Config,
    /// The switcher session, while one is open.
    live_switch: Option<LiveSwitch>,
    /// Where the popup is drawn. `None` until the main thread registers itself, and absent entirely in
    /// tests — the switch works without it, which is what keeps the popup cosmetic.
    switcher_tx: Option<crate::switcher::platform::actor::Sender>,
    bar: bar::BarFeed,
    /// The applications asking for attention, whose windows are drawn urgent.
    attention: crate::windows::domain::attention::Attention,
    rest: decorations::RestFeed,
    apps: HashMap<pid_t, AppState>,
    layout_engine: LayoutEngine,
    pub(crate) state: RiniState,
    space_state: ForwardedSpaceState,
    /// Where each WORKSPACE's strip started on screen the last time a layout pass looked
    /// (`LayoutEngine::strip_origin`).
    ///
    /// The animation path needs the DISTANCE the strip is moving. Reading that off the windows is
    /// unreliable: macOS clamps the ones it will not place off screen, and the layout is recomputed several
    /// times per keystroke, so the same press produced five different answers. The strip's origin is the
    /// layout's own description of the movement.
    ///
    /// Keyed by workspace, not by space, because every workspace has its own strip. Keyed by space alone,
    /// the first pass after a workspace switch compared the new workspace's offset against the old one's
    /// and animated the difference as a horizontal pan that no window had made.
    last_strip_origin: HashMap<(SpaceId, crate::workspaces::VirtualWorkspaceId), f64>,
    /// Where each WORKSPACE's strip had its windows the last time a layout pass looked, parked ones
    /// at their strip frames: where a pan starts a parked window whose place on the strip the pass
    /// changed (`strip_move::start_where_standing`). Taken and claimed with `last_strip_origin`.
    last_strip_layout:
        HashMap<(SpaceId, crate::workspaces::VirtualWorkspaceId), Vec<(WindowId, CGRect)>>,
    /// The window the current command moved along its strip, and which way, for the passes that
    /// command asked for: they draw the two columns crossing. See `animate_layout`.
    moved_along_strip: Option<(WindowId, Direction)>,
    space_activation_policy: SpaceActivationPolicy,
    main_window_tracker: MainWindowTracker,
    /// The focus reports rini's own raises are about to produce, so they are not mistaken for the user
    /// moving. See [`crate::windows::domain::focus::RaiseEcho`].
    raise_echo: crate::windows::domain::focus::RaiseEcho,
    live_spaces: space_affinity::LiveSpaces,
    /// Windows a layout pass has asked a picture of. See `animate_layout`.
    pictured_windows: HashSet<WindowId>,
    /// The layout's focus as the event being handled began: the window still drawn focused on screen
    /// when a pass the event asks for flies, which the flight engine draws focused until focus hands
    /// over. A focus report reaches the reactor before the pass it causes.
    focused_as_event_began: Option<WindowId>,
    /// The event being handled, and when the reactor took it: the press every flight it asks for
    /// carries, where that flight's journal starts. Every request goes to the flight engine while an
    /// event is handled.
    cause: Cause,
    /// The strip windows the current event's regroup already raised, so the post-layout judgment does
    /// not raise them a second time. See `regroup_after_layout`.
    regroup_raised: Vec<WindowId>,
    drag_manager: managers::DragManager,
    workspace_switch_manager: managers::WorkspaceSwitchManager,
    communication_manager: managers::CommunicationManager,
    /// The workspace each space's last layout pass drew, where its next switch slides from.
    drawn_workspace: HashMap<SpaceId, crate::workspaces::VirtualWorkspaceId>,
    /// The window-server ids rini last asked to be told about, sorted.
    last_sls_notification_ids: Vec<u32>,
    transactions: WindowTxStore,
    menu_state: MenuState,
    mission_control_manager: managers::MissionControlManager,
    refocus_manager: managers::RefocusManager,
    refresh_quarantine_manager: managers::RefreshQuarantineManager,
    /// The space snapshot that arrived while Mission Control was open, applied once it closes.
    pending_space_change: Option<ForwardedSpaceState>,
    active_spaces: HashSet<SpaceId>,
    /// When the layout file was last written, for debouncing autosaves.
    last_autosave: Option<std::time::Instant>,
    /// A layout change arrived inside the debounce window and has not been written
    /// yet. Flushed on shutdown and on display reconfiguration.
    autosave_pending: bool,
    /// Where autosave writes. `None` disables it.
    ///
    /// A field rather than a call to `config::restore_file()` at the write site because
    /// autosave fires from `update_layout`, which almost every test drives.
    /// Resolving the real path there meant the suite overwrote the user's own
    /// ~/.rini/layout.ron with test fixtures. `Reactor::new_for_test` sets it to `None`.
    autosave_path: Option<PathBuf>,
    reads: SystemReads,
}

impl Reactor {
    #[allow(clippy::too_many_arguments, reason = "P4.1")]
    pub fn spawn(
        config: Config,
        layout_engine: LayoutEngine,
        display_memory: DisplayMemory,
        event_tap_tx: event_tap::Sender,
        cursor_warp_tx: Option<crate::displays::platform::cursor_warp::Sender>,
        transitions_tx: Option<crate::transitions::platform::engine::Sender>,
        pictures: Option<crate::pictures::platform::service::Pictures>,
        switcher_tx: Option<crate::switcher::platform::actor::Sender>,
        bar_tx: Option<crate::bar::platform::actor::Sender>,
        rest: Option<(
            crate::decorations::platform::rest::Sender,
            crate::displays::event::Following,
        )>,
        window_tx_store: WindowTxStore,
    ) -> ReactorHandle {
        let (events_tx, events) = channels::channel();
        let events_tx_clone = events_tx.clone();
        let mut reactor = Reactor::new(config, layout_engine, display_memory, window_tx_store);
        reactor.communication_manager.event_tap_tx = Some(event_tap_tx);
        reactor.communication_manager.cursor_warp_tx = cursor_warp_tx;
        reactor.connect_transitions(transitions_tx);
        reactor.communication_manager.pictures = pictures;
        reactor.switcher_tx = switcher_tx;
        reactor.bar.tx = bar_tx;
        if let Some((tx, following)) = rest {
            reactor.rest.tx = Some(tx);
            reactor.rest.following = following;
        }
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
        display_memory: DisplayMemory,
        window_tx_store: WindowTxStore,
    ) -> Reactor {
        Self::with_reads(
            config,
            layout_engine,
            display_memory,
            window_tx_store,
            SystemReads::live(),
        )
    }

    fn with_reads(
        config: Config,
        layout_engine: LayoutEngine,
        display_memory: DisplayMemory,
        window_tx_store: WindowTxStore,
        reads: SystemReads,
    ) -> Reactor {
        // Apps that are no longer running were already dropped from the restored state, at load
        // rather than here: `discard_unmatchable_startup_candidates` refuses a candidate whose
        // window server id is gone AND whose bundle is not running.
        let (raise_manager_tx, _rx) = channels::channel();

        Reactor {
            live_switch: None,
            switcher_tx: None,
            bar: bar::BarFeed::default(),
            attention: Default::default(),
            rest: decorations::RestFeed::default(),
            config: config.clone(),
            apps: HashMap::default(),
            layout_engine,
            state: RiniState {
                display_memory,
                ..Default::default()
            },
            space_state: ForwardedSpaceState::default(),
            last_strip_origin: HashMap::default(),
            last_strip_layout: HashMap::default(),
            moved_along_strip: None,
            space_activation_policy: SpaceActivationPolicy::new(),
            main_window_tracker: MainWindowTracker::default(),
            raise_echo: crate::windows::domain::focus::RaiseEcho::default(),
            live_spaces: space_affinity::LiveSpaces::default(),
            pictured_windows: HashSet::default(),
            focused_as_event_began: None,
            cause: Cause {
                at: std::time::Instant::now(),
                what: "start",
            },
            regroup_raised: Vec::new(),
            drag_manager: managers::DragManager {
                drag_state: DragState::Inactive,
                drag_swap: crate::input::domain::drag_swap::DragSwap::new(
                    config.settings.window_snapping,
                ),
                skip_layout_for_window: None,
                title_bar_click: None,
            },
            workspace_switch_manager: managers::WorkspaceSwitchManager {
                workspace_switch_state: WorkspaceSwitchState::Inactive,
                workspace_switch_generation: 0,
                active_workspace_switch: None,
                pending_workspace_switch_origin: None,
                pending_workspace_mouse_warp: None,
            },
            drawn_workspace: HashMap::default(),
            communication_manager: managers::CommunicationManager {
                event_tap_tx: None,
                cursor_warp_tx: None,
                transitions_tx: None,
                pictures: None,
                raise_manager_tx,
                wm_sender: None,
                events_tx: None,
            },
            last_sls_notification_ids: Vec::new(),
            transactions: window_tx_store,
            menu_state: MenuState::Closed,
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
            pending_space_change: None,
            active_spaces: HashSet::default(),
            last_autosave: None,
            autosave_pending: false,
            autosave_path: Some(rini_core::paths::restore_file()),
            reads,
        }
    }

    /// The read-only space-resolution view over the stores. See `space_affinity.rs`.
    pub(crate) fn affinity(&self) -> SpaceAffinity<'_> {
        SpaceAffinity {
            windows: &self.state.windows,
            spaces: &self.space_state,
            engine: &self.layout_engine,
            transactions: &self.transactions,
            active_spaces: &self.active_spaces,
            space_kinds: self.reads.space_kinds,
            window_server: &*self.reads.window_server,
            live_spaces: &self.live_spaces,
        }
    }

    /// The localised name of the application `window` belongs to, empty when rini has none.
    fn app_name(&self, window: WindowId) -> String {
        self.apps
            .get(&window.pid)
            .and_then(|app| app.info.localized_name.clone())
            .unwrap_or_default()
    }

    fn is_login_window_pid(&self, pid: pid_t) -> bool {
        self.apps.get(&pid).and_then(|a| a.info.bundle_id.as_deref())
            == Some("com.apple.loginwindow")
    }

    fn is_mission_control_active(&self) -> bool {
        matches!(
            self.mission_control_manager.mission_control_state,
            MissionControlState::Active
        )
    }

    async fn run(mut reactor: Reactor, events: Receiver, events_tx: Sender) {
        let (raise_manager_tx, raise_manager_rx) = channels::channel();
        reactor.communication_manager.raise_manager_tx = raise_manager_tx.clone();
        let event_tap_tx = reactor.communication_manager.event_tap_tx.clone();
        let warp = move |point| {
            if let Some(event_tap_tx) = &event_tap_tx {
                event_tap_tx.send(crate::input::platform::input_tap::Request::Warp(point));
            }
        };
        let reactor_task = Self::run_reactor_loop(reactor, events);
        let raise_manager_task = RaiseManager::run(raise_manager_rx, events_tx, warp);
        let _ = tokio::join!(reactor_task, raise_manager_task);
    }

    async fn run_reactor_loop(mut reactor: Reactor, mut events: Receiver) {
        const MAX_EVENT_BATCH: usize = 64;

        while let Some(first) = events.recv().await {
            // Drain a bounded batch to reduce recv/select overhead.
            let rest = std::iter::from_fn(|| events.try_recv().ok()).take(MAX_EVENT_BATCH - 1);
            reactor.handle_batch(std::iter::once(first).chain(rest));
        }
    }

    /// One batch of the loop's events, then the strip laid out again if a rule now gives a window
    /// another border, and what the batch changed on the bar and at rest.
    fn handle_batch(&mut self, batch: impl IntoIterator<Item = (tracing::Span, Event)>) {
        for (span, event) in batch {
            let _guard = span.enter();
            self.handle_loop_event(event);
        }
        if self.refresh_borders() {
            self.update_layout(false, false, None);
        }
        self.publish_bar();
        self.publish_rest();
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
        self.note_windowserver_activity(&event);
        self.handle_event(event);
        #[cfg(any(test, debug_assertions))]
        self.state.windows.debug_assert_invariants();
    }

    fn log_event(&self, event: &Event) {
        match event {
            Event::WindowFrameChanged(..)
            | Event::MouseUp
            | Event::LeftMouseDown(_)
            | Event::MouseMoved(_) => trace!(?event, "Event"),
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

    #[instrument(name = "reactor::handle_event", skip(self), fields(event=?event))]
    fn handle_event(&mut self, event: Event) {
        let cause = Cause {
            at: std::time::Instant::now(),
            what: event.what(),
        };
        let outer_cause = std::mem::replace(&mut self.cause, cause);
        let began = self.layout_engine.focused_window();
        let outer = std::mem::replace(&mut self.focused_as_event_began, began);
        match self.dispatch_workflow(event) {
            Ok(outcome) => self.apply_event_outcome(outcome),
            Err(error) => warn!(%error, "reactor workflow failed"),
        }
        self.focused_as_event_began = outer;
        self.cause = outer_cause;
    }

    /// A focus report naming a window rini knows and does not manage is a panel's; see
    /// [`FocusEvent::PanelFocused`].
    fn recognise_panel(&self, focus: FocusEvent) -> FocusEvent {
        match focus {
            FocusEvent::WindowServerFocusChanged(window)
                if self
                    .state
                    .windows
                    .window(window)
                    .is_some_and(|state| !state.is_effectively_manageable()) =>
            {
                FocusEvent::PanelFocused(window)
            }
            other => other,
        }
    }

    /// Tells the flight engine whose face the key window turned if it changed, rini's own raise
    /// included, each managed window at its frame: at rest the engine pictures both afresh, and in
    /// flight whatever the flight does not picture. See "Focus changing during a flight" in
    /// `src/transitions/docs/coordination.md`.
    fn tell_faces_turned(&self, key_before: Option<WindowId>) {
        let turned = crate::windows::domain::focus::faces_turned(
            key_before,
            self.main_window_tracker.key_window(),
        );
        let windows: Vec<(WindowId, ScreenRect)> = turned
            .into_iter()
            .filter_map(|window| {
                let state = self.state.windows.window(window)?;
                let frame = ScreenRect::from_cg(state.frame_monotonic);
                state.is_effectively_manageable().then_some((window, frame))
            })
            .collect();
        if let (false, Some(tx)) = (windows.is_empty(), &self.communication_manager.transitions_tx)
        {
            tx.send(crate::transitions::platform::engine::Event::FacesTurned(windows));
        }
    }

    /// Applies workflow follow-up requests in one stable order.
    ///
    /// Explicit transition frames are written before layout calculation so the
    /// resulting layout remains authoritative. Focus selection follows layout
    /// writes, then UI/platform presentation state is refreshed. Discovery
    /// requests made directly by a workflow are consequently observed only after
    /// its model mutation is complete.
    ///
    /// Work a step hands back (a discovery's, a fullscreen restoration's, a pending space change's,
    /// a drag's release) has its steps before the arrange carried out where it is handed back, and
    /// the rest absorbed into this outcome: one arrange, one regroup and this event's cause for all
    /// of it. See "Carrying out an outcome" in `src/app/docs/README.md`.
    fn apply_event_outcome(&mut self, outcome: EventOutcome) {
        // Per event: what this event's regroup raises is only known to this event.
        self.regroup_raised.clear();
        let outcome = self.settle_ahead_of_arrange(outcome);
        self.arrange_and_follow_up(outcome);
    }

    /// Carries out `outcome`'s steps before the arrange now, depth-first, and returns the arrange and
    /// what follows it, with what each step handed back left over ahead of `outcome`'s own.
    pub(super) fn settle_ahead_of_arrange(&mut self, mut outcome: EventOutcome) -> EventOutcome {
        if let Some(work) = outcome.take_work_before_arrange() {
            outcome.absorb_ahead(self.settle_before_arrange(work));
        }
        outcome
    }

    /// The steps before the arrange, each handed-back outcome settled where it is handed back;
    /// returns what they leave for the arrange and after it, in the order they were handed back.
    fn settle_before_arrange(&mut self, work: EventOutcome) -> EventOutcome {
        let mut handed_back = EventOutcome::default();
        // Exhaustive so a field taken before the arrange cannot go unused here.
        let EventOutcome {
            window_server_updates,
            discoveries,
            recompute_active_spaces,
            repair_spaces_after_mission_control,
            refresh_after_mission_control,
            force_refresh_all_windows,
            wm_events: _,
            app_requests: _,
            topology_reassignments,
            forgotten_windows,
            confirmed_window_spaces,
            fullscreen_restorations,
            raise_requests: _,
            make_key_windows: _,
            mouse_warps: _,
            pre_layout_window_frame_writes,
            drag_swap_evaluations,
            dispatch_mouse_up,
            close_window: _,
            service_config_update: _,
            stdout_lines: _,
            reapply_app_rules,
            finalize_created_windows,
            layout_events,
            layout_responses,
            arrange: _,
            focused_window: _,
            focus_landed_before_arrange: _,
            refresh_window_notifications: _,
            refresh_focus_follows_mouse: _,
        } = work;
        // Ahead of the layout events: a removed window's pictures go first.
        for window in forgotten_windows {
            if let Some(pictures) = &self.communication_manager.pictures {
                pictures.forget(window);
            }
        }
        if !window_server_updates.is_empty() {
            self.update_partial_window_server_info(window_server_updates);
        }
        if recompute_active_spaces {
            self.recompute_and_set_active_spaces_from_current_screens();
        }
        if repair_spaces_after_mission_control {
            handed_back.absorb(self.repair_spaces_after_mission_control());
        }
        if refresh_after_mission_control {
            self.refresh_windows_after_mission_control();
        }
        if force_refresh_all_windows {
            self.force_refresh_all_windows();
        }
        // Discovery responses reconcile model state before layout. Requests
        // which schedule new discovery are deferred to the final phase below.
        for discovery in discoveries {
            let discovered = self.on_windows_discovered_with_app_info(
                discovery.pid,
                discovery.new,
                discovery.known_visible,
                discovery.app_info,
            );
            handed_back.absorb(self.settle_ahead_of_arrange(discovered));
        }
        for window in reapply_app_rules {
            self.maybe_reapply_app_rules_for_window(window);
        }
        for window in finalize_created_windows {
            self.finalize_created_window(window);
        }

        for (window_server_id, space) in confirmed_window_spaces {
            self.clear_pending_target_if_confirmed_space(window_server_id, space);
        }
        for (window_server_id, space, window) in fullscreen_restorations {
            let restored = self.restore_fullscreen_window(window_server_id, space, window);
            handed_back.absorb(self.settle_ahead_of_arrange(restored));
        }
        for reassignment in topology_reassignments {
            self.reassign_window_to_authoritative_space(reassignment.window, reassignment.space);
        }

        // Some transitions need to place a window on its destination display
        // before arranging that display. Keep these writes ahead of both layout
        // responses and the arrange pass so tiling always supplies the final frame.
        for write in pre_layout_window_frame_writes {
            self.write_pre_layout_frame(write);
        }

        for event in layout_events {
            self.send_layout_event(event);
        }
        for (response, workspace_switch_space) in layout_responses {
            self.handle_layout_response(response, workspace_switch_space);
        }
        for (window, frame) in drag_swap_evaluations {
            self.maybe_swap_on_drag(window, frame);
        }
        // After the drag swap above, which reads the drag the release ends.
        if dispatch_mouse_up {
            match self.on_mouse_up() {
                Ok(released) => handed_back.absorb(self.settle_ahead_of_arrange(released)),
                Err(error) => warn!(%error, "mouse up failed"),
            }
        }
        handed_back
    }

    /// Back from native fullscreen to `space`, or reassigned there when nothing is held to restore.
    fn restore_fullscreen_window(
        &mut self,
        window_server_id: WindowServerId,
        space: SpaceId,
        window: WindowId,
    ) -> EventOutcome {
        let mut restored = EventOutcome::default();
        if self
            .restore_fullscreen_window_to_user_space(window_server_id, space, window, &mut restored)
            .is_none()
        {
            self.reassign_window_to_authoritative_space(window, space);
        }
        restored
    }

    fn finalize_created_window(&mut self, window: WindowId) {
        let active_space = self.state.windows.window(window).and_then(|state| {
            self.affinity()
                .best_space_for_window(&state.frame_monotonic, state.info.sys_id)
                .filter(|space| self.is_space_active(*space))
                .or_else(|| {
                    state.info.sys_id.is_none().then(|| self.workspace_command_space()).flatten()
                })
        });
        let Some(space) = active_space else {
            return;
        };
        if let Some(app_info) = self.apps.get(&window.pid).map(|app| app.info.clone()) {
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

    fn write_pre_layout_frame(&mut self, write: WindowFrameWriteRequest) {
        let window_server_id =
            self.state.windows.window(write.window).and_then(|window| window.info.sys_id);
        let transaction = if let Some(window_server_id) = window_server_id {
            let transaction = self.transactions.next_txid(window_server_id);
            self.transactions.insert(window_server_id, transaction, write.frame);
            transaction
        } else {
            TransactionId::default()
        };
        if let Some(app) = self.apps.get(&write.window.pid)
            && let Err(error) = app.handle.send(Request::SetWindowFrame(
                write.window,
                write.frame,
                transaction,
                true,
            ))
        {
            warn!(window = ?write.window, %error, "failed to write requested window frame");
        }
    }

    /// The arrange, once for everything settled before it, and what follows it.
    fn arrange_and_follow_up(&mut self, outcome: EventOutcome) {
        let focus_landed = outcome.focus_landed();
        let mut layout_changed = false;
        if let Some(passes) = outcome.arrange.passes_to_run(self.is_in_drag()) {
            for _ in 0..passes {
                layout_changed |= self.update_layout(
                    outcome.arrange.is_resize,
                    matches!(
                        self.workspace_switch_manager.workspace_switch_state,
                        WorkspaceSwitchState::Active
                    ),
                    outcome.arrange.space_scope,
                );
            }
        }
        // A move is only a move for the passes its command asked for.
        self.moved_along_strip = None;

        for request in outcome.raise_requests {
            self.dispatch_raise(request);
        }
        if let Some((space, window)) = focus_service::resolve(outcome.focused_window, |wid| {
            self.affinity().best_space_for_window_id(wid)
        }) {
            self.send_layout_event(LayoutEvent::WindowFocused(space, window));
        }

        // After the raises above, so a column this pass scrolled in is lifted over a floating window
        // in a sequence that ends after the focus raise, with the focused window last again. A focus
        // report with no layout change (a click) is judged too: macOS raised only the clicked window.
        if layout_changed || focus_landed {
            self.regroup_after_layout();
        }

        for (pid, window) in outcome.make_key_windows {
            if let Err(error) = self.reads.window_server.make_key_window(pid, window) {
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
            self.close_requested_window(window_server_id);
        }
        if let Some(config) = outcome.service_config_update {
            self.publish_service_config(config);
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
        if let Some(wid) = self.workspace_switch_manager.pending_workspace_mouse_warp.take()
            && let Some(window_center) = self.affinity().window_center_on_known_screen(wid)
        {
            self.warp_mouse(window_center);
        }

        if outcome.refresh_window_notifications {
            self.refresh_window_notifications();
        }
        if outcome.refresh_focus_follows_mouse {
            self.update_focus_follows_mouse_state();
        }
        // Requests which schedule fresh discovery are last so observers see
        // the fully reconciled model, layout and UI.
        for (pid, request) in outcome.app_requests {
            if let Some(app) = self.apps.get(&pid)
                && let Err(error) = app.handle.send(request)
            {
                warn!(pid, %error, "failed to send deferred application request");
            }
        }
    }

    fn close_requested_window(&mut self, window_server_id: Option<WindowServerId>) {
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

    fn publish_service_config(&mut self, config: Config) {
        if let Some(wm) = &self.communication_manager.wm_sender {
            wm.send(crate::app::hotkeys::WmEvent::ConfigUpdated(Box::new(config)));
        }
        // Re-sends both the flag and the geometry, so toggling
        // warp_cursor_between_stacked_displays applies without a restart.
        self.publish_cursor_warp_screens();
        self.publish_animation_display();
        self.publish_journal();
    }

    fn refresh_window_notifications(&mut self) {
        let tracked = self.state.windows.iter_tracked_window_server_ids().map(|wsid| wsid.as_u32());
        if let Some(ids) =
            managers::notification_ids_to_publish(&self.last_sls_notification_ids, tracked)
        {
            (self.reads.watch_windows)(&ids);
            self.last_sls_notification_ids = ids;
        }
    }

    fn insert_app_handle_for_window(
        &self,
        app_handles: &mut HashMap<pid_t, AppThreadHandle>,
        wid: WindowId,
    ) {
        if let Some(app) = self.apps.get(&wid.pid) {
            app_handles.insert(wid.pid, app.handle.clone());
        }
    }

    fn window_is_standard(&self, id: WindowId) -> bool {
        self.state
            .windows
            .window(id)
            .is_some_and(|window| window.matches_filter(WindowFilter::EffectivelyManageable))
    }

    fn request_close_window(&mut self, pid: pid_t, window_server_id: Option<WindowServerId>) {
        if let Some(app) = self.apps.get(&pid)
            && let Err(err) = app.handle.send(Request::CloseWindow(window_server_id))
        {
            warn!(
                pid,
                ?window_server_id,
                "Failed to send close window request: {}",
                err
            );
        }
    }

    pub(crate) fn main_window(&self) -> Option<WindowId> { self.main_window_tracker.main_window() }
}
