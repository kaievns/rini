use std::future;

use objc2_app_kit::NSRunningApplication;
use tracing::{debug, warn};

use crate::windows::platform::app::{AppInfo, NSRunningApplicationExt};
use crate::windows::platform::carbon::{CarbonListener, Event, event_type};
use rini_core::ids::pid_t;

const NO_ERR: i32 = 0;

const K_EVENT_CLASS_APPLICATION: u32 = 1_634_758_764; // 'appl'
const K_EVENT_APP_LAUNCHED: u32 = 5;
const K_EVENT_APP_TERMINATED: u32 = 6;
const K_EVENT_APP_FRONT_SWITCHED: u32 = 7;

const K_EVENT_PARAM_PROCESS_ID: u32 = 1_886_613_024; // 'psn '
const TYPE_PROCESS_SERIAL_NUMBER: u32 = 1_886_613_024; // 'psn '

type OSStatus = i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct ProcessSerialNumber {
    high_long_of_psn: u32,
    low_long_of_psn: u32,
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetProcessPID(psn: *const ProcessSerialNumber, pid: *mut pid_t) -> OSStatus;
}

fn app_info_for_pid(pid: pid_t) -> AppInfo {
    if let Some(app) = NSRunningApplication::with_process_id(pid) {
        AppInfo::from(&*app)
    } else {
        warn!("Carbon event: failed to resolve NSRunningApplication for pid {pid}, falling back");
        AppInfo {
            bundle_id: None,
            localized_name: None,
        }
    }
}

unsafe fn pid_for_psn(psn: &ProcessSerialNumber) -> Option<pid_t> {
    let mut pid: pid_t = 0;
    if unsafe { GetProcessPID(psn, &mut pid) } == NO_ERR && pid != 0 {
        Some(pid)
    } else {
        None
    }
}

/// An app came, went, or came to the front, per Carbon's process events.
#[derive(Debug, Clone)]
pub enum AppLifecycle {
    Launched(pid_t, AppInfo),
    FrontSwitched(pid_t),
    Terminated(pid_t),
}

/// Listens to Carbon's application events and forwards them as [`AppLifecycle`].
pub struct ProcessActor {
    _listener: CarbonListener,
}

impl ProcessActor {
    pub fn new(sink: impl Fn(AppLifecycle) + Send + Sync + 'static) -> Self {
        let types = [
            event_type(K_EVENT_CLASS_APPLICATION, K_EVENT_APP_LAUNCHED),
            event_type(K_EVENT_CLASS_APPLICATION, K_EVENT_APP_TERMINATED),
            event_type(K_EVENT_CLASS_APPLICATION, K_EVENT_APP_FRONT_SWITCHED),
        ];

        let listener = CarbonListener::application(&types, move |ev: Event| {
            let psn = match unsafe {
                ev.parameter::<ProcessSerialNumber>(
                    K_EVENT_PARAM_PROCESS_ID,
                    TYPE_PROCESS_SERIAL_NUMBER,
                )
            } {
                Some(psn) => psn,
                None => return NO_ERR,
            };

            let pid = match unsafe { pid_for_psn(&psn) } {
                Some(pid) => pid,
                None => return NO_ERR,
            };

            match ev.kind {
                K_EVENT_APP_LAUNCHED => {
                    debug!("Carbon: App launched ({pid})");
                    let info = app_info_for_pid(pid);
                    sink(AppLifecycle::Launched(pid, info));
                }
                K_EVENT_APP_FRONT_SWITCHED => {
                    debug!("Carbon: App front switched ({pid})");
                    sink(AppLifecycle::FrontSwitched(pid));
                }
                K_EVENT_APP_TERMINATED => {
                    debug!("Carbon: App terminated ({pid})");
                    sink(AppLifecycle::Terminated(pid));
                }
                _ => {}
            }

            NO_ERR
        })
        .expect("Failed to create Carbon application event listener");

        debug!("ProcessActor: Carbon listener installed");
        Self { _listener: listener }
    }

    pub async fn run(self) {
        future::pending::<()>().await;
    }
}
