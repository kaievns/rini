//! Starting the thread: refusing a process rini does not manage, subscribing to the application
//! while it launches, and announcing it with the windows it already has.

use std::thread;
use std::time::{Duration, Instant};

use objc2_app_kit::NSRunningApplication;
use objc2_application_services::AXError;
use rini_core::ids::pid_t;
use rini_runloop::channel as channels;
use rini_runloop::executor::Executor;
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, info, instrument, trace};

use super::State;
use super::subscriptions::APP_NOTIFICATIONS;
use crate::windows::domain::ax_events::{decode_notification_data, encode_notification_data};
use crate::windows::domain::info::AppInfo;
use crate::windows::domain::request::AppThreadHandle;
use crate::windows::domain::transaction::WindowTxStore;
use crate::windows::event::{Event, EventSink};
use crate::windows::platform::app::NSRunningApplicationExt;
use crate::windows::platform::ax::ask_again::AskAgain;
use crate::windows::platform::ax::element::{AXUIElement, Error as AxError};
use crate::windows::platform::ax::observer::Observer;
use crate::windows::platform::ax::world::{AxWorld, MacAx};
use crate::windows::platform::process::ProcessInfo;

/// How long a launching application gets to answer Accessibility before its thread gives up.
const LAUNCH_PATIENCE: Duration = Duration::from_secs(60);

/// `gave_up` runs when the thread ends without taking the application on, so the next sighting can try
/// again. It does not run for a process rini refuses, nor after the thread has taken the app on.
pub fn spawn_app_thread(
    pid: pid_t,
    info: AppInfo,
    events_tx: Box<dyn EventSink>,
    tx_store: Option<WindowTxStore>,
    gave_up: Box<dyn FnOnce() + Send>,
) {
    thread::Builder::new()
        .name(format!("{}({pid})", info.bundle_id.as_deref().unwrap_or("")))
        .spawn(move || {
            if app_thread_main(pid, info, events_tx, tx_store) == Outcome::GaveUp {
                gave_up();
            }
        })
        .unwrap();
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    TakenOn,
    Refused,
    GaveUp,
}

fn app_thread_main(
    pid: pid_t,
    info: AppInfo,
    events_tx: Box<dyn EventSink>,
    tx_store: Option<WindowTxStore>,
) -> Outcome {
    let app = AXUIElement::application(pid);
    let Some(running_app) = NSRunningApplication::with_process_id(pid) else {
        info!(?pid, "Making NSRunningApplication failed; exiting app thread");
        return Outcome::GaveUp;
    };

    let bundle_id = running_app.bundleIdentifier();

    let Ok(process_info) = ProcessInfo::for_pid(pid) else {
        info!(?pid, ?bundle_id, "Could not get ProcessInfo; exiting app thread");
        return Outcome::GaveUp;
    };
    if process_info.is_xpc {
        // XPC processes are not supposed to have windows so at best they are
        // extra work and noise. Worse, Apple's QuickLookUIService reports
        // having standard windows (these seem to be for Finder previews), but
        // they are non-standard and unmanageable.
        debug!(?pid, ?bundle_id, "Filtering out XPC process");
        return Outcome::Refused;
    }

    let Ok(observer) = Observer::new(pid) else {
        info!(?pid, ?bundle_id, "Making observer failed; exiting app thread");
        return Outcome::GaveUp;
    };
    let (notifications_tx, notifications_rx) = channels::channel();
    let observer = observer.install(move |elem, data| {
        if let Some((notif, wid)) = decode_notification_data(pid, data) {
            notifications_tx.send((elem, notif, wid));
        }
    });

    let (raises_tx, raises_rx) = channels::channel();
    let mut info = info;
    if info.bundle_id.is_none() {
        info.bundle_id = bundle_id.as_deref().map(ToString::to_string);
    }
    if info.localized_name.is_none() {
        info.localized_name = running_app.localizedName().as_deref().map(ToString::to_string);
    }

    let mut state = State {
        pid,
        bundle_id: info.bundle_id.clone(),
        ax: MacAx::new(pid, app.clone(), running_app.clone(), observer),
        events_tx,
        windows: HashMap::default(),
        elem_to_wid: HashMap::default(),
        last_window_idx: 0,
        main_window: None,
        last_activated: None,
        pending_activation_quiet: None,
        is_hidden: false,
        is_frontmost: false,
        raises_tx,
        tx_store,
    };

    let (requests_tx, requests_rx) = channels::channel();
    if !state.init(AppThreadHandle::from_sender(requests_tx), info, LAUNCH_PATIENCE) {
        info!(
            ?pid,
            ?bundle_id,
            "Accessibility did not answer while the application launched; giving up until it is seen again"
        );
        return Outcome::GaveUp;
    }
    Executor::run(state.run(requests_rx, notifications_rx, raises_rx));
    Outcome::TakenOn
}

impl<W: AxWorld> State<W> {
    #[instrument(skip_all, fields(?info))]
    #[must_use]
    fn init(&mut self, handle: AppThreadHandle, info: AppInfo, patience: Duration) -> bool {
        if !self.watch_application(patience) {
            return false;
        }

        let initial_window_elements = self.ax.windows(&self.ax.app()).unwrap_or_default();
        let server_info_by_id = self.visible_window_server_info_map(&initial_window_elements);

        let window_count = initial_window_elements.len();
        self.windows.reserve(window_count);
        self.elem_to_wid.reserve(window_count);
        let mut windows = Vec::with_capacity(window_count);
        let mut window_server_info = Vec::with_capacity(window_count);

        for elem in initial_window_elements {
            let wsid = self.ax.window_server_id(&elem);
            let hint = wsid.and_then(|id| server_info_by_id.get(&id).copied());
            if let Some(info) = hint {
                window_server_info.push(info);
            }
            if !Self::has_visible_cg_peer(wsid, hint) {
                trace!(pid = ?self.pid, ?wsid, "Ignoring AX window without a visible CG window");
                continue;
            }
            let Some((info, wid, _)) = self.register_window(elem, hint) else {
                continue;
            };
            windows.push((wid, info));
        }

        self.main_window = self.ax.main_window(&self.ax.app()).ok().and_then(|w| self.id(&w).ok());
        self.is_frontmost = self.ax.frontmost(&self.ax.app()).unwrap_or(false);

        self.events_tx.send(Event::ApplicationLaunched {
            pid: self.pid,
            handle,
            info,
            is_frontmost: self.is_frontmost,
            main_window: self.main_window,
            visible_windows: windows,
            window_server_info,
        });

        true
    }

    /// Subscribes to every application-level notification, asking a busy application again until
    /// `patience` runs out; false when it does.
    fn watch_application(&self, patience: Duration) -> bool {
        let mut again = AskAgain::new(Instant::now(), patience);
        let mut sleep = || again.next_wait(Instant::now()).map(thread::sleep).is_some();
        for &(kind, notif) in APP_NOTIFICATIONS {
            // App-level notifications are not tied to a specific window, but the
            // observer callback still recovers the notification kind by decoding
            // the refcon hint (see `decode_notification_data`). Registering with the
            // plain `add_notification` would attach a zero hint, which decodes to an
            // invalid tag and causes the notification to be silently dropped - so
            // encode the kind here just like the per-window registrations do.
            let data = encode_notification_data(kind, None);
            loop {
                match self.ax.watch(&self.ax.app(), notif, data) {
                    Ok(()) => break,
                    Err(AxError::Ax(AXError::NotificationAlreadyRegistered)) => {
                        debug!(
                            pid = ?self.pid,
                            "Watching app for {notif} was already registered; continuing"
                        );
                        break;
                    }
                    Err(err) => {
                        debug!(pid = ?self.pid, ?err, "Watching app for {notif} failed");
                        if !sleep() {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{PID, state_with};
    use super::*;
    use crate::windows::platform::ax::world::FakeAx;

    fn launch(state: &mut State<FakeAx>, patience: Duration) -> bool {
        let (requests_tx, _requests_rx) = channels::channel();
        let info = AppInfo {
            bundle_id: Some("com.electron.dockerdesktop".to_owned()),
            localized_name: Some("Docker Desktop".to_owned()),
        };
        state.init(AppThreadHandle::from_sender(requests_tx), info, patience)
    }

    /// A subscription that fails while the application launches is asked again until
    /// `LAUNCH_PATIENCE` runs out, not given up on at the first failure.
    #[test]
    fn an_application_too_busy_launching_to_answer_is_asked_again() {
        let ax = FakeAx { app: 1, ..FakeAx::default() };
        ax.busy_watches.set(2);
        let (mut state, sink) = state_with(ax);

        assert!(launch(&mut state, LAUNCH_PATIENCE), "taken on");

        let watched = state.ax.watched.borrow();
        for &(_, notif) in APP_NOTIFICATIONS {
            assert!(
                watched.iter().any(|(elem, n)| *elem == 1 && *n == notif),
                "{notif} was never subscribed"
            );
        }
        assert!(
            sink.0
                .lock()
                .iter()
                .any(|event| matches!(event, Event::ApplicationLaunched { pid: PID, .. })),
            "and the reactor hears of it"
        );
    }

    #[test]
    fn an_application_that_never_answers_is_given_up_on_once_patience_runs_out() {
        let ax = FakeAx { app: 1, ..FakeAx::default() };
        ax.busy_watches.set(u32::MAX);
        let (mut state, sink) = state_with(ax);

        assert!(!launch(&mut state, Duration::ZERO), "given up on");
        assert!(sink.0.lock().is_empty(), "and the reactor hears nothing");
    }
}
