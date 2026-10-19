//! The controller launches app threads, registers the hotkeys and lowers what they fire, and
//! re-registers them when the config is reloaded.

pub(in crate::app) mod lower;

use dispatchr::queue;
use dispatchr::time::Time;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication};
use rini_core::ids::pid_t;
use tracing::{debug, error, info, instrument, warn};

use crate::app::config;
use crate::input::domain::binding::{Action, Bind};
use crate::input::domain::key::{Hotkey, ModKey};
use crate::input::domain::switch_session::{SwitchKeys, overview_keys};
use crate::windows::platform::app::NSRunningApplicationExt;

pub type Sender = channels::Sender<WmEvent>;

type Receiver = channels::Receiver<WmEvent>;

use rini_runloop::dispatch::DispatchExt;

use crate::app::{channels, reactor};
use crate::input::platform::input_tap;
use crate::windows::domain::app_threads::{AppThreads, Next, Readiness};
use crate::windows::domain::info::AppInfo;
use crate::windows::domain::transaction::WindowTxStore;

#[derive(Debug)]
pub enum WmEvent {
    DiscoverRunningApps,
    AppEventsRegistered,
    AppLaunch(pid_t, AppInfo),
    AppGloballyActivated(pid_t),
    AppGloballyDeactivated(pid_t),
    AppTerminated(pid_t),
    /// The application's thread ended without taking it on.
    AppThreadGaveUp(pid_t),
    /// Everything the input context reports: a bound action is lowered here, pointer events go to
    /// the reactor.
    Input(crate::input::event::Event),
    PowerStateChanged(bool),
    KeyboardLayoutChanged,
    /// The system clock was set, or the time zone changed.
    ClockChanged,
    ConfigUpdated(Box<crate::app::config::Config>),
}

impl From<crate::input::event::Event> for WmEvent {
    fn from(event: crate::input::event::Event) -> Self { WmEvent::Input(event) }
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

pub struct Controller {
    config: crate::app::config::Config,
    config_tx: config::actor::Sender,
    events_tx: reactor::Sender,
    event_tap_tx: input_tap::Sender,
    window_tx_store: Option<WindowTxStore>,
    receiver: Receiver,
    sender: Sender,
    hotkeys_installed: bool,
    app_threads: AppThreads,
}

impl Controller {
    pub fn new(
        config: crate::app::config::Config,
        config_tx: config::actor::Sender,
        events_tx: reactor::Sender,
        event_tap_tx: input_tap::Sender,
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
            window_tx_store,
            receiver,
            sender: sender.clone(),
            hotkeys_installed: false,
            app_threads: AppThreads::default(),
        };
        (this, sender)
    }

    pub async fn run(mut self) {
        while let Some((span, event)) = self.receiver.recv().await {
            let _guard = span.enter();
            self.handle_event(event);
        }
    }

    #[instrument(name = "controller::handle_event", skip(self))]
    pub fn handle_event(&mut self, event: WmEvent) {
        debug!("handle_event");
        use reactor::Event;

        use self::WmEvent::*;

        match event {
            Input(crate::input::event::Event::Action(action)) => self.carry_out(action),
            Input(crate::input::event::Event::Switch(signal)) => {
                self.events_tx.send(Event::Switch(signal))
            }
            Input(crate::input::event::Event::MouseUp) => self.events_tx.send(Event::MouseUp),
            Input(crate::input::event::Event::PointerEnteredWindow(window)) => {
                self.events_tx.send(Event::MouseMoved(window))
            }
            AppEventsRegistered => {
                self.event_tap_tx.send(input_tap::Request::SetEventProcessing(false));

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
                    (event_tap_tx, input_tap::Request::SetEventProcessing(true)),
                    |(sender, event)| sender.send(event),
                );
            }
            DiscoverRunningApps => {
                for (pid, info) in crate::windows::platform::app::running_apps() {
                    self.new_app(pid, info);
                }
                self.app_threads.discovered();
            }
            AppLaunch(pid, info) => {
                self.new_app(pid, info);
            }
            AppGloballyActivated(pid) => {
                self.events_tx.send(Event::ApplicationGloballyActivated(pid));
                if self.app_threads.look_on_activation(pid) {
                    self.app_activated_without_thread(pid);
                }
            }
            AppGloballyDeactivated(pid) => {
                self.events_tx.send(Event::ApplicationGloballyDeactivated(pid));
            }
            AppTerminated(pid) => {
                self.app_threads.ended(pid);
                crate::windows::platform::app::remove_application_observer(pid);
                self.events_tx.send(Event::ApplicationTerminated(pid));
            }
            AppThreadGaveUp(pid) => self.app_threads.ended(pid),
            ConfigUpdated(new_cfg) => {
                let binds_changed = Registration::of(&self.config) != Registration::of(&new_cfg);

                self.config = *new_cfg;

                let input_settings = crate::input::settings::InputSettings::from(&self.config);
                self.event_tap_tx.send(input_tap::Request::SettingsUpdated(input_settings));

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
                self.event_tap_tx.send(input_tap::Request::SetLowPowerMode(is_low_power_mode));
            }
            KeyboardLayoutChanged => {
                self.event_tap_tx.send(input_tap::Request::KeyboardLayoutChanged);
            }
            ClockChanged => self.events_tx.send(Event::ClockChanged),
        }
    }

    /// Every bound action is a translation; `lower` is that translation and is tested on its own.
    /// What is left here is the two things only the controller can carry out.
    fn carry_out(&self, action: Action) {
        match lower::lower(action, &self.config.virtual_workspaces.workspace_names) {
            lower::Lowered::Command(cmd) => self.events_tx.send(reactor::Event::Command(cmd)),
            lower::Lowered::Spawn(argv) => crate::app::startup::spawn(argv),
            lower::Lowered::ReloadConfig => self.reload_config(),
            lower::Lowered::UnknownWorkspace(name) => {
                warn!(%name, "a binding asked for a workspace that is not configured; ignoring");
            }
            lower::Lowered::Held => debug!("toggle-overview reached the hotkey table; ignoring"),
        }
    }

    fn new_app(&mut self, pid: pid_t, info: AppInfo) {
        let Some(running_app) = NSRunningApplication::with_process_id(pid) else {
            debug!(pid = ?pid, "Failed to resolve NSRunningApplication for new app");
            return;
        };
        self.app_seen(pid, info, &running_app);
    }

    fn app_activated_without_thread(&mut self, pid: pid_t) {
        let Some(running_app) = NSRunningApplication::with_process_id(pid) else {
            return;
        };
        let info = AppInfo::from(&*running_app);
        if self.app_seen(pid, info.clone(), &running_app) == Next::Spawn {
            info!(
                pid,
                bundle = ?info.bundle_id,
                "Taking on an application first seen ready when it was activated"
            );
        }
    }

    fn app_seen(&mut self, pid: pid_t, info: AppInfo, running_app: &NSRunningApplication) -> Next {
        let readiness = Readiness {
            regular: running_app.activationPolicy() == NSApplicationActivationPolicy::Regular,
            finished_launching: running_app.isFinishedLaunching(),
            login_window: info.bundle_id.as_deref() == Some("com.apple.loginwindow"),
        };
        let next = self.app_threads.next(pid, readiness);
        match next {
            Next::AlreadyRunning => {}
            Next::AwaitActivationPolicy => {
                debug!(
                    pid,
                    bundle = ?info.bundle_id,
                    "App not yet regular; waiting for its activation policy"
                );
                crate::windows::platform::app::ensure_activation_policy_observer(pid, info);
            }
            Next::AwaitFinishedLaunching => {
                debug!(
                    pid,
                    bundle = ?info.bundle_id,
                    "App has not finished launching; waiting for it"
                );
                crate::windows::platform::app::ensure_finished_launching_observer(pid, info);
            }
            Next::Spawn => {
                crate::windows::platform::app::remove_application_observer(pid);
                let sender = self.sender.clone();
                crate::windows::platform::app_actor::spawn_app_thread(
                    pid,
                    info,
                    Box::new(self.events_tx.clone()),
                    self.window_tx_store.clone(),
                    Box::new(move || sender.send(WmEvent::AppThreadGaveUp(pid))),
                );
            }
        }
        next
    }

    fn register_hotkeys(&mut self) {
        debug!("register_hotkeys");
        let (switch_keys, binds) = Registration::of(&self.config).for_tap();
        self.event_tap_tx.send(input_tap::Request::SetSwitchKeys(switch_keys));
        self.event_tap_tx.send(input_tap::Request::SetHotkeys(binds));
    }

    fn reload_config(&self) {
        let (response, _fut) = r#continue::continuation();
        let msg = config::actor::Event::ApplyConfig {
            cmd: crate::app::config::ConfigCommand::ReloadConfig,
            response,
        };
        if let Err(e) = self.config_tx.try_send(msg) {
            let error_message = e.to_string();
            let tokio::sync::mpsc::error::SendError((_span, msg)) = e;
            match msg {
                config::actor::Event::ApplyConfig { response, .. } => std::mem::forget(response),
                config::actor::Event::QueryConfig(response) => std::mem::forget(response),
            }
            error!("Failed to request config reload: {error_message}");
        }
    }
}

/// What the tap is given for a config's keys: every bind and switcher bind with `Mod` replaced by
/// `mod-key`, and `mod-key`, which the overview looks its keys up with. A config change
/// re-registers exactly when this changes.
#[derive(Debug, PartialEq)]
struct Registration {
    binds: Vec<Bind>,
    switcher_binds: Vec<Bind>,
    mod_key: ModKey,
}

impl Registration {
    fn of(config: &crate::app::config::Config) -> Self {
        let mod_key = config.settings.mod_key;
        let resolve = |binds: &[Bind]| {
            binds
                .iter()
                .map(|bind| Bind {
                    key: bind.resolved_key(mod_key),
                    ..bind.clone()
                })
                .collect()
        };
        Registration {
            binds: resolve(&config.binds),
            switcher_binds: resolve(&config.switcher_binds),
            mod_key,
        }
    }

    /// The keys each switcher and the overview answer to, and every bind. Derived from the same
    /// bindings rather than configured separately, so the chord that opens a switch or the overview
    /// and the modifier that holds it open cannot disagree.
    fn for_tap(self) -> (Vec<SwitchKeys>, Vec<Bind>) {
        let parse = |spec: &str| {
            let hotkey = <Hotkey as std::str::FromStr>::from_str(spec).ok()?;
            Some((hotkey.modifiers, hotkey.key_code))
        };
        let mut switch_keys = crate::switcher::domain::trigger::switch_keys_from_bindings(
            &self.switcher_binds,
            parse,
        );
        switch_keys.extend(overview_keys(&self.binds, self.mod_key, parse));
        (
            switch_keys,
            self.binds.into_iter().chain(self.switcher_binds).collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registration(text: &str) -> Registration {
        let config = crate::app::config::Config::parse("test.kdl", text)
            .unwrap_or_else(|error| panic!("{}", error.render()));
        Registration::of(&config)
    }

    fn keys(binds: &[Bind]) -> Vec<&str> { binds.iter().map(|bind| bind.key.as_str()).collect() }

    const BINDS: &str = concat!(
        "binds {\n    Mod+J { focus-column-left; }\n    Alt+K { close-window; }\n}\n",
        "recent-windows {\n    binds {\n        Mod+Tab { next-window; }\n    }\n}\n",
    );

    #[test]
    fn every_mod_bind_is_registered_on_the_mod_key_switcher_binds_too() {
        let ctrl = registration(&format!("input {{\n    mod-key \"Ctrl\"\n}}\n{BINDS}"));
        assert_eq!(keys(&ctrl.binds), ["Ctrl+J", "Alt+K"]);
        assert_eq!(keys(&ctrl.switcher_binds), ["Ctrl+Tab"]);
        for key in keys(&ctrl.binds).into_iter().chain(keys(&ctrl.switcher_binds)) {
            assert!(
                <crate::input::domain::key::Hotkey as std::str::FromStr>::from_str(key).is_ok(),
                "{key} would not register"
            );
        }
    }

    #[test]
    fn a_reload_that_only_changes_mod_key_re_registers() {
        let alt = registration(BINDS);
        let super_key = registration(&format!("input {{\n    mod-key \"Super\"\n}}\n{BINDS}"));
        assert_ne!(alt, super_key);
        assert_eq!(keys(&super_key.binds), ["Super+J", "Alt+K"]);
    }

    #[test]
    fn a_reload_that_leaves_every_key_alone_does_not_re_register() {
        assert_eq!(
            registration(BINDS),
            registration(&format!("layout {{\n    gaps 8\n}}\n{BINDS}"))
        );
        assert_eq!(
            registration("binds {\n    Mod+J { focus-column-left; }\n}\n"),
            registration("binds {\n    Alt+J { focus-column-left; }\n}\n"),
            "`Mod+J` under the default mod-key registers what `Alt+J` does"
        );
    }

    /// Kai's ask: Cmd+Caps, which his keyboard layer sends as Super+F19, bound to
    /// `toggle-overview` opens the overview held by Cmd, and J pressed with Cmd held runs `Mod+J`,
    /// Shift adding `Mod+Shift+J`. Bound beside `Mod+Slash`, each opens it held by its own modifier,
    /// under whichever modifier `mod-key` names.
    #[test]
    fn a_super_f19_bind_opens_the_overview_held_by_cmd_with_mod_binds_inside() {
        use std::str::FromStr;
        use std::time::Instant;

        use crate::input::domain::key::{KeyCode, modifier_mask_for_key};
        use crate::input::domain::router::{KeyRouter, Keystroke};
        use crate::input::domain::switch_session::{
            KeyEventKind, OverviewSignal, SessionId, Signal,
        };

        let binds = concat!(
            "binds {\n    Mod+J { focus-column-left; }\n    Mod+Shift+J { move-window-left; }\n",
            "    Mod+Slash { toggle-overview; }\n    Super+F19 { toggle-overview; }\n}\n",
        );
        let ctrl = "input {\n    mod-key \"Ctrl\"\n}\n";
        let mask = |key| modifier_mask_for_key(key).expect("a modifier");
        let shift = mask(KeyCode::ShiftLeft);
        for (input, trigger, modifier) in [
            ("", KeyCode::F19, KeyCode::MetaLeft),
            ("", KeyCode::Slash, KeyCode::AltLeft),
            (ctrl, KeyCode::F19, KeyCode::MetaLeft),
            (ctrl, KeyCode::Slash, KeyCode::ControlLeft),
        ] {
            let (switch_keys, binds) = registration(&format!("{input}{binds}")).for_tap();
            let mut router = KeyRouter::default();
            let unresolved = router.set_binds(binds, |spec| Hotkey::from_str(spec).ok());
            assert!(unresolved.is_empty(), "{unresolved:?}");
            assert_eq!(router.set_switch_keys(switch_keys), None);
            let hold = mask(modifier);
            let mut press = |kind, key, flags| {
                router.on_key(Keystroke {
                    kind,
                    key: Some(key),
                    flags,
                    repeat: false,
                    synthetic: false,
                    at: Instant::now(),
                })
            };
            press(KeyEventKind::FlagsChanged, modifier, hold);
            assert_eq!(
                press(KeyEventKind::Down, trigger, hold).signal,
                Some(Signal::Overview(OverviewSignal::Open(SessionId(1)))),
                "{input}{trigger:?}"
            );
            assert_eq!(
                press(KeyEventKind::Down, KeyCode::KeyJ, hold).action,
                Some(Action::FocusColumnLeft),
                "{input}{trigger:?}"
            );
            press(KeyEventKind::FlagsChanged, KeyCode::ShiftLeft, hold | shift);
            assert_eq!(
                press(KeyEventKind::Down, KeyCode::KeyJ, hold | shift).action,
                Some(Action::MoveWindowLeft),
                "{input}{trigger:?}"
            );
            press(KeyEventKind::FlagsChanged, KeyCode::ShiftLeft, hold);
            assert_eq!(
                press(KeyEventKind::FlagsChanged, modifier, 0).signal,
                Some(Signal::Overview(OverviewSignal::Close)),
                "{input}{trigger:?}: let go"
            );
        }
    }

    #[test]
    fn a_clock_change_is_passed_to_the_reactor() {
        let (config_tx, _config_rx) = channels::channel();
        let (events_tx, mut events_rx) = channels::channel();
        let (event_tap_tx, _event_tap_rx) = channels::channel();
        let (mut controller, _sender) = Controller::new(
            crate::app::config::Config::reference(),
            config_tx,
            events_tx,
            event_tap_tx,
            None,
        );
        controller.handle_event(WmEvent::ClockChanged);
        assert!(matches!(
            events_rx.try_recv(),
            Ok((_, reactor::Event::ClockChanged))
        ));
    }
}
