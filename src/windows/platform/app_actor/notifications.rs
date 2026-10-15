//! What the application's notifications tell the reactor: windows created, destroyed, moved,
//! miniaturized and retitled, the application hidden and shown, and a change of main window.
//!
//! A notification for an element a window no longer has says nothing about the window: after a
//! wake a refresh rebinds the window to a new element, and a late one for the old is ignored.

use objc2_application_services::AXError;
use rini_core::ids::WindowId;
use tracing::{debug, instrument, trace};

use super::State;
use crate::windows::domain::ax_events::AxNotificationKind;
use crate::windows::domain::transaction::Requested;
use crate::windows::event::Event;
use crate::windows::platform::ax::element::Error as AxError;
use crate::windows::platform::ax::world::AxWorld;

impl<W: AxWorld> State<W> {
    #[instrument(skip_all, fields(pid = ?self.pid, ?notif))]
    pub(super) fn handle_notification(
        &mut self,
        elem: W::Element,
        notif: AxNotificationKind,
        hinted_wid: Option<WindowId>,
    ) {
        trace!(?notif, ?hinted_wid, "Got notification");
        match notif {
            AxNotificationKind::ApplicationHidden => self.on_application_hidden(),
            AxNotificationKind::ApplicationShown => self.on_application_shown(),
            AxNotificationKind::ApplicationActivated
            | AxNotificationKind::ApplicationDeactivated => _ = self.on_ax_activation_changed(),
            AxNotificationKind::MainWindowChanged => {
                // `AXWindows` is filtered to the current macOS space, so using it as
                // a membership list here will incorrectly "destroy" windows that
                // merely live on another space. This fallback therefore only prunes
                // windows whose AX element has actually gone invalid.
                self.remove_stale_windows();
                self.on_main_window_changed(None, false);
            }
            AxNotificationKind::WindowCreated => self.on_window_created(elem),
            AxNotificationKind::MenuOpened => self.send_event(Event::MenuOpened(self.pid)),
            AxNotificationKind::MenuClosed => self.send_event(Event::MenuClosed(self.pid)),
            AxNotificationKind::WindowDestroyed => self.on_window_destroyed(&elem, hinted_wid),
            AxNotificationKind::WindowMoved | AxNotificationKind::WindowResized => {
                self.on_window_moved(&elem, notif, hinted_wid)
            }
            AxNotificationKind::WindowMiniaturized => {
                self.on_miniaturized_changed(&elem, hinted_wid, true)
            }
            AxNotificationKind::WindowDeminiaturized => {
                self.on_miniaturized_changed(&elem, hinted_wid, false)
            }
            AxNotificationKind::TitleChanged => self.on_title_changed(&elem, hinted_wid),
        }
    }

    fn on_window_created(&mut self, elem: W::Element) {
        if self.id(&elem).is_ok() {
            return;
        }
        let Some((window, wid, window_server_info)) = self.register_window(elem, None) else {
            return;
        };
        let window_server_info = window_server_info
            .or_else(|| window.sys_id.and_then(|id| self.ax.server().get_window(id)));
        self.send_event(Event::WindowCreated {
            window: wid,
            info: window,
            server_info: window_server_info,
            mouse_state: self.ax.mouse_state(),
        });
    }

    fn on_window_destroyed(&mut self, elem: &W::Element, hinted_wid: Option<WindowId>) {
        let Ok(wid) = self.wid_for_notification(elem, hinted_wid) else {
            return;
        };
        // A refreshed AXUIElement can reuse the same stable WindowServer-backed
        // WindowId. Removing by the callback's encoded wid would then let a late
        // destroy notification for the superseded element tear down the replacement.
        // Only the element currently bound to this wid owns its lifetime.
        if !self.is_current_window_element(wid, elem) {
            trace!(?wid, "Ignoring destroy notification for superseded AX element");
            return;
        }
        if self.remove_window(wid).is_none() {
            return;
        }
        self.send_event(Event::WindowDestroyed(wid));

        self.on_main_window_changed(Some(wid), false);
    }

    fn on_window_moved(
        &mut self,
        elem: &W::Element,
        notif: AxNotificationKind,
        hinted_wid: Option<WindowId>,
    ) {
        let Ok(wid) = self.wid_for_notification(elem, hinted_wid) else {
            return;
        };
        if !self.is_current_window_element(wid, elem) {
            trace!(?wid, ?notif, "Ignoring notification for superseded AX element");
            return;
        }

        let mouse_state = self.ax.mouse_state();
        let Ok(window) = self.window(wid) else {
            return;
        };
        let txid = self.txid_for_window_state(window);
        let frame = match self.ax.frame(elem) {
            Ok(frame) => frame,
            // During display teardown, macOS can send AXWindowMoved after
            // the old AX element has been invalidated. This is not a
            // destruction notification. Only AXUIElementDestroyed is
            // authoritative for removing the app's window record; treating
            // this transient read failure as a destroy drops manual
            // workspace ownership before the window is rediscovered.
            Err(AxError::Ax(AXError::InvalidUIElement)) => {
                trace!(
                    ?wid,
                    ?notif,
                    "Ignoring invalid AX element from move/resize notification"
                );
                return;
            }
            Err(AxError::Ax(AXError::CannotComplete)) => return,
            Err(err) => {
                debug!(?wid, ?err, "Failed to read frame for window");
                return;
            }
        };
        self.send_event(Event::WindowFrameChanged {
            window: wid,
            frame,
            last_seen: txid,
            requested: Requested(false),
            mouse_state,
        });
    }

    /// A window the user miniaturized or brought back, which a show of the application leaves as
    /// it is.
    fn on_miniaturized_changed(
        &mut self,
        elem: &W::Element,
        hinted_wid: Option<WindowId>,
        miniaturized: bool,
    ) {
        let Ok(wid) = self.wid_for_notification(elem, hinted_wid) else {
            return;
        };
        let Some(window) = self.windows.get_mut(&wid).filter(|window| window.elem == *elem) else {
            if miniaturized {
                trace!(?wid, "Ignoring miniaturize for superseded AX element");
            } else {
                trace!(?wid, "Ignoring deminiaturize for superseded AX element");
            }
            return;
        };
        window.hidden_by_app = false;
        self.send_event(if miniaturized {
            Event::WindowMinimized(wid)
        } else {
            Event::WindowDeminiaturized(wid)
        });
    }

    fn on_title_changed(&mut self, elem: &W::Element, hinted_wid: Option<WindowId>) {
        let Ok(wid) = self.wid_for_notification(elem, hinted_wid) else {
            return;
        };
        if !self.is_current_window_element(wid, elem) {
            trace!(?wid, "Ignoring title change for superseded AX element");
            return;
        }
        match self.ax.title(elem) {
            Ok(title) => {
                let Ok(window) = self.window_mut(wid) else {
                    return;
                };
                if window.title == title {
                    return;
                }
                window.title = title.clone();
                self.send_event(Event::WindowTitleChanged(wid, title));
            }
            Err(err) => debug!(
                ?wid,
                ?err,
                "Failed to read title for WindowTitleChanged notification"
            ),
        }
    }

    fn on_application_hidden(&mut self) {
        if self.is_hidden {
            return;
        }

        self.is_hidden = true;
        let mut to_minimize = Vec::new();
        for (wid, window) in self.windows.iter_mut() {
            if window.hidden_by_app {
                continue;
            }
            window.hidden_by_app = true;
            to_minimize.push(*wid);
        }

        for wid in to_minimize {
            self.send_event(Event::WindowMinimized(wid));
        }
    }

    fn on_application_shown(&mut self) {
        if !self.is_hidden {
            return;
        }

        self.is_hidden = false;
        let mut to_restore = Vec::new();
        for (wid, window) in self.windows.iter_mut() {
            if !window.hidden_by_app {
                continue;
            }
            window.hidden_by_app = false;
            let minimized = match super::trace("minimized", wid, || self.ax.minimized(&window.elem))
            {
                Ok(minimized) => minimized,
                Err(err) => {
                    debug!(?wid, ?err, "Failed to read minimized state after app shown");
                    false
                }
            };
            if minimized {
                continue;
            }
            let wid = *wid;
            to_restore.push(wid);
        }

        for wid in to_restore {
            self.send_event(Event::WindowDeminiaturized(wid));
        }
    }
}

#[cfg(test)]
mod tests {
    use rini_core::ids::WindowServerId;

    use super::super::fixtures::{
        PID, Sink, announced, app_with, minimized_and_restored, peer, rect, state_with,
    };
    use super::*;
    use crate::windows::domain::request::{Quiet, Request};
    use crate::windows::platform::ax::world::{FakeAx, MouseState};

    /// A created window is announced with the window server's record of it and the pointer state,
    /// so a window that opens mid-drag is not taken for a layout echo.
    #[test]
    fn a_created_window_is_announced_with_its_server_record_and_the_pointer_state() {
        let window = 7;
        let frame = rect(0., 0., 100., 100.);
        let mut ax = FakeAx::with_one_window(window, frame);
        ax.server.set_window(WindowServerId::new(window), peer(window, frame));
        ax.mouse = Some(MouseState::Down);
        let (mut state, sink) = state_with(ax);

        state.handle_notification(window, AxNotificationKind::WindowCreated, None);

        let announced = sink.0.borrow();
        assert!(
            matches!(
                announced.as_slice(),
                [Event::WindowCreated { server_info: Some(info), mouse_state: Some(MouseState::Down), .. }]
                    if info.id == WindowServerId::new(window)
            ),
            "{announced:?}"
        );
    }

    /// A window with a server id the window server does not report is not on screen, and is not
    /// taken on when the application says it was created.
    #[test]
    fn a_created_window_the_window_server_does_not_report_is_not_taken_on() {
        let window = 7;
        let (mut state, sink) =
            state_with(FakeAx::with_one_window(window, rect(0., 0., 100., 100.)));

        state.handle_notification(window, AxNotificationKind::WindowCreated, None);

        assert!(state.windows.is_empty());
        assert!(sink.0.borrow().is_empty(), "{:?}", sink.0.borrow());
    }

    /// `window` registered, then found at a refresh as a new element, `replacement`, of the same
    /// window-server window, as after a wake: the element the window had is superseded.
    fn replaced(window: u32, replacement: u32) -> (State<FakeAx>, Sink, WindowId) {
        let (mut state, sink, wids) = app_with(&[window]);
        state.ax.describe_standard_window(replacement, rect(0., 0., 100., 100.));
        state.ax.server_ids.insert(replacement, WindowServerId::new(window));
        state.ax.windows = vec![replacement];
        state.handle_request(Request::GetVisibleWindows).expect("refreshed");
        assert!(
            state.is_current_window_element(wids[0], &replacement),
            "rebound to the replacement"
        );
        sink.0.borrow_mut().clear();
        (state, sink, wids[0])
    }

    /// A late notification for the element a window had before it was replaced says nothing about
    /// the window: the element now bound to it does.
    #[test]
    fn a_move_or_resize_for_a_replaced_element_is_ignored() {
        let (mut state, sink, wid) = replaced(7, 70);
        state.ax.frames.insert(7, rect(300., 0., 100., 100.));

        for kind in [
            AxNotificationKind::WindowMoved,
            AxNotificationKind::WindowResized,
        ] {
            state.handle_notification(7, kind, Some(wid));
        }

        assert!(announced(&sink).is_empty());
    }

    #[test]
    fn a_destroy_for_a_replaced_element_leaves_the_window() {
        let (mut state, sink, wid) = replaced(7, 70);

        state.handle_notification(7, AxNotificationKind::WindowDestroyed, Some(wid));

        assert!(state.windows.contains_key(&wid), "the window stays");
        assert!(announced(&sink).is_empty());
    }

    #[test]
    fn a_title_change_for_a_replaced_element_is_ignored() {
        let (mut state, sink, wid) = replaced(7, 70);
        state.ax.titles.insert(7, "stale".to_owned());

        state.handle_notification(7, AxNotificationKind::TitleChanged, Some(wid));

        assert!(announced(&sink).is_empty());
    }

    #[test]
    fn a_miniaturize_or_deminiaturize_for_a_replaced_element_is_ignored() {
        let (mut state, sink, wid) = replaced(7, 70);

        for kind in [
            AxNotificationKind::WindowMiniaturized,
            AxNotificationKind::WindowDeminiaturized,
        ] {
            state.handle_notification(7, kind, Some(wid));
        }

        assert!(announced(&sink).is_empty());
    }

    /// A move the application reports is announced as not asked for by rini, with the frame read
    /// back and the pointer state.
    #[test]
    fn a_move_is_announced_unrequested_with_the_pointer_state() {
        let (mut state, sink, wid) = replaced(7, 70);
        let moved = rect(300., 0., 100., 100.);
        state.ax.frames.insert(70, moved);
        state.ax.mouse = Some(MouseState::Down);

        state.handle_notification(70, AxNotificationKind::WindowMoved, Some(wid));

        let events = announced(&sink);
        assert!(
            matches!(
                events.as_slice(),
                [Event::WindowFrameChanged { window: w, frame, last_seen: None, requested: Requested(false), mouse_state: Some(MouseState::Down) }]
                    if *w == wid && *frame == moved
            ),
            "{events:?}"
        );
    }

    /// A move whose frame cannot be read, as during a display teardown, is not a destroy.
    #[test]
    fn a_move_whose_element_cannot_be_read_is_not_taken_for_a_destroy() {
        let (mut state, sink, wids) = app_with(&[7]);
        state.ax.frames.remove(&7);

        state.handle_notification(7, AxNotificationKind::WindowMoved, Some(wids[0]));

        assert!(state.windows.contains_key(&wids[0]), "the window stays");
        assert!(announced(&sink).is_empty());
    }

    #[test]
    fn a_destroy_retires_the_window_once() {
        let (mut state, sink, wids) = app_with(&[7]);

        for _ in 0..2 {
            state.handle_notification(7, AxNotificationKind::WindowDestroyed, Some(wids[0]));
        }

        assert!(state.windows.is_empty());
        let events = announced(&sink);
        assert!(
            matches!(events.as_slice(), [Event::WindowDestroyed(w)] if *w == wids[0]),
            "{events:?}"
        );
    }

    #[test]
    fn a_title_change_is_announced_once_per_title() {
        let (mut state, sink, wids) = app_with(&[7]);
        state.ax.titles.insert(7, "renamed".to_owned());

        for _ in 0..2 {
            state.handle_notification(7, AxNotificationKind::TitleChanged, Some(wids[0]));
        }

        let events = announced(&sink);
        assert!(
            matches!(events.as_slice(), [Event::WindowTitleChanged(w, title)]
                if *w == wids[0] && title == "renamed"),
            "{events:?}"
        );
    }

    #[test]
    fn a_miniaturized_window_is_announced_minimized_and_a_deminiaturized_one_restored() {
        let (mut state, sink, wids) = app_with(&[7]);

        state.handle_notification(7, AxNotificationKind::WindowMiniaturized, Some(wids[0]));
        state.handle_notification(7, AxNotificationKind::WindowDeminiaturized, Some(wids[0]));

        let events = announced(&sink);
        assert!(
            matches!(
                events.as_slice(),
                [Event::WindowMinimized(a), Event::WindowDeminiaturized(b)]
                    if *a == wids[0] && *b == wids[0]
            ),
            "{events:?}"
        );
    }

    #[test]
    fn hiding_the_application_minimizes_each_window_once() {
        let (mut state, sink, wids) = app_with(&[7, 8]);

        for _ in 0..2 {
            state.handle_notification(1, AxNotificationKind::ApplicationHidden, None);
        }

        assert_eq!(minimized_and_restored(&announced(&sink)), (wids, vec![]));
    }

    /// The show restores what the hide minimized, but not a window the user minimized meanwhile.
    #[test]
    fn showing_the_application_restores_what_hiding_minimized_and_nothing_else() {
        let (mut state, sink, wids) = app_with(&[7, 8]);
        state.handle_notification(1, AxNotificationKind::ApplicationHidden, None);
        state.ax.minimized.insert(8, true);
        announced(&sink);

        for _ in 0..2 {
            state.handle_notification(1, AxNotificationKind::ApplicationShown, None);
        }

        assert_eq!(
            minimized_and_restored(&announced(&sink)),
            (vec![], vec![wids[0]])
        );
    }

    /// A window miniaturized while its application is hidden was minimized by the user, not by the
    /// hide, so the show leaves it minimized.
    #[test]
    fn a_window_miniaturized_while_the_application_is_hidden_stays_minimized_at_the_show() {
        let (mut state, sink, wids) = app_with(&[7, 8]);
        state.handle_notification(1, AxNotificationKind::ApplicationHidden, None);
        state.handle_notification(8, AxNotificationKind::WindowMiniaturized, Some(wids[1]));
        announced(&sink);

        state.handle_notification(1, AxNotificationKind::ApplicationShown, None);

        assert_eq!(
            minimized_and_restored(&announced(&sink)),
            (vec![], vec![wids[0]])
        );
    }

    /// AXWindows is filtered to the current space, so a change of main window retires only a window
    /// whose element has died, and names the new main window.
    #[test]
    fn a_main_window_change_retires_only_a_dead_element_and_names_the_main_window() {
        let (mut state, sink, wids) = app_with(&[7, 8]);
        state.ax.windows = vec![7];
        state.ax.frames.remove(&8);
        state.ax.roles.remove(&8);
        state.ax.main_window = Some(7);

        state.handle_notification(1, AxNotificationKind::MainWindowChanged, None);

        let events = announced(&sink);
        assert!(
            matches!(
                events.as_slice(),
                [
                    Event::WindowDestroyed(gone),
                    Event::ApplicationMainWindowChanged(PID, Some(main), Quiet::No),
                ] if *gone == wids[1] && *main == wids[0]
            ),
            "{events:?}"
        );
    }
}
