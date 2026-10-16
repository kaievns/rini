#![allow(non_upper_case_globals, reason = "Accessibility's notification names")]
//! The notifications the thread subscribes to, of the application and of each window it takes on.

use objc2_application_services::AXError;
use rini_core::ids::WindowId;
use tracing::trace;

use super::State;
use crate::windows::domain::ax_events::{AxNotificationKind, encode_notification_data};
use crate::windows::platform::ax::element::{AX_WINDOW_ROLE, Error as AxError};
use crate::windows::platform::ax::world::AxWorld;

const kAXApplicationActivatedNotification: &str = "AXApplicationActivated";
const kAXApplicationDeactivatedNotification: &str = "AXApplicationDeactivated";
const kAXApplicationHiddenNotification: &str = "AXApplicationHidden";
const kAXApplicationShownNotification: &str = "AXApplicationShown";
const kAXMainWindowChangedNotification: &str = "AXMainWindowChanged";
const kAXWindowCreatedNotification: &str = "AXWindowCreated";
const kAXMenuOpenedNotification: &str = "AXMenuOpened";
const kAXMenuClosedNotification: &str = "AXMenuClosed";
const kAXUIElementDestroyedNotification: &str = "AXUIElementDestroyed";
const kAXWindowMovedNotification: &str = "AXWindowMoved";
const kAXWindowResizedNotification: &str = "AXWindowResized";
const kAXWindowMiniaturizedNotification: &str = "AXWindowMiniaturized";
const kAXWindowDeminiaturizedNotification: &str = "AXWindowDeminiaturized";
const kAXTitleChangedNotification: &str = "AXTitleChanged";

pub(super) const APP_NOTIFICATIONS: &[(AxNotificationKind, &str)] = &[
    (
        AxNotificationKind::ApplicationActivated,
        kAXApplicationActivatedNotification,
    ),
    (
        AxNotificationKind::ApplicationDeactivated,
        kAXApplicationDeactivatedNotification,
    ),
    (
        AxNotificationKind::ApplicationHidden,
        kAXApplicationHiddenNotification,
    ),
    (
        AxNotificationKind::ApplicationShown,
        kAXApplicationShownNotification,
    ),
    (
        AxNotificationKind::MainWindowChanged,
        kAXMainWindowChangedNotification,
    ),
    (AxNotificationKind::WindowCreated, kAXWindowCreatedNotification),
    (AxNotificationKind::MenuOpened, kAXMenuOpenedNotification),
    (AxNotificationKind::MenuClosed, kAXMenuClosedNotification),
];

const WINDOW_NOTIFICATIONS: &[(AxNotificationKind, &str)] = &[
    (
        AxNotificationKind::WindowDestroyed,
        kAXUIElementDestroyedNotification,
    ),
    (AxNotificationKind::WindowMoved, kAXWindowMovedNotification),
    (AxNotificationKind::WindowResized, kAXWindowResizedNotification),
    (
        AxNotificationKind::WindowMiniaturized,
        kAXWindowMiniaturizedNotification,
    ),
    (
        AxNotificationKind::WindowDeminiaturized,
        kAXWindowDeminiaturizedNotification,
    ),
    (AxNotificationKind::TitleChanged, kAXTitleChangedNotification),
];

impl<W: AxWorld> State<W> {
    pub(super) fn register_window_notifications(&self, elem: &W::Element, wid: WindowId) -> bool {
        match self.ax.role(elem) {
            Ok(role) if role == AX_WINDOW_ROLE => (),
            _ => return false,
        }
        for &(kind, notif) in WINDOW_NOTIFICATIONS {
            let res = self.ax.watch(elem, notif, encode_notification_data(kind, Some(wid)));
            if let Err(err) = res {
                let is_already_registered = matches!(
                    err,
                    AxError::Ax(code) if code == AXError::NotificationAlreadyRegistered
                );
                if !is_already_registered {
                    trace!(?wid, "Watching failed with error {err:?}");
                    return false;
                }
            }
        }
        true
    }

    pub(super) fn remove_window_notifications(&self, elem: &W::Element) {
        for &(_, notif) in WINDOW_NOTIFICATIONS {
            self.ax.unwatch(elem, notif);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{peer, rect, state_with};
    use super::*;
    use crate::windows::domain::request::Request;
    use crate::windows::event::Event;
    use crate::windows::platform::ax::world::FakeAx;

    /// Registering a window subscribes to its notifications. Without that the actor never hears the
    /// window move, resize or close again.
    #[test]
    fn registering_a_window_subscribes_to_its_notifications() {
        let window = 7;
        let (mut state, _sink) =
            state_with(FakeAx::with_one_window(window, rect(0., 0., 100., 100.)));

        state
            .register_window(window, peer(window, rect(0., 0., 100., 100.)))
            .expect("registers");

        let watched = state.ax.watched.borrow();
        for &(_, notif) in WINDOW_NOTIFICATIONS {
            assert!(
                watched.iter().any(|(elem, n)| *elem == window && *n == notif),
                "{notif} was never subscribed"
            );
        }
    }

    /// A window whose subscription the application is too busy to answer is not registered, and the
    /// next AXWindows refresh takes it on: no window server id, so it needs no server record.
    #[test]
    fn a_window_too_busy_to_watch_is_taken_on_at_the_next_refresh() {
        let window = 7;
        let mut ax = FakeAx::with_one_window(window, rect(0., 0., 100., 100.));
        ax.server_ids.clear();
        ax.busy_watches.set(1);
        let (mut state, sink) = state_with(ax);

        assert!(
            state.register_window(window, None).is_none(),
            "left out while busy"
        );
        assert!(state.windows.is_empty());

        state.handle_request(Request::GetVisibleWindows).expect("refreshed");

        let wid = state.id(&window).expect("taken on at the refresh");
        let watched = state.ax.watched.borrow();
        for &(_, notif) in WINDOW_NOTIFICATIONS {
            assert!(
                watched.iter().any(|(elem, n)| *elem == window && *n == notif),
                "{notif} was never subscribed"
            );
        }
        assert!(
            sink.0.lock().iter().any(|event| matches!(
                event,
                Event::WindowsDiscovered { new, .. } if new.iter().any(|(w, _)| *w == wid)
            )),
            "and the reactor hears of it"
        );
    }
}
