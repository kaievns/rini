//! Mission Control, observed through the Dock's Accessibility notifications. The application
//! stops trusting window positions while it is up.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2_app_kit::NSRunningApplication;
use objc2_foundation::ns_string;
use tracing::{error, info, instrument, warn};

use rini_windows::app::NSRunningApplicationExt;
use rini_windows::ax::element::AXUIElement;
use rini_windows::ax::observer::Observer;
use rini_windows::ids::pid_t;

use crate::event::{Event, EventSink};

const K_AX_EXPOSE_SHOW_ALL_WINDOWS: &str = "AXExposeShowAllWindows";
const K_AX_EXPOSE_SHOW_FRONT_WINDOWS: &str = "AXExposeShowFrontWindows";
const K_AX_EXPOSE_SHOW_DESKTOP: &str = "AXExposeShowDesktop";
const K_AX_EXPOSE_EXIT: &str = "AXExposeExit";

const NOTIFICATIONS: &[&str] = &[
    K_AX_EXPOSE_EXIT,
    K_AX_EXPOSE_SHOW_ALL_WINDOWS,
    K_AX_EXPOSE_SHOW_FRONT_WINDOWS,
    K_AX_EXPOSE_SHOW_DESKTOP,
];

pub struct NativeMissionControl {
    observer: Option<Observer>,
    app_elem: Option<AXUIElement>,
    active: Arc<AtomicBool>,
    events_tx: Arc<dyn EventSink>,
}

struct State {
    events_tx: Arc<dyn EventSink>,
    active: Arc<AtomicBool>,
}

impl NativeMissionControl {
    pub fn new(events_tx: impl EventSink + 'static) -> Self {
        Self {
            observer: None,
            app_elem: None,
            active: Arc::new(AtomicBool::new(false)),
            events_tx: Arc::new(events_tx),
        }
    }

    #[instrument(skip(self))]
    /// Installs the Dock observer and keeps it alive for the life of the process.
    pub async fn run(mut self) {
        info!("Starting native mission-control monitor (must run on main thread)");
        self.observe();
        std::future::pending::<()>().await;
    }

    pub fn observe(&mut self) {
        if self.observer.is_some() {
            return;
        }

        let Some(pid) = find_dock_pid() else {
            warn!("Could not find the Dock process; Mission Control observer disabled");
            return;
        };

        let builder = match Observer::new_with_notification(pid) {
            Ok(builder) => builder,
            Err(err) => {
                warn!(?err, pid, "Could not create Dock accessibility observer");
                return;
            }
        };

        let state = Rc::new(RefCell::new(State {
            events_tx: self.events_tx.clone(),
            active: self.active.clone(),
        }));
        let callback_state = state.clone();
        let observer = builder.install_with_notification(move |_elem, notification| {
            callback_state.borrow_mut().handle_notification(notification);
        });

        let elem = AXUIElement::application(pid);
        for notification in NOTIFICATIONS {
            if let Err(err) = observer.add_notification(&elem, notification) {
                warn!(?err, notification, "Could not observe Dock notification");
            }
        }

        self.observer = Some(observer);
        self.app_elem = Some(elem);
    }

}

impl State {
    #[instrument(skip(self))]
    fn handle_notification(&mut self, notification: &'static str) {
        match notification {
            K_AX_EXPOSE_SHOW_ALL_WINDOWS
            | K_AX_EXPOSE_SHOW_FRONT_WINDOWS
            | K_AX_EXPOSE_SHOW_DESKTOP => {
                self.active.store(true, Ordering::SeqCst);
                self.events_tx.send(Event::MissionControlEntered);
            }
            K_AX_EXPOSE_EXIT => {
                self.active.store(false, Ordering::SeqCst);
                self.events_tx.send(Event::MissionControlExited);
            }
            _ => error!(?notification, "Unhandled notification from Dock"),
        }
    }
}

fn find_dock_pid() -> Option<pid_t> {
    let apps =
        NSRunningApplication::runningApplicationsWithBundleIdentifier(ns_string!("com.apple.dock"))
            .to_vec();
    let [app] = apps.as_slice() else {
        return None;
    };
    Some(app.pid())
}
