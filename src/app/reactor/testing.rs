use std::any::Any;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::{SpaceId, WindowId, WindowServerId, pid_t};
use rini_geometry::SameAs;
use rustc_hash::FxHashMap as HashMap;
use tracing::debug;

use super::{Event, EventOutcome, MenuState, Reactor, ScreenInfo, TransactionId};
use crate::app::channels;
use crate::app::config::{Config, LayoutSettings, Settings};
use crate::app::reactor::state::RiniState;
use crate::app::system_reads::SystemReads;
use crate::displays::domain::topology::{ForwardedSpaceState, SpaceEventKind, Topology};
use crate::transitions::domain::request::Cause;
use crate::windows::domain::focus::{MainWindowTracker, RaiseEcho};
use crate::windows::domain::info::{AppInfo, WindowInfo, WindowServerInfo};
use crate::windows::domain::request::{AppThreadHandle, Quiet, Request};
use crate::windows::domain::transaction::{Requested, WindowTxStore};
use crate::windows::platform::window_server::{FakeWindowServer, Listed};
use crate::workspaces::domain::display_affinity::DisplayAffinity;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::domain::window_store::WindowStore;
use crate::workspaces::{LayoutCommand, LayoutEngine, WorkspaceCommand};

impl Reactor {
    pub fn new_for_test(layout: LayoutEngine) -> Reactor {
        let mut config = Config::reference();
        config.settings.activate_spaces_by_default = true;
        config.settings.animate = false;
        let reads = SystemReads::fake(Arc::new(FakeWindowServer::default()));
        let mut reactor = Reactor::with_reads(
            config,
            layout,
            DisplayMemory::default(),
            WindowTxStore::new(),
            reads,
        );
        // Never let the suite write to the real layout file. Almost every test drives
        // `update_layout`, which autosaves; a test that wants to exercise autosave sets a temp path.
        reactor.autosave_path = None;
        reactor
    }

    /// The on-screen order, frontmost first, each window at the normal level.
    pub fn state_front_to_back(&self, order: &[u32]) {
        let listed = order
            .iter()
            .map(|&id| Listed {
                id: WindowServerId::new(id),
                level: 0,
                pid: 1234,
                bounds: CGRect::default(),
                title: None,
            })
            .collect();
        self.window_server().set_on_screen_windows(listed);
    }

    /// The window server a test reactor reads, for the test to state its answers on.
    pub fn window_server(&self) -> &FakeWindowServer {
        let port: &dyn Any = &*self.reads.window_server;
        port.downcast_ref().expect("a test reactor reads a FakeWindowServer")
    }

    /// A window-server window appearing on `space`, as the reactor hears it.
    pub fn window_server_appeared(
        &mut self,
        wsid: WindowServerId,
        space: SpaceId,
        kind: SpaceEventKind,
    ) {
        self.handle_event(Event::WindowServerAppeared(wsid, space, kind));
    }

    /// A window-server window destroyed on `space`, its outcome applied.
    pub fn window_server_destroyed(
        &mut self,
        wsid: WindowServerId,
        space: SpaceId,
        kind: SpaceEventKind,
    ) {
        let outcome = self.on_window_server_destroyed(wsid, space, kind).unwrap();
        self.apply_event_outcome(outcome);
    }

    pub fn handle_events(&mut self, events: Vec<Event>) {
        for event in events {
            self.handle_event(event);
        }
    }

    pub fn test_workspace_ids(
        &mut self,
        space: rini_core::ids::SpaceId,
    ) -> Vec<crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId> {
        self.layout_engine
            .virtual_workspace_manager_mut()
            .list_workspaces(space)
            .iter()
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn test_workspace(
        &mut self,
        space: rini_core::ids::SpaceId,
        index: usize,
    ) -> crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId {
        self.test_workspace_ids(space)[index]
    }

    /// Make `window` the focused window, as WindowServer focus would.
    pub fn set_test_focus(&mut self, window: WindowId) {
        self.main_window_tracker.set_focus_for_test(window);
    }

    /// Which workspace `space` is currently showing.
    pub fn test_active_workspace(
        &self,
        space: rini_core::ids::SpaceId,
    ) -> Option<crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId> {
        self.layout_engine.active_workspace(space)
    }

    pub fn test_workspace_for_window(
        &self,
        space: rini_core::ids::SpaceId,
        wid: WindowId,
    ) -> Option<crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId> {
        self.layout_engine.virtual_workspace_manager().workspace_for_window(
            &self.state.windows,
            space,
            wid,
        )
    }

    pub fn test_window_server_id(&self, wid: WindowId) -> WindowServerId {
        self.state
            .windows
            .window(wid)
            .and_then(|window| window.info.sys_id)
            .expect("test window should have a WindowServer identity")
    }

    pub fn test_active_workspace_windows(&self, space: SpaceId) -> Vec<WindowId> {
        self.layout_engine.windows_in_active_workspace(&self.state.windows, space)
    }

    pub fn test_workspace_windows(
        &self,
        space: SpaceId,
        workspace: crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId,
    ) -> Vec<WindowId> {
        self.layout_engine.virtual_workspace_manager().workspace_windows(
            &self.state.windows,
            space,
            workspace,
        )
    }

    pub fn assign_test_window_to_workspace(
        &mut self,
        space: SpaceId,
        wid: WindowId,
        workspace: crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId,
    ) -> bool {
        self.layout_engine.virtual_workspace_manager_mut().assign_window_to_workspace(
            &mut self.state.windows,
            space,
            wid,
            workspace,
        )
    }

    pub fn set_test_active_workspace(
        &mut self,
        space: SpaceId,
        workspace: crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId,
    ) -> bool {
        self.layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space, workspace)
    }

    pub fn handle_test_workspace_command(&mut self, space: SpaceId, command: WorkspaceCommand) {
        let _ = self.layout_engine.handle_command(
            &mut self.state.windows,
            &mut self.state.display_memory,
            Some(space),
            &[space],
            &HashMap::default(),
            command.into(),
        );
    }

    /// Drive the drag-swap evaluation the way a real drag frame does.
    pub fn maybe_swap_on_drag_for_test(&mut self, wid: WindowId, new_frame: CGRect) {
        self.maybe_swap_on_drag(wid, new_frame);
    }

    /// Drive a switcher session signal the way the input thread does, returning the outcome.
    pub(crate) fn dispatch_test_switch(
        &mut self,
        signal: crate::input::domain::switch_session::Signal,
    ) -> EventOutcome {
        self.dispatch_workflow(Event::Switch(signal))
            .expect("switch signal should dispatch")
    }

    /// Wire in a switcher popup, returning what it is sent.
    pub fn connect_test_switcher(&mut self) -> crate::switcher::platform::actor::Receiver {
        let (tx, rx) = channels::channel();
        self.switcher_tx = Some(tx);
        rx
    }

    /// Wire in a pictures service that only records what it is asked, returning the record.
    pub fn connect_test_pictures(&mut self) -> PicturesAsked {
        let (pictures, asked) = crate::pictures::platform::service::Pictures::detached();
        self.communication_manager.pictures = Some(pictures);
        asked
    }

    /// Click a popup row the way the switcher panel does, returning the outcome.
    pub(crate) fn dispatch_test_pick(&mut self, window: WindowId) -> EventOutcome {
        self.dispatch_workflow(Event::SwitchPicked(window))
            .expect("pick should dispatch")
    }

    /// Handle `events` as one batch of the reactor loop, which ends by sending the bars what the batch
    /// changed.
    pub fn handle_test_batch(&mut self, events: Vec<Event>) {
        self.handle_batch(events.into_iter().map(|event| (tracing::Span::none(), event)));
    }

    /// Where the active workspace's strip had each window at the last pass.
    pub fn test_strip_layout(&self, space: SpaceId) -> Vec<(WindowId, CGRect)> {
        let workspace = self.layout_engine.active_workspace(space);
        workspace
            .and_then(|workspace| self.last_strip_layout.get(&(space, workspace)).cloned())
            .unwrap_or_default()
    }

    /// The tiled frames of `space`'s active workspace on `screen`, at their strip coordinates and
    /// in strip order, the strip spending what it held as a pass does.
    pub fn test_tiled_layout(&mut self, space: SpaceId, screen: CGRect) -> Vec<(WindowId, CGRect)> {
        let gaps = self.config.settings.layout.gaps.effective_for_display(None);
        let engine = &mut self.layout_engine;
        let Some(workspace) = engine.active_workspace(space) else {
            return Vec::new();
        };
        let frames: HashMap<WindowId, CGRect> = engine
            .calculate_layout_for_workspace(&self.state.windows, space, workspace, screen, &gaps)
            .into_iter()
            .collect();
        let strip = engine.ordered_windows_in_active_workspace(space);
        strip.into_iter().filter_map(|wid| Some((wid, *frames.get(&wid)?))).collect()
    }

    /// Wire in the rest windows, returning what they are sent.
    pub fn connect_test_rest(&mut self) -> crate::decorations::platform::rest::Receiver {
        let (tx, rx) = channels::channel();
        self.rest.tx = Some(tx);
        rx
    }

    /// Wire in a bar, returning what it is sent.
    pub fn connect_test_bar(&mut self) -> crate::bar::platform::actor::Receiver {
        let (tx, rx) = channels::channel();
        self.bar.tx = Some(tx);
        rx
    }

    /// Click the bar the way its actor does, returning the outcome.
    pub(crate) fn dispatch_test_bar_action(
        &mut self,
        action: crate::bar::domain::model::Action,
    ) -> EventOutcome {
        self.dispatch_workflow(Event::BarAction(action))
            .expect("bar action should dispatch")
    }

    pub fn handle_test_layout_command(&mut self, command: LayoutCommand) {
        self.handle_event(Event::Command(crate::app::reactor::Command::Layout(command)));
    }

    pub(crate) fn dispatch_test_layout_command(&mut self, command: LayoutCommand) -> EventOutcome {
        self.dispatch_workflow(Event::Command(crate::app::reactor::Command::Layout(command)))
            .expect("test layout command should dispatch")
    }

    pub fn mark_test_window_visible_in_space(&mut self, wsid: WindowServerId, space: SpaceId) {
        self.state.windows.set_window_server_space(wsid, Some(space));
        self.state.windows.mark_window_visible(wsid);
    }

    pub fn discover_test_windows(
        &mut self,
        pid: pid_t,
        new: Vec<(WindowId, WindowInfo)>,
        known_visible: Vec<WindowId>,
    ) {
        self.handle_event(Event::WindowsDiscovered { pid, new, known_visible });
    }

    /// Install app rules and rebuild the rule engine, as a config reload does.
    pub fn set_test_app_rules(&mut self, rules: Vec<crate::windows::domain::rules::WindowRule>) {
        self.config.virtual_workspaces.app_rules = rules;
        let settings = self.config.virtual_workspaces.clone();
        self.layout_engine.update_virtual_workspace_settings(&settings);
    }

    pub fn add_test_app(&mut self, pid: pid_t) {
        self.add_test_app_with_info(pid, "com.test.app", "Test App");
    }

    pub fn add_test_app_with_info(&mut self, pid: pid_t, bundle_id: &str, name: &str) {
        let _ = self.add_test_app_listening(pid, bundle_id, name);
    }

    /// Register an app with the reactor, returning what its thread is asked.
    pub fn add_test_app_listening(
        &mut self,
        pid: pid_t,
        bundle_id: &str,
        name: &str,
    ) -> channels::Receiver<Request> {
        let (app_tx, app_rx) = channels::channel();
        self.apps.insert(pid, super::AppState {
            info: AppInfo {
                bundle_id: Some(bundle_id.to_string()),
                localized_name: Some(name.to_string()),
            },
            handle: AppThreadHandle::from_sender(app_tx),
        });
        app_rx
    }

    pub fn add_test_window(
        &mut self,
        wid: WindowId,
        wsid: WindowServerId,
        space: Option<SpaceId>,
        frame: CGRect,
    ) {
        self.add_test_window_with_manageability(wid, wsid, space, frame, true);
    }

    pub fn add_test_window_with_manageability(
        &mut self,
        wid: WindowId,
        wsid: WindowServerId,
        space: Option<SpaceId>,
        frame: CGRect,
        is_manageable: bool,
    ) {
        self.track_test_window_server_info(wsid, wid.pid, frame);
        self.state.windows.mark_window_visible(wsid);
        self.insert_test_window(wid, wsid, space, frame, is_manageable);
    }

    pub fn track_test_window_server_info(
        &mut self,
        wsid: WindowServerId,
        pid: pid_t,
        frame: CGRect,
    ) {
        self.state.windows.track_window_server_info(WindowServerInfo {
            id: wsid,
            pid,
            layer: 0,
            frame,
            min_frame: frame.size,
            max_frame: frame.size,
        });
    }

    pub fn insert_test_window(
        &mut self,
        wid: WindowId,
        wsid: WindowServerId,
        space: Option<SpaceId>,
        frame: CGRect,
        is_manageable: bool,
    ) {
        self.state.windows.track_window_server_id(wsid, wid);
        self.state.windows.set_window_server_space(wsid, space);
        self.insert_test_window_state(wid, frame, Some(wsid), is_manageable);
    }

    pub fn insert_test_window_state(
        &mut self,
        wid: WindowId,
        frame: CGRect,
        sys_id: Option<WindowServerId>,
        is_manageable: bool,
    ) {
        self.state
            .windows
            .insert_window(wid, crate::windows::domain::state::WindowState {
                info: WindowInfo {
                    is_standard: true,
                    is_root: true,
                    is_minimized: false,
                    is_resizable: true,
                    min_size: None,
                    max_size: None,
                    title: format!("Window {wid:?}"),
                    frame,
                    sys_id,
                    bundle_id: None,
                    path: None,
                    ax_role: None,
                    ax_subrole: None,
                    is_modal: false,
                },
                frame_monotonic: frame,
                is_manageable,
                ignore_app_rule: false,
            });
    }
}

/// What a test reads and arranges of the reactor's stores, so a case names what it looks at rather
/// than where the reactor keeps it.
impl Reactor {
    pub(crate) fn windows(&self) -> &WindowStore { &self.state.windows }

    pub(crate) fn windows_mut(&mut self) -> &mut WindowStore { &mut self.state.windows }

    pub(crate) fn rini_state(&self) -> &RiniState { &self.state }

    pub(crate) fn layout(&self) -> &LayoutEngine { &self.layout_engine }

    pub(crate) fn layout_mut(&mut self) -> &mut LayoutEngine { &mut self.layout_engine }

    pub(crate) fn display_affinity(&self) -> &DisplayAffinity {
        &self.state.display_memory.affinity
    }

    pub(crate) fn display_affinity_mut(&mut self) -> &mut DisplayAffinity {
        &mut self.state.display_memory.affinity
    }

    pub(crate) fn config(&self) -> &Config { &self.config }

    pub(crate) fn config_mut(&mut self) -> &mut Config { &mut self.config }

    pub(crate) fn settings(&self) -> &Settings { &self.config.settings }

    pub(crate) fn settings_mut(&mut self) -> &mut Settings { &mut self.config.settings }

    /// Change the layout settings and hand them to the layout, as a config reload does.
    pub(crate) fn set_test_layout_settings(&mut self, edit: impl FnOnce(&mut LayoutSettings)) {
        edit(&mut self.config.settings.layout);
        self.layout_engine.set_layout_settings(&self.config.settings.layout);
    }

    pub(crate) fn transactions(&self) -> &WindowTxStore { &self.transactions }

    pub(crate) fn transactions_mut(&mut self) -> &mut WindowTxStore { &mut self.transactions }

    /// Record a write of `frame` to `wsid` as sent and not yet echoed, returning its transaction.
    pub(crate) fn pend_test_frame(&mut self, wsid: WindowServerId, frame: CGRect) -> TransactionId {
        let txid = self.transactions.next_txid(wsid);
        self.transactions.insert(wsid, txid, frame);
        txid
    }

    pub(crate) fn reads(&self) -> &SystemReads { &self.reads }

    pub(crate) fn reads_mut(&mut self) -> &mut SystemReads { &mut self.reads }

    pub(crate) fn space_state(&self) -> &ForwardedSpaceState { &self.space_state }

    pub(crate) fn space_state_mut(&mut self) -> &mut ForwardedSpaceState { &mut self.space_state }

    /// The space snapshot held while Mission Control is open.
    pub(crate) fn pending_space_change(&self) -> Option<&ForwardedSpaceState> {
        self.pending_space_change.as_ref()
    }

    pub(super) fn quarantine(&self) -> &super::managers::RefreshQuarantineManager {
        &self.refresh_quarantine_manager
    }

    pub(super) fn drag(&self) -> &super::managers::DragManager { &self.drag_manager }

    pub(super) fn drag_mut(&mut self) -> &mut super::managers::DragManager {
        &mut self.drag_manager
    }

    pub(super) fn workspace_switches_mut(
        &mut self,
    ) -> &mut super::managers::WorkspaceSwitchManager {
        &mut self.workspace_switch_manager
    }

    pub(crate) fn menu_state(&self) -> &MenuState { &self.menu_state }

    pub(crate) fn focus_tracker(&self) -> &MainWindowTracker { &self.main_window_tracker }

    pub(crate) fn focus_tracker_mut(&mut self) -> &mut MainWindowTracker {
        &mut self.main_window_tracker
    }

    pub(crate) fn raise_echo_mut(&mut self) -> &mut RaiseEcho { &mut self.raise_echo }

    pub(crate) fn cause(&self) -> &Cause { &self.cause }

    pub(crate) fn set_test_cause(&mut self, cause: Cause) { self.cause = cause; }

    pub(crate) fn switch_is_open(&self) -> bool { self.live_switch.is_some() }

    /// Whether the rest windows follow focus.
    pub(crate) fn rest_follows_focus(&self) -> bool { self.rest.following.on() }

    /// Forget which windows a layout pass has asked a picture of.
    pub(crate) fn forget_pictured_windows(&mut self) { self.pictured_windows.clear(); }

    /// Hold `pid`'s next refresh until Mission Control closes.
    pub(crate) fn defer_test_refresh_past_mission_control(&mut self, pid: pid_t) {
        self.mission_control_manager.pending_mission_control_refresh.insert(pid);
    }

    /// Autosave to `path`, which a test owns.
    pub(crate) fn set_test_autosave_path(&mut self, path: PathBuf) {
        self.autosave_path = Some(path);
    }

    pub(crate) fn apps(&self) -> &HashMap<pid_t, super::AppState> { &self.apps }

    /// The frame rini last knew `wid` at.
    pub(crate) fn test_frame(&self, wid: WindowId) -> CGRect {
        self.state
            .windows
            .window(wid)
            .expect("test window should be tracked")
            .frame_monotonic
    }

    pub(crate) fn set_test_frame(&mut self, wid: WindowId, frame: CGRect) {
        self.state
            .windows
            .window_mut(wid)
            .expect("test window should be tracked")
            .frame_monotonic = frame;
    }

    /// Wire in a flight engine, returning what it is sent.
    pub(crate) fn connect_test_transitions(
        &mut self,
    ) -> crate::transitions::platform::engine::Receiver {
        let (tx, rx) = channels::channel();
        self.communication_manager.transitions_tx = Some(tx);
        rx
    }

    /// Wire in a raise manager, returning what it is sent.
    pub(crate) fn connect_test_raises(&mut self) -> crate::windows::domain::raise::Receiver {
        let (tx, rx) = channels::channel();
        self.communication_manager.raise_manager_tx = tx;
        rx
    }

    /// Wire in the input tap, returning what it is sent.
    pub(crate) fn connect_test_event_tap(&mut self) -> crate::input::platform::input_tap::Receiver {
        let (tx, rx) = channels::channel();
        self.communication_manager.event_tap_tx = Some(tx);
        rx
    }

    /// Wire in the controller, returning what it is sent.
    pub(crate) fn connect_test_controller(
        &mut self,
    ) -> channels::Receiver<crate::app::controller::WmEvent> {
        let (tx, rx) = channels::channel();
        self.communication_manager.wm_sender = Some(tx);
        rx
    }

    /// Take the pictures service away, returning it to be put back with
    /// [`Reactor::restore_test_pictures`].
    pub(crate) fn take_test_pictures(
        &mut self,
    ) -> Option<crate::pictures::platform::service::Pictures> {
        self.communication_manager.pictures.take()
    }

    pub(crate) fn restore_test_pictures(
        &mut self,
        pictures: Option<crate::pictures::platform::service::Pictures>,
    ) {
        self.communication_manager.pictures = pictures;
    }

    pub(super) fn workspace_switches(&self) -> &super::managers::WorkspaceSwitchManager {
        &self.workspace_switch_manager
    }

    /// The active workspace's layout of `space` on `screen`, the only screen, with the display's
    /// gaps and each window at the frame rini last knew it at.
    pub(crate) fn test_layout_with_virtual_workspaces(
        &mut self,
        space: SpaceId,
        screen: CGRect,
    ) -> Vec<(WindowId, CGRect)> {
        let gaps = self.config.settings.layout.gaps.effective_for_display(None);
        let windows = &self.state.windows;
        self.layout_engine.calculate_layout_with_virtual_workspaces(
            windows,
            space,
            screen,
            &gaps,
            |window| windows.window(window).map(|state| state.frame_monotonic),
            &[screen],
        )
    }

    /// Write the layout file at `path` the way an autosave does.
    pub(crate) fn autosave_test_layout(
        &mut self,
        path: PathBuf,
        active_space: Option<SpaceId>,
    ) -> std::io::Result<()> {
        self.layout_engine.autosave_current_layout(
            path,
            &self.state.windows,
            &self.state.display_memory,
            active_space,
        )
    }

    /// Carry `old`'s workspaces over to `new`, as a space id change does.
    pub(crate) fn remap_test_space(&mut self, old: SpaceId, new: SpaceId) {
        self.layout_engine.remap_space(
            &mut self.state.windows,
            &mut self.state.display_memory,
            old,
            new,
        );
    }

    /// Home `window` to the display showing `space`.
    pub(crate) fn set_test_window_display_home(&mut self, window: WindowId, space: SpaceId) {
        self.layout_engine
            .set_window_display_home(&mut self.state.display_memory, window, space);
    }

    /// Hand the window-destroyed reducer `window` over the reactor's own stores.
    pub(crate) fn reduce_test_window_destroyed(
        &mut self,
        window: WindowId,
    ) -> anyhow::Result<EventOutcome> {
        super::events::window::handle_window_destroyed(
            &mut self.state,
            &self.transactions,
            &mut self.drag_manager,
            super::events::window::WindowDestroyedPayload { window },
        )
    }

    /// Hand the mouse-up reducer `payload` over the reactor's own stores.
    pub(crate) fn reduce_test_mouse_up(
        &mut self,
        payload: super::events::drag::MouseUpPayload,
    ) -> anyhow::Result<EventOutcome> {
        super::events::drag::handle_mouse_up(
            &mut self.state,
            &mut self.layout_engine,
            &mut self.drag_manager,
            payload,
        )
    }
}

/// The default reactor used by the tests. Keep the individual tests focused on
/// the behavior they exercise instead of repeating the production wiring.
pub fn test_reactor() -> Reactor {
    test_reactor_with_workspace_settings(&crate::app::config::VirtualWorkspaceSettings::default())
}

pub fn test_reactor_with_workspace_settings(
    workspace_settings: &crate::app::config::VirtualWorkspaceSettings,
) -> Reactor {
    Reactor::new_for_test(LayoutEngine::new(
        workspace_settings,
        &crate::app::config::LayoutSettings::default(),
    ))
}

pub fn test_reactor_with_workspace_count(count: usize) -> Reactor {
    let mut settings = crate::app::config::VirtualWorkspaceSettings::default();
    settings.workspace_names = crate::workspaces::settings::numbered_names(count);
    test_reactor_with_workspace_settings(&settings)
}

/// What a test pictures service was asked; see `Reactor::connect_test_pictures`.
pub type PicturesAsked = channels::Receiver<crate::pictures::platform::service::Command>;

/// Everything the test pictures service was asked since it was last read.
pub fn pictures_asked(
    asked: &mut PicturesAsked,
) -> Vec<crate::pictures::platform::service::Command> {
    std::iter::from_fn(|| asked.try_recv().ok().map(|(_, command)| command)).collect()
}

/// The windows asked for at `priority`, in the order asked.
pub fn asked_at(
    commands: &[crate::pictures::platform::service::Command],
    priority: crate::pictures::domain::schedule::Priority,
) -> Vec<WindowId> {
    commands
        .iter()
        .filter_map(|command| match command {
            crate::pictures::platform::service::Command::Ask(ask) if ask.priority == priority => {
                Some(ask.targets.iter().map(|target| target.window))
            }
            _ => None,
        })
        .flatten()
        .collect()
}

pub fn make_screen_snapshots(frames: Vec<CGRect>, spaces: Vec<Option<SpaceId>>) -> Vec<ScreenInfo> {
    assert_eq!(frames.len(), spaces.len());
    frames
        .into_iter()
        .zip(spaces)
        .enumerate()
        .map(|(idx, (frame, space))| ScreenInfo {
            is_builtin: false,
            id: rini_core::ids::ScreenId::new(idx as u32),
            frame,
            bounds: frame,
            space,
            display_uuid: format!("test-display-{idx}"),
            name: None,
            scale: 2.0,
        })
        .collect()
}

pub fn space_state_event(frames: Vec<CGRect>, spaces: Vec<Option<SpaceId>>) -> Event {
    space_state_event_from_screens(make_screen_snapshots(frames, spaces))
}

pub fn space_state_event_with(
    frames: Vec<CGRect>,
    spaces: Vec<Option<SpaceId>>,
    update: impl FnOnce(&mut ForwardedSpaceState),
) -> Event {
    let mut state = forwarded_space_state(make_screen_snapshots(frames, spaces));
    update(&mut state);
    Event::SpaceStateChanged(state)
}

pub fn space_state_event_from_screens(screens: Vec<ScreenInfo>) -> Event {
    Event::SpaceStateChanged(forwarded_space_state(screens))
}

/// Screens given by hand, so a test can name the display UUIDs rather than take `test-display-N`.
/// Needed by anything about display IDENTITY: which arrangement is in force depends on the UUIDs,
/// so "a different external" cannot be expressed by an index.
pub fn space_state_event_from_screens_with(
    screens: Vec<ScreenInfo>,
    update: impl FnOnce(&mut ForwardedSpaceState),
) -> Event {
    let mut state = forwarded_space_state(screens);
    update(&mut state);
    Event::SpaceStateChanged(state)
}

pub fn forwarded_space_state(screens: Vec<ScreenInfo>) -> ForwardedSpaceState {
    let command_space = screens.iter().find_map(|screen| screen.space);
    let active_spaces = screens.iter().filter_map(|screen| screen.space).collect();
    ForwardedSpaceState {
        screens,
        fullscreen_spaces: Default::default(),
        has_seen_display_set: false,
        active_spaces,
        menu_bar_space: command_space,
        command_space,
        display_space_ids: Default::default(),
        display_space_ids_from_screens: false,
        last_user_space_by_display: Default::default(),
        space_remaps: Vec::new(),
        topology: Topology::Unchanged,
        releases_lifecycle_refresh_quarantine: false,
        resized_spaces: Vec::new(),
        topology_window_delta: None,
        active_window_spaces: Default::default(),
    }
}

pub fn fullscreen_startup_space_state(
    screen: CGRect,
    display_uuid: String,
    user_space: SpaceId,
    fullscreen_space: SpaceId,
) -> Event {
    let mut state = forwarded_space_state(vec![ScreenInfo {
        is_builtin: false,
        id: rini_core::ids::ScreenId::new(0),
        frame: screen,
        bounds: screen,
        space: None,
        display_uuid,
        name: None,
        scale: 2.0,
    }]);
    state.fullscreen_spaces.insert(fullscreen_space);
    state.has_seen_display_set = true;
    state.active_spaces.clear();
    state.menu_bar_space = None;
    state.command_space = None;
    state
        .last_user_space_by_display
        .insert(state.screens[0].display_uuid.clone(), user_space);
    Event::SpaceStateChanged(state)
}

pub fn make_window(idx: usize) -> WindowInfo {
    make_window_info(
        CGRect::new(
            CGPoint::new(100.0 * f64::from(idx as u32), 100.0),
            CGSize::new(50.0, 50.0),
        ),
        Some(WindowServerId::new(idx as u32)),
        &format!("Window{idx}"),
        None,
    )
}

pub fn make_window_info(
    frame: CGRect,
    sys_id: Option<WindowServerId>,
    title: &str,
    bundle_id: Option<&str>,
) -> WindowInfo {
    WindowInfo {
        is_standard: true,
        is_root: true,
        is_minimized: false,
        is_resizable: true,
        min_size: None,
        max_size: None,
        title: title.to_string(),
        frame,
        sys_id,
        bundle_id: bundle_id.map(str::to_string),
        path: None,
        ax_role: None,
        ax_subrole: None,
        is_modal: false,
    }
}

pub fn make_windows(count: usize) -> Vec<WindowInfo> { (1..=count).map(make_window).collect() }

pub struct Apps {
    tx: channels::Sender<Request>,
    rx: channels::Receiver<Request>,
    pub windows: BTreeMap<WindowId, TestWindowState>,
}

/// How a test app launches: its main window, whether it is frontmost, and whether the launch
/// reports its windows' window-server info.
#[derive(Debug, Clone, Copy)]
pub struct AppLaunch {
    pub main_window: Option<WindowId>,
    pub is_frontmost: bool,
    pub with_ws_info: bool,
}

impl AppLaunch {
    pub fn frontmost(main_window: Option<WindowId>) -> Self {
        Self {
            main_window,
            is_frontmost: true,
            with_ws_info: true,
        }
    }

    pub fn background(main_window: Option<WindowId>) -> Self {
        Self {
            main_window,
            is_frontmost: false,
            with_ws_info: true,
        }
    }

    pub fn without_ws_info(self) -> Self { Self { with_ws_info: false, ..self } }
}

#[derive(Default, PartialEq, Debug, Clone)]
pub struct TestWindowState {
    pub last_seen_txid: TransactionId,
    pub last_sent_txid: TransactionId,
    pub frame: CGRect,
}

impl Apps {
    pub fn new() -> Apps {
        let (tx, rx) = channels::channel();
        Apps {
            tx,
            rx,
            windows: BTreeMap::new(),
        }
    }

    pub fn make_app(&mut self, pid: pid_t, windows: Vec<WindowInfo>) -> Vec<Event> {
        let main_window = windows.first().map(|_| WindowId::new(pid, 1));
        self.make_app_with_opts(pid, windows, AppLaunch::background(main_window))
    }

    pub fn make_app_with_opts(
        &mut self,
        pid: pid_t,
        windows: Vec<WindowInfo>,
        launch: AppLaunch,
    ) -> Vec<Event> {
        let AppLaunch {
            main_window,
            is_frontmost,
            with_ws_info,
        } = launch;
        let windows: Vec<WindowInfo> = windows
            .into_iter()
            .enumerate()
            .map(|(idx, mut info)| {
                // Keep synthetic window-server ids unique across apps so tests
                // exercise the same invariants as production.
                info.sys_id = Some(WindowServerId::new(
                    (pid as u32).saturating_mul(10_000) + idx as u32 + 1,
                ));
                info
            })
            .collect();

        for (id, info) in (1..).map(|idx| WindowId::new(pid, idx)).zip(&windows) {
            self.windows.insert(id, TestWindowState {
                frame: info.frame,
                ..Default::default()
            });
        }
        let handle = AppThreadHandle::from_sender(self.tx.clone());
        vec![Event::ApplicationLaunched {
            pid,
            info: AppInfo {
                bundle_id: Some(format!("com.testapp{pid}")),
                localized_name: Some(format!("TestApp{pid}")),
            },
            handle,
            is_frontmost,
            main_window,
            window_server_info: if with_ws_info {
                windows
                    .iter()
                    .map(|info| WindowServerInfo {
                        pid,
                        id: info.sys_id.unwrap(),
                        layer: 0,
                        frame: info.frame,
                        min_frame: CGSize::ZERO,
                        max_frame: CGSize::ZERO,
                    })
                    .collect()
            } else {
                Default::default()
            },
            visible_windows: (1..).map(|idx| WindowId::new(pid, idx)).zip(windows).collect(),
        }]
    }

    pub fn requests(&mut self) -> Vec<Request> {
        let mut requests = Vec::new();
        while let Ok((_, req)) = self.rx.try_recv() {
            requests.push(req);
        }
        requests
    }

    pub fn simulate_until_quiet(&mut self, reactor: &mut Reactor) {
        let mut requests = self.requests();
        while !requests.is_empty() {
            for event in self.simulate_events_for_requests(requests) {
                reactor.handle_event(event);
            }
            requests = self.requests();
        }
    }

    pub fn make_app_and_settle(
        &mut self,
        reactor: &mut Reactor,
        pid: pid_t,
        windows: Vec<WindowInfo>,
    ) {
        reactor.handle_events(self.make_app(pid, windows));
        self.simulate_until_quiet(reactor);
    }

    pub fn make_app_and_settle_on_screen(
        &mut self,
        reactor: &mut Reactor,
        screen: CGRect,
        space: SpaceId,
        pid: pid_t,
        windows: Vec<WindowInfo>,
    ) {
        reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
        self.make_app_and_settle(reactor, pid, windows);
    }

    pub fn simulate_events(&mut self) -> Vec<Event> {
        let requests = self.requests();
        self.simulate_events_for_requests(requests)
    }

    pub fn simulate_events_for_requests(&mut self, requests: Vec<Request>) -> Vec<Event> {
        let mut events = vec![];
        let mut got_visible_windows = false;
        for request in requests {
            debug!(?request);
            match request {
                Request::Terminate => break,
                Request::WindowMaybeDestroyed(_) => {}
                Request::GetVisibleWindows => {
                    if got_visible_windows {
                        continue;
                    }
                    got_visible_windows = true;
                    let mut app_windows = BTreeMap::<pid_t, Vec<WindowId>>::new();
                    for &wid in self.windows.keys() {
                        app_windows.entry(wid.pid).or_default().push(wid);
                    }
                    for (pid, windows) in app_windows {
                        events.push(Event::WindowsDiscovered {
                            pid,
                            new: vec![],
                            known_visible: windows,
                        });
                    }
                }
                Request::ApplicationGloballyActivated(pid) => {
                    events.push(Event::ApplicationActivated(pid, Quiet::No));
                }
                Request::SetWindowFrame { window: wid, frame, txid, .. } => {
                    let window = self.windows.entry(wid).or_default();
                    window.last_seen_txid = txid;
                    let old_frame = window.frame;
                    window.frame = frame;
                    if !old_frame.same_as(frame) {
                        events.push(Event::WindowFrameChanged {
                            window: wid,
                            frame,
                            last_seen: Some(txid),
                            requested: Requested(true),
                            mouse_state: None,
                        });
                    }
                }
                Request::SetBatchWindowFrame(frames, txid, _) => {
                    for (wid, frame) in frames {
                        let window = self.windows.entry(wid).or_default();
                        window.last_seen_txid = txid;
                        let old_frame = window.frame;
                        window.frame = frame;
                        if !old_frame.same_as(frame) {
                            events.push(Event::WindowFrameChanged {
                                window: wid,
                                frame,
                                last_seen: Some(txid),
                                requested: Requested(true),
                                mouse_state: None,
                            });
                        }
                    }
                }
                Request::SetWorkspaceSwitchPositions(positions, txid, _) => {
                    for (wid, position) in positions {
                        let window = self.windows.entry(wid).or_default();
                        window.last_seen_txid = txid;
                        let old_frame = window.frame;
                        window.frame.origin = position;
                        if !old_frame.same_as(window.frame) {
                            events.push(Event::WindowFrameChanged {
                                window: wid,
                                frame: window.frame,
                                last_seen: Some(txid),
                                requested: Requested(true),
                                mouse_state: None,
                            });
                        }
                    }
                }
                Request::Raise { .. } => todo!(),
                Request::CloseWindow(..) => todo!(),
            }
        }
        debug!(?events);
        events
    }
}

pub fn test_context() -> (Apps, Reactor) { (Apps::new(), test_reactor()) }

pub fn test_context_with_workspace_count(count: usize) -> (Apps, Reactor) {
    let mut settings = crate::app::config::VirtualWorkspaceSettings::default();
    settings.workspace_names = crate::workspaces::settings::numbered_names(count);
    (Apps::new(), test_reactor_with_workspace_settings(&settings))
}
