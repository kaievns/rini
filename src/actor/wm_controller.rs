//! The WM Controller handles major events like enabling and disabling the
//! window manager on certain spaces and launching app threads. It also
//! controls hotkey registration.

use std::path::PathBuf;

use dispatchr::queue;
use dispatchr::time::Time;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication};
use serde_json;
use tracing::{debug, error, info, instrument, warn};

use crate::actor::gesture_tap;
use rini_config::actor as config;
pub use rini_config::{ExecCmd, WmCmd, WmCommand};
use rini_config::WorkspaceSelector;
use rini_windows::app::NSRunningApplicationExt;
use rini_windows::ids::pid_t;

pub type Sender = actor::Sender<WmEvent>;

type Receiver = actor::Receiver<WmEvent>;

use self::WmCmd::*;
use rini_windows::app::AppInfo;
use crate::actor::{self, event_tap, reactor};
use rini_windows::transaction::WindowTxStore;
use rini_runloop::dispatch::DispatchExt;

use crate::layout_engine as layout;

#[derive(Debug)]
pub enum WmEvent {
    DiscoverRunningApps,
    AppEventsRegistered,
    AppLaunch(pid_t, AppInfo),
    AppGloballyActivated(pid_t),
    AppGloballyDeactivated(pid_t),
    AppTerminated(pid_t),
    /// Everything the displays context reports. The topology snapshot is also fanned out to the
    /// event tap; the rest goes to the reactor as is.
    Displays(rini_displays::event::Event),
    PowerStateChanged(bool),
    KeyboardLayoutChanged,
    ConfigUpdated(rini_config::Config),
    Command(WmCommand),
}

impl From<rini_displays::event::Event> for WmEvent {
    fn from(event: rini_displays::event::Event) -> Self {
        WmEvent::Displays(event)
    }
}

impl From<rini_windows::lifecycle::AppLifecycle> for WmEvent {
    fn from(event: rini_windows::lifecycle::AppLifecycle) -> Self {
        use rini_windows::lifecycle::AppLifecycle as L;
        match event {
            L::Launched(pid, info) => WmEvent::AppLaunch(pid, info),
            L::FrontSwitched(pid) => WmEvent::AppGloballyActivated(pid),
            L::Terminated(pid) => WmEvent::AppTerminated(pid),
        }
    }
}

pub struct Config {
    pub restore_file: PathBuf,
    pub config: rini_config::Config,
}

pub struct WmController {
    config: Config,
    config_tx: config::Sender,
    events_tx: reactor::Sender,
    event_tap_tx: event_tap::Sender,
    gesture_tap_tx: Option<gesture_tap::Sender>,
    window_tx_store: Option<WindowTxStore>,
    receiver: Receiver,
    sender: Sender,
    hotkeys_installed: bool,
}

impl WmController {
    pub fn new(
        config: Config,
        config_tx: config::Sender,
        events_tx: reactor::Sender,
        event_tap_tx: event_tap::Sender,
        gesture_tap_tx: Option<gesture_tap::Sender>,
        window_tx_store: Option<WindowTxStore>,
    ) -> (Self, actor::Sender<WmEvent>) {
        let (sender, receiver) = actor::channel();
        rini_windows::app::set_application_callback({
            let sender = sender.clone();
            move |pid, info| sender.send(WmEvent::AppLaunch(pid, info))
        });
        let this = Self {
            config,
            config_tx,
            events_tx,
            event_tap_tx,
            gesture_tap_tx,
            window_tx_store,
            receiver,
            sender: sender.clone(),
            hotkeys_installed: false,
        };
        (this, sender)
    }

    pub async fn run(mut self) {
        while let Some((span, event)) = self.receiver.recv().await {
            let _guard = span.enter();
            self.handle_event(event);
        }
    }

    #[instrument(name = "wm_controller::handle_event", skip(self))]
    pub fn handle_event(&mut self, event: WmEvent) {
        debug!("handle_event");
        use reactor::Event;

        use self::WmCommand::*;
        use self::WmEvent::*;


        match event {
            Displays(rini_displays::event::Event::SpaceStateUpdated(space_state, converter)) => {
                _ = self.event_tap_tx.send(event_tap::Request::SpaceStateUpdated(
                    space_state.clone(),
                    converter,
                ));
                self.events_tx.send(Event::SpaceStateChanged(space_state));
            }
            Displays(event) => self.events_tx.send(Event::from(event)),
            AppEventsRegistered => {
                _ = self.event_tap_tx.send(event_tap::Request::SetEventProcessing(false));

                if !self.hotkeys_installed {
                    self.register_hotkeys();
                    self.hotkeys_installed = true;
                }

                let sender = self.sender.clone();
                let event_tap_tx = self.event_tap_tx.clone();
                queue::main().after_f_s(
                    Time::new_after(Time::NOW, 250 * 1000000),
                    (sender, WmEvent::DiscoverRunningApps),
                    |(sender, event)| sender.send(event),
                );

                queue::main().after_f_s(
                    Time::new_after(Time::NOW, (250 + 350) * 1000000),
                    (event_tap_tx, event_tap::Request::SetEventProcessing(true)),
                    |(sender, event)| sender.send(event),
                );
            }
            DiscoverRunningApps => {
                for (pid, info) in rini_windows::app::running_apps(None) {
                    self.new_app(pid, info);
                }
            }
            AppLaunch(pid, info) => {
                self.new_app(pid, info);
            }
            AppGloballyActivated(pid) => {
                _ = self.event_tap_tx.send(event_tap::Request::EnforceHidden);
                self.events_tx.send(Event::ApplicationGloballyActivated(pid));
            }
            AppGloballyDeactivated(pid) => {
                self.events_tx.send(Event::ApplicationGloballyDeactivated(pid));
            }
            AppTerminated(pid) => {
                rini_windows::app::remove_application_observer(pid);
                self.events_tx.send(Event::ApplicationTerminated(pid));
            }
            ConfigUpdated(new_cfg) => {
                let old_keys_ser = serde_json::to_string(&self.config.config.keys).ok();

                self.config.config = new_cfg;

                _ = self
                    .event_tap_tx
                    .send(event_tap::Request::ConfigUpdated(self.config.config.clone()));
                if let Some(tx) = &self.gesture_tap_tx {
                    tx.send(gesture_tap::GestureRequest::ConfigUpdated(
                        self.config.config.clone(),
                    ));
                }

                if !self.hotkeys_installed {
                    debug!(
                        "hotkeys not yet installed; deferring hotkey update until AppEventsRegistered"
                    );
                    return;
                }

                if let Some(old_ser) = old_keys_ser {
                    if serde_json::to_string(&self.config.config.keys).ok().as_deref()
                        != Some(&old_ser)
                    {
                        debug!("hotkey bindings changed; reloading hotkeys");
                        self.register_hotkeys();
                    } else {
                        debug!("hotkey bindings unchanged; skipping reload");
                    }
                } else {
                    debug!("could not compare hotkey bindings; reloading hotkeys");
                    self.register_hotkeys();
                }
            }
            PowerStateChanged(is_low_power_mode) => {
                info!("Power state changed: low power mode = {}", is_low_power_mode);
                _ = self.event_tap_tx.send(event_tap::Request::SetLowPowerMode(is_low_power_mode));
            }
            KeyboardLayoutChanged => {
                _ = self.event_tap_tx.send(event_tap::Request::KeyboardLayoutChanged);
            }
            Command(Wm(ReloadConfig)) => self.reload_config(),
            Command(Wm(crate::actor::wm_controller::WmCmd::ToggleSpaceActivated)) => {
                self.events_tx.send(reactor::Event::Command(reactor::Command::Reactor(
                    reactor::ReactorCommand::ToggleSpaceActivated,
                )));
            }
            Command(Wm(NextWorkspace)) => {
                self.events_tx.send(reactor::Event::Command(reactor::Command::Layout(
                    layout::LayoutCommand::NextWorkspace(None),
                )));
            }
            Command(Wm(PrevWorkspace)) => {
                self.events_tx.send(reactor::Event::Command(reactor::Command::Layout(
                    layout::LayoutCommand::PrevWorkspace(None),
                )));
            }
            Command(Wm(SwitchToWorkspace(ws_sel))) => {
                let maybe_index: Option<usize> = match &ws_sel {
                    WorkspaceSelector::Index(i) => Some(*i),
                    WorkspaceSelector::Name(name) => self
                        .config
                        .config
                        .virtual_workspaces
                        .workspace_names
                        .iter()
                        .position(|n| n == name),
                };

                if let Some(workspace_index) = maybe_index {
                    self.events_tx.send(reactor::Event::Command(reactor::Command::Layout(
                        layout::LayoutCommand::SwitchToWorkspace(workspace_index),
                    )));
                } else {
                    tracing::warn!(
                        "Hotkey requested switch to workspace {:?} but it could not be resolved; ignoring",
                        ws_sel
                    );
                }
            }
            Command(Wm(MoveWindowToWorkspace(workspace))) => {
                self.events_tx.send(reactor::Event::Command(reactor::Command::Layout(
                    layout::LayoutCommand::MoveWindowToWorkspace {
                        workspace,
                        follow: false,
                        window_id: None,
                    },
                )));
            }
            Command(Wm(CreateWorkspace)) => {
                self.events_tx.send(reactor::Event::Command(reactor::Command::Layout(
                    layout::LayoutCommand::CreateWorkspace,
                )));
            }
            Command(Wm(SwitchToLastWorkspace)) => {
                self.events_tx.send(reactor::Event::Command(reactor::Command::Layout(
                    layout::LayoutCommand::SwitchToLastWorkspace,
                )));
            }
            Command(Wm(CloseWindow)) => {
                self.events_tx.send(reactor::Event::Command(reactor::Command::Reactor(
                    reactor::ReactorCommand::CloseWindow { window_server_id: None },
                )));
            }
            Command(Wm(CycleAppWindows)) => {
                self.events_tx.send(reactor::Event::Command(reactor::Command::Reactor(
                    reactor::ReactorCommand::CycleAppWindows { backward: false },
                )));
            }
            Command(Wm(CycleAppWindowsBackward)) => {
                self.events_tx.send(reactor::Event::Command(reactor::Command::Reactor(
                    reactor::ReactorCommand::CycleAppWindows { backward: true },
                )));
            }
            Command(Wm(Exec(cmd))) => {
                self.exec_cmd(cmd);
            }
            Command(ReactorCommand(cmd)) => {
                self.events_tx.send(reactor::Event::Command(cmd));
            }
        }
    }

    fn new_app(&mut self, pid: pid_t, info: AppInfo) {
        let Some(running_app) = NSRunningApplication::with_process_id(pid) else {
            debug!(pid = ?pid, "Failed to resolve NSRunningApplication for new app");
            return;
        };

        if running_app.activationPolicy() != NSApplicationActivationPolicy::Regular
            && info.bundle_id.as_deref() != Some("com.apple.loginwindow")
        {
            rini_windows::app::ensure_activation_policy_observer(pid, info.clone());
            debug!(
                pid = ?pid,
                bundle = ?info.bundle_id,
                "App not yet regular; deferring spawn until activation policy changes"
            );

            if running_app.activationPolicy() == NSApplicationActivationPolicy::Regular {
                rini_windows::app::remove_activation_policy_observer(pid);
            } else {
                return;
            }
        }

        if !running_app.isFinishedLaunching() {
            rini_windows::app::ensure_finished_launching_observer(pid, info.clone());
            debug!(
                pid = ?pid,
                bundle = ?info.bundle_id,
                "App has not finished launching; deferring spawn until finished"
            );

            if running_app.isFinishedLaunching() {
                rini_windows::app::remove_finished_launching_observer(pid);
            } else {
                return;
            }
        }

        rini_windows::app_actor::spawn_app_thread(
            pid,
            info,
            Box::new(self.events_tx.clone()),
            self.window_tx_store.clone(),
        );
    }

    fn register_hotkeys(&mut self) {
        debug!("register_hotkeys");
        let bindings: Vec<(String, WmCommand)> =
            self.config.config.key_specs.iter().cloned().collect();
        _ = self.event_tap_tx.send(event_tap::Request::SetHotkeys(bindings));
    }

    fn reload_config(&self) {
        let (response, _fut) = r#continue::continuation();
        let msg = config::Event::ApplyConfig {
            cmd: rini_config::ConfigCommand::ReloadConfig,
            response,
        };
        if let Err(e) = self.config_tx.try_send(msg) {
            let error_message = e.to_string();
            let tokio::sync::mpsc::error::SendError((_span, msg)) = e;
            match msg {
                config::Event::ApplyConfig { response, .. } => std::mem::forget(response),
                config::Event::QueryConfig(response) => std::mem::forget(response),
            }
            error!("Failed to request config reload: {error_message}");
        }
    }

    fn exec_cmd(&self, cmd_args: ExecCmd) {
        std::thread::spawn(move || {
            let cmd_args = cmd_args.as_array();
            let [cmd, args @ ..] = &*cmd_args else {
                error!("Empty argument list passed to exec");
                return;
            };
            let output = std::process::Command::new(cmd).args(args).output();
            let output = match output {
                Ok(o) => o,
                Err(e) => {
                    error!("Failed to execute command {cmd:?}: {e:?}");
                    return;
                }
            };
            if !output.status.success() {
                error!(
                    "Exec command exited with status {}: {cmd:?} {args:?}",
                    output.status
                );
                error!("stdout: {}", String::from_utf8_lossy(&*output.stdout));
                error!("stderr: {}", String::from_utf8_lossy(&*output.stderr));
            }
        });
    }
}

