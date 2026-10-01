//! The WM Controller handles major events like enabling and disabling the
//! window manager on certain spaces and launching app threads. It also
//! controls hotkey registration.

mod lower;

use std::path::PathBuf;

use dispatchr::queue;
use dispatchr::time::Time;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication};
use tracing::{debug, error, info, instrument, warn};

use crate::app::config::actor as config;
use crate::input::domain::binding::Action;
use crate::input::platform::gesture_tap;
use crate::windows::platform::app::NSRunningApplicationExt;
use rini_core::ids::pid_t;

pub type Sender = channels::Sender<WmEvent>;

type Receiver = channels::Receiver<WmEvent>;

use crate::app::channels;
use crate::app::reactor;
use crate::input::platform::input_tap as event_tap;
use crate::windows::domain::info::AppInfo;
use crate::windows::domain::transaction::WindowTxStore;
use rini_runloop::dispatch::DispatchExt;

#[derive(Debug)]
pub enum WmEvent {
    DiscoverRunningApps,
    AppEventsRegistered,
    AppLaunch(pid_t, AppInfo),
    AppGloballyActivated(pid_t),
    AppGloballyDeactivated(pid_t),
    AppTerminated(pid_t),
    /// Everything the displays context reports; forwarded to the reactor as is.
    Displays(crate::displays::event::Event),
    /// Everything the input context reports: a bound action is lowered here, pointer events go to
    /// the reactor.
    Input(crate::input::event::Event),
    PowerStateChanged(bool),
    KeyboardLayoutChanged,
    /// The system clock was set, or the time zone changed.
    ClockChanged,
    ConfigUpdated(crate::app::config::Config),
}

impl From<crate::input::event::Event> for WmEvent {
    fn from(event: crate::input::event::Event) -> Self {
        WmEvent::Input(event)
    }
}

impl From<crate::displays::event::Event> for WmEvent {
    fn from(event: crate::displays::event::Event) -> Self {
        WmEvent::Displays(event)
    }
}

impl From<crate::windows::platform::lifecycle::AppLifecycle> for WmEvent {
    fn from(event: crate::windows::platform::lifecycle::AppLifecycle) -> Self {
        use crate::windows::platform::lifecycle::AppLifecycle as L;
        match event {
            L::Launched(pid, info) => WmEvent::AppLaunch(pid, info),
            L::FrontSwitched(pid) => WmEvent::AppGloballyActivated(pid),
            L::Terminated(pid) => WmEvent::AppTerminated(pid),
        }
    }
}

pub struct Config {
    pub restore_file: PathBuf,
    pub config: crate::app::config::Config,
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
    ) -> (Self, channels::Sender<WmEvent>) {
        let (sender, receiver) = channels::channel();
        crate::windows::platform::app::set_application_callback({
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

        use self::WmEvent::*;

        match event {
            Displays(event) => self.events_tx.send(Event::from(event)),
            Input(crate::input::event::Event::Action(action)) => self.carry_out(action),
            Input(crate::input::event::Event::Command(command)) => {
                self.events_tx.send(reactor::Event::Command(command))
            }
            Input(crate::input::event::Event::Switch(signal)) => {
                self.events_tx.send(Event::Switch(signal))
            }
            Input(crate::input::event::Event::MouseUp) => self.events_tx.send(Event::MouseUp),
            Input(crate::input::event::Event::PointerEnteredWindow(window)) => {
                self.events_tx.send(Event::MouseMoved(window))
            }
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
                for (pid, info) in crate::windows::platform::app::running_apps(None) {
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
                crate::windows::platform::app::remove_application_observer(pid);
                self.events_tx.send(Event::ApplicationTerminated(pid));
            }
            ConfigUpdated(new_cfg) => {
                let old = &self.config.config;
                let binds_changed = (&old.binds, &old.switcher_binds, old.settings.mod_key)
                    != (&new_cfg.binds, &new_cfg.switcher_binds, new_cfg.settings.mod_key);

                self.config.config = new_cfg;

                let input_settings =
                    crate::input::settings::InputSettings::from(&self.config.config);
                _ = self
                    .event_tap_tx
                    .send(event_tap::Request::SettingsUpdated(input_settings.clone()));
                if let Some(tx) = &self.gesture_tap_tx {
                    tx.send(gesture_tap::GestureRequest::SettingsUpdated(input_settings));
                }

                if !self.hotkeys_installed {
                    debug!(
                        "hotkeys not yet installed; deferring hotkey update until AppEventsRegistered"
                    );
                    return;
                }

                if binds_changed {
                    debug!("hotkey bindings changed; reloading hotkeys");
                    self.register_hotkeys();
                } else {
                    debug!("hotkey bindings unchanged; skipping reload");
                }
            }
            PowerStateChanged(is_low_power_mode) => {
                info!("Power state changed: low power mode = {}", is_low_power_mode);
                _ = self.event_tap_tx.send(event_tap::Request::SetLowPowerMode(is_low_power_mode));
            }
            KeyboardLayoutChanged => {
                _ = self.event_tap_tx.send(event_tap::Request::KeyboardLayoutChanged);
            }
            ClockChanged => self.events_tx.send(Event::ClockChanged),
        }
    }

    /// Every bound action is a translation; `lower` is that translation and is tested on its own.
    /// What is left here is the two things only the controller can carry out.
    fn carry_out(&self, action: Action) {
        match lower::lower(action, &self.config.config.virtual_workspaces.workspace_names) {
            lower::Lowered::Command(cmd) => self.events_tx.send(reactor::Event::Command(cmd)),
            lower::Lowered::Spawn(argv) => crate::app::startup::spawn(argv),
            lower::Lowered::ReloadConfig => self.reload_config(),
            lower::Lowered::UnknownWorkspace(name) => {
                warn!(%name, "a binding asked for a workspace that is not configured; ignoring");
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
            crate::windows::platform::app::ensure_activation_policy_observer(pid, info.clone());
            debug!(
                pid = ?pid,
                bundle = ?info.bundle_id,
                "App not yet regular; deferring spawn until activation policy changes"
            );

            if running_app.activationPolicy() == NSApplicationActivationPolicy::Regular {
                crate::windows::platform::app::remove_activation_policy_observer(pid);
            } else {
                return;
            }
        }

        if !running_app.isFinishedLaunching() {
            crate::windows::platform::app::ensure_finished_launching_observer(pid, info.clone());
            debug!(
                pid = ?pid,
                bundle = ?info.bundle_id,
                "App has not finished launching; deferring spawn until finished"
            );

            if running_app.isFinishedLaunching() {
                crate::windows::platform::app::remove_finished_launching_observer(pid);
            } else {
                return;
            }
        }

        crate::windows::platform::app_actor::spawn_app_thread(
            pid,
            info,
            Box::new(self.events_tx.clone()),
            self.window_tx_store.clone(),
        );
    }

    fn register_hotkeys(&mut self) {
        debug!("register_hotkeys");
        let config = &self.config.config;
        let mod_key = config.settings.mod_key;
        let resolve = |binds: &[crate::input::domain::binding::Bind]| {
            binds
                .iter()
                .map(|bind| crate::input::domain::binding::Bind {
                    key: bind.resolved_key(mod_key),
                    ..bind.clone()
                })
                .collect::<Vec<_>>()
        };
        let binds = resolve(&config.binds);
        let switcher_binds = resolve(&config.switcher_binds);
        // Derived from the same bindings rather than configured separately, so the chord that opens a
        // switch and the modifier that holds it open cannot disagree.
        let switch_keys =
            crate::switcher::domain::trigger::switch_keys_from_bindings(&switcher_binds, |spec| {
                let hotkey =
                    <crate::input::domain::key::Hotkey as std::str::FromStr>::from_str(spec)
                        .ok()?;
                Some((hotkey.modifiers, hotkey.key_code))
            });
        _ = self.event_tap_tx.send(event_tap::Request::SetSwitchKeys(switch_keys));
        _ = self
            .event_tap_tx
            .send(event_tap::Request::SetHotkeys(binds.into_iter().chain(switcher_binds).collect()));
    }

    fn reload_config(&self) {
        let (response, _fut) = r#continue::continuation();
        let msg = config::Event::ApplyConfig {
            cmd: crate::app::config::ConfigCommand::ReloadConfig,
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

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_change_is_passed_to_the_reactor() {
        let (config_tx, _config_rx) = channels::channel();
        let (events_tx, mut events_rx) = channels::channel();
        let (event_tap_tx, _event_tap_rx) = channels::channel();
        let config = Config {
            restore_file: PathBuf::new(),
            config: crate::app::config::Config::default(),
        };
        let (mut controller, _sender) =
            WmController::new(config, config_tx, events_tx, event_tap_tx, None, None);
        controller.handle_event(WmEvent::ClockChanged);
        assert!(matches!(
            events_rx.try_recv(),
            Ok((_, reactor::Event::ClockChanged))
        ));
    }
}
