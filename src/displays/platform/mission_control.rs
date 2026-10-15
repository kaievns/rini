//! Mission Control, observed through the Dock's Accessibility notifications. The application
//! stops trusting window positions while it is up.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use objc2_app_kit::NSRunningApplication;
use objc2_application_services::AXError;
use objc2_foundation::ns_string;
use rini_core::ids::pid_t;
use rini_runloop::timer::Timer;
use tracing::{debug, error, info, instrument, warn};

use crate::displays::event::{Event, EventSink};
use crate::windows::platform::app::NSRunningApplicationExt;
use crate::windows::platform::ax::ask_again::{AskAgain, ask_until};
use crate::windows::platform::ax::element::{AXUIElement, Error as AxError};
use crate::windows::platform::ax::observer::Observer;

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

/// How long the Dock gets to answer its subscriptions. At login it can be too busy to.
const DOCK_PATIENCE: Duration = Duration::from_secs(60);

pub struct NativeMissionControl {
    observer: Option<Observer>,
    app_elem: Option<AXUIElement>,
    unwatched: Vec<&'static str>,
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
            unwatched: NOTIFICATIONS.to_vec(),
            active: Arc::new(AtomicBool::new(false)),
            events_tx: Arc::new(events_tx),
        }
    }

    #[instrument(skip(self))]
    /// Installs the Dock observer, asking again while the Dock is too busy to answer, and keeps it
    /// alive for the life of the process.
    pub async fn run(mut self) {
        info!("Starting native mission-control monitor (must run on main thread)");
        let again = AskAgain::new(Instant::now(), DOCK_PATIENCE);
        if !ask_until(again, Instant::now, Timer::sleep, || self.observe()).await {
            warn!(
                unwatched = ?self.unwatched,
                "The Dock never answered; Mission Control goes unseen this session"
            );
        }
        std::future::pending::<()>().await;
    }

    /// Whether every notification is watched, after one more ask for those that are not.
    fn observe(&mut self) -> bool {
        if self.observer.is_none() {
            let Some(pid) = find_dock_pid() else {
                debug!("Could not find the Dock process");
                return false;
            };
            let builder = match Observer::new_with_notification(pid) {
                Ok(builder) => builder,
                Err(err) => {
                    debug!(?err, pid, "Could not create Dock accessibility observer");
                    return false;
                }
            };
            let state = Rc::new(RefCell::new(State {
                events_tx: self.events_tx.clone(),
                active: self.active.clone(),
            }));
            self.observer = Some(builder.install_with_notification(move |_elem, notification| {
                state.borrow_mut().handle_notification(notification);
            }));
            self.app_elem = Some(AXUIElement::application(pid));
        }
        let (Some(observer), Some(elem)) = (&self.observer, &self.app_elem) else {
            return false;
        };
        watch_each(&mut self.unwatched, |notification| {
            observer.add_notification(elem, notification)
        });
        self.unwatched.is_empty()
    }
}

/// Drops from `unwatched` each notification `watch` subscribes or finds already subscribed.
fn watch_each(
    unwatched: &mut Vec<&'static str>,
    mut watch: impl FnMut(&'static str) -> Result<(), AxError>,
) {
    unwatched.retain(|&notification| match watch(notification) {
        Ok(()) | Err(AxError::Ax(AXError::NotificationAlreadyRegistered)) => false,
        Err(err) => {
            debug!(?err, notification, "The Dock did not answer a subscription");
            true
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A Dock too busy to answer one subscription leaves it to the next ask, and the rest are not
    /// asked again.
    #[test]
    fn a_subscription_the_dock_is_too_busy_to_answer_is_left_for_the_next_ask() {
        let mut unwatched = NOTIFICATIONS.to_vec();
        let mut asked = Vec::new();
        let mut busy = 2;
        let mut ask = |unwatched: &mut Vec<&'static str>| {
            watch_each(unwatched, |notification| {
                asked.push(notification);
                if notification == K_AX_EXPOSE_EXIT && busy > 0 {
                    busy -= 1;
                    return Err(AxError::Ax(AXError::CannotComplete));
                }
                Ok(())
            })
        };

        ask(&mut unwatched);
        assert_eq!(unwatched, [K_AX_EXPOSE_EXIT]);
        ask(&mut unwatched);
        assert_eq!(unwatched, [K_AX_EXPOSE_EXIT]);
        ask(&mut unwatched);
        assert!(unwatched.is_empty());
        assert_eq!(
            asked.len(),
            NOTIFICATIONS.len() + 2,
            "only the unanswered one is asked again"
        );
    }

    #[test]
    fn a_subscription_already_registered_counts_as_watched() {
        let mut unwatched = NOTIFICATIONS.to_vec();
        watch_each(&mut unwatched, |_| {
            Err(AxError::Ax(AXError::NotificationAlreadyRegistered))
        });
        assert!(unwatched.is_empty());
    }
}
