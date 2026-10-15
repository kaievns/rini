//! The application's main window and its activation, and whether rini's own raise caused the
//! activation, which then carries the raise's quiet.

use std::time::{Duration, Instant};

use rini_core::ids::WindowId;
use tracing::{debug, info, trace, warn};

use super::State;
use crate::windows::domain::request::Quiet;
use crate::windows::event::Event;
use crate::windows::platform::ax::element::Error as AxError;
use crate::windows::platform::ax::world::AxWorld;

impl<W: AxWorld> State<W> {
    pub(super) fn on_main_window_changed(
        &mut self,
        quiet_if: Option<WindowId>,
        allow_register: bool,
    ) -> Option<WindowId> {
        let elem =
            match super::trace("main_window", self.pid, || self.ax.main_window(&self.ax.app())) {
                Ok(elem) => elem,
                Err(e) => {
                    if self.windows.is_empty() {
                        trace!("Failed to read main window (no windows): {e:?}");
                    } else {
                        warn!("Failed to read main window: {e:?}");
                    }
                    return None;
                }
            };

        let wid = match self.id(&elem).ok() {
            Some(wid) => wid,
            None => {
                if !allow_register {
                    info!(?self.pid, "Got MainWindowChanged on unknown window; clearing main window");
                    if self.main_window.take().is_some() {
                        self.send_event(Event::ApplicationMainWindowChanged(
                            self.pid,
                            None,
                            Quiet::No,
                        ));
                    }
                    return None;
                }
                let Some((info, wid, window_server_info)) = self.register_window(elem, None) else {
                    debug!(?self.pid, "Got MainWindowChanged on unknown window");
                    return None;
                };
                let window_server_info = window_server_info
                    .or_else(|| info.sys_id.and_then(|id| self.ax.server().get_window(id)));
                self.send_event(Event::WindowCreated(
                    wid,
                    info,
                    window_server_info,
                    self.ax.mouse_state(),
                ));
                wid
            }
        };

        if self.main_window == Some(wid) {
            return Some(wid);
        }
        self.main_window = Some(wid);
        let quiet = match quiet_if {
            Some(id) if id == wid => Quiet::Yes,
            _ => Quiet::No,
        };
        self.send_event(Event::ApplicationMainWindowChanged(self.pid, Some(wid), quiet));
        Some(wid)
    }

    fn take_activation_context(&mut self) -> (Quiet, Option<WindowId>) {
        match self.last_activated.take() {
            Some((ts, quiet_activation, quiet_window_change, tx)) => {
                _ = tx.send(());
                if ts.elapsed() < Duration::from_millis(1000) {
                    trace!("by us");
                    (quiet_activation, quiet_window_change)
                } else {
                    trace!("by user");
                    (Quiet::No, None)
                }
            }
            None => {
                trace!("by user");
                (Quiet::No, None)
            }
        }
    }

    pub(super) fn on_ax_activation_changed(&mut self) -> Result<(), AxError> {
        let is_frontmost =
            super::trace("is_frontmost", self.pid, || self.ax.frontmost(&self.ax.app()))?;
        let old_frontmost = std::mem::replace(&mut self.is_frontmost, is_frontmost);
        debug!(
            "on_ax_activation_changed, pid={:?}, is_frontmost={:?}, old_frontmost={:?}",
            self.pid, is_frontmost, old_frontmost
        );

        if !is_frontmost {
            self.pending_activation_quiet = None;
            if old_frontmost {
                self.send_event(Event::ApplicationDeactivated(self.pid));
            }
        } else if !old_frontmost {
            let (quiet, quiet_window_change) = self.take_activation_context();
            self.on_main_window_changed(quiet_window_change, true);
            self.pending_activation_quiet = Some((Instant::now(), quiet));
        }
        Ok(())
    }

    pub(super) fn on_global_activation(&mut self) -> Result<(), AxError> {
        let (quiet, quiet_window_change) = if self.last_activated.is_some() {
            self.take_activation_context()
        } else if let Some((ts, quiet)) = self.pending_activation_quiet.take()
            && ts.elapsed() < Duration::from_millis(1000)
        {
            (quiet, None)
        } else {
            (Quiet::No, None)
        };

        // Carbon is the authoritative inter-application activation edge. AX
        // frontmost polling/notifications can lag it, so do not reject this
        // request based on a transient AX value.
        self.is_frontmost = true;
        if self.on_main_window_changed(quiet_window_change, true).is_none()
            && self.main_window.take().is_some()
        {
            // Do not let the reactor reuse a previous window as the target for
            // this authoritative activation when AX cannot resolve the current
            // main window. This event is queued before ApplicationActivated.
            self.send_event(Event::ApplicationMainWindowChanged(self.pid, None, Quiet::No));
        }
        self.send_event(Event::ApplicationActivated(self.pid, quiet));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use tokio_util::sync::CancellationToken;

    use super::super::fixtures::{PID, app_with, main_window_changes, poll_once};
    use super::*;
    use crate::windows::domain::ax_events::AxNotificationKind;
    use crate::windows::domain::request::{RaiseSequence, Request};

    /// The windows a batch raises before its last are always quiet, so the activation its first
    /// window waits for is quiet whatever the batch asked.
    #[test]
    fn a_batch_waits_for_activation_quietly_for_its_first_window() {
        let (mut state, sink, wids) = app_with(&[7, 8]);
        state.ax.frontmost = false;
        let state = RefCell::new(state);
        let token = CancellationToken::new();
        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            wids.clone(),
            &token,
            RaiseSequence(1),
            Quiet::No
        ));
        assert!(poll_once(raise.as_mut()).is_pending());

        {
            let mut app = state.borrow_mut();
            app.ax.frontmost = true;
            app.ax.main_window = Some(7);
            app.handle_notification(1, AxNotificationKind::ApplicationActivated, None);
        }

        assert_eq!(main_window_changes(&sink), [(Some(wids[0]), Quiet::Yes)]);
        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        assert_eq!(*state.borrow().ax.raised.borrow(), [7, 8]);
        state
            .borrow_mut()
            .handle_request(Request::ApplicationGloballyActivated(PID))
            .expect("handled");
        assert!(
            sink.0
                .borrow()
                .iter()
                .any(|event| matches!(event, Event::ApplicationActivated(PID, Quiet::Yes))),
            "the activation is quiet: {:?}",
            sink.0.borrow()
        );
    }

    /// A raise of one window activates its application with the quiet it was asked, which the
    /// application's activation, when the reactor hears of it, carries.
    #[test]
    fn a_single_window_raise_activates_with_the_quiet_it_was_asked() {
        let (mut state, sink, wids) = app_with(&[7]);
        state.ax.frontmost = false;
        let state = RefCell::new(state);
        let token = CancellationToken::new();
        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            wids.clone(),
            &token,
            RaiseSequence(1),
            Quiet::Yes
        ));
        assert!(poll_once(raise.as_mut()).is_pending());
        {
            let mut app = state.borrow_mut();
            app.ax.frontmost = true;
            app.ax.main_window = Some(7);
            app.handle_notification(1, AxNotificationKind::ApplicationActivated, None);
        }
        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));

        state
            .borrow_mut()
            .handle_request(Request::ApplicationGloballyActivated(PID))
            .expect("handled");

        assert_eq!(main_window_changes(&sink), [(Some(wids[0]), Quiet::Yes)]);
        assert!(
            sink.0
                .borrow()
                .iter()
                .any(|event| matches!(event, Event::ApplicationActivated(PID, Quiet::Yes))),
            "{:?}",
            sink.0.borrow()
        );
    }
}
