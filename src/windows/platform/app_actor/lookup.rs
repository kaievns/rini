//! Finding a window the thread tracks, and what a failed Accessibility answer about one makes of it:
//! retired, ignored or handed back, by `domain::ax_events::handling`.

use objc2_application_services::AXError;
use rini_core::ids::{WindowId, WindowServerId};
use tracing::{debug, trace, warn};

use super::{AppWindowState, State};
use crate::windows::domain::ax_events::{AxFailure, Handling, handling, is_gone};
use crate::windows::domain::transaction::TransactionId;
use crate::windows::event::Event;
use crate::windows::platform::ax::element::Error as AxError;
use crate::windows::platform::ax::world::AxWorld;

impl<W: AxWorld> State<W> {
    pub(super) fn window(&self, wid: WindowId) -> Result<&AppWindowState<W::Element>, AxError> {
        self.check_own(wid)?;
        self.windows.get(&wid).ok_or(AxError::NotFound)
    }

    pub(super) fn window_mut(
        &mut self,
        wid: WindowId,
    ) -> Result<&mut AppWindowState<W::Element>, AxError> {
        self.check_own(wid)?;
        self.windows.get_mut(&wid).ok_or(AxError::NotFound)
    }

    /// The element and window-server id a write to `wid` goes to, with `txid` recorded as the
    /// window's last write; `None` for a window this thread does not track, which a write skips.
    pub(super) fn write_target(
        &mut self,
        wid: WindowId,
        txid: TransactionId,
    ) -> Option<(W::Element, Option<WindowServerId>)> {
        let window = self.window_mut(wid).ok()?;
        window.last_seen_txid = txid;
        Some((window.elem.clone(), window.window_server_id))
    }

    /// Another application's window is not found here, and says so in the log.
    fn check_own(&self, wid: WindowId) -> Result<(), AxError> {
        if wid.pid == self.pid {
            return Ok(());
        }
        warn!(?wid, pid = ?self.pid, "asked about another application's window");
        Err(AxError::NotFound)
    }

    pub(super) fn id(&self, elem: &W::Element) -> Result<WindowId, AxError> {
        if let Some(id) = self.ax.window_server_id(elem)
            && let Some(idx) = id.as_nonzero()
        {
            let wid = WindowId { pid: self.pid, idx };
            if self.windows.contains_key(&wid) {
                return Ok(wid);
            }
        }
        if let Some(&wid) = self.elem_to_wid.get(elem) {
            return Ok(wid);
        }
        Err(AxError::NotFound)
    }

    pub(super) fn wid_for_notification(
        &self,
        elem: &W::Element,
        hinted_wid: Option<WindowId>,
    ) -> Result<WindowId, AxError> {
        hinted_wid
            .filter(|wid| wid.pid == self.pid)
            .or_else(|| self.id(elem).ok())
            .ok_or(AxError::NotFound)
    }

    pub(super) fn is_current_window_element(&self, wid: WindowId, elem: &W::Element) -> bool {
        self.windows.get(&wid).is_some_and(|window| window.elem == *elem)
    }

    pub(super) fn remove_window(&mut self, wid: WindowId) -> Option<AppWindowState<W::Element>> {
        let window = self.windows.remove(&wid)?;
        self.elem_to_wid.remove(&window.elem);
        Some(window)
    }

    /// Translate an Accessibility error code into what rini makes of it. The rule itself is
    /// `domain::ax_events::handling`; this is the only place that knows macOS's spelling of it.
    fn failure_of(err: &AxError) -> AxFailure {
        match err {
            AxError::NotFound => AxFailure::Untracked,
            AxError::Ax(AXError::InvalidUIElement) => AxFailure::ElementInvalid,
            AxError::Ax(AXError::CannotComplete) => AxFailure::AppBusy,
            AxError::Ax(_) => AxFailure::Other,
        }
    }

    /// `result`'s value, or `None` for a failure that retires `wid` or is ignored; the error for one
    /// the caller has to hear.
    pub(super) fn handle_ax_result<T>(
        &mut self,
        wid: WindowId,
        result: Result<T, AxError>,
    ) -> Result<Option<T>, AxError> {
        let error = match result {
            Ok(value) => return Ok(Some(value)),
            Err(error) => error,
        };
        match handling(Self::failure_of(&error)) {
            Handling::Retire => {
                self.retire_window(wid);
                Ok(None)
            }
            Handling::Ignore => {
                trace!(
                    ?wid,
                    ?error,
                    "AX request did not answer; leaving window registered"
                );
                Ok(None)
            }
            Handling::Propagate => Err(error),
        }
    }

    fn retire_window(&mut self, wid: WindowId) {
        if self.remove_window(wid).is_some() {
            self.send_event(Event::WindowDestroyed(wid));
            self.on_main_window_changed(Some(wid), false);
        }
    }

    pub(super) fn remove_stale_windows(&mut self) {
        let mut to_remove = Vec::new();
        for (&wid, window) in self.windows.iter() {
            // `kAXWindowsAttribute` is space-filtered and cannot be used to decide
            // whether a tracked window still exists globally. Only drop state when
            // the element itself has become invalid.
            if let Err(error) = self.ax.role(&window.elem)
                && is_gone(Self::failure_of(&error))
            {
                to_remove.push(wid);
            }
        }

        for wid in to_remove {
            self.remove_tracked_window(wid, "Removed stale window (invalid AX element)");
        }
    }

    fn remove_tracked_window(&mut self, wid: WindowId, reason: &'static str) {
        if self.remove_window(wid).is_some() {
            debug!(?wid, reason);
            self.send_event(Event::WindowDestroyed(wid));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{PID, announced, app_with, peer, raisable, rect, state_with};
    use super::*;
    use crate::windows::domain::request::Quiet;
    use crate::windows::platform::ax::world::FakeAx;

    /// The sweep retires a window whose element has died, and only that.
    #[test]
    fn the_sweep_retires_a_window_whose_element_died() {
        let (kept, died) = (7, 8);
        let mut ax = FakeAx::with_one_window(kept, rect(0., 0., 100., 100.));
        ax.windows.push(died);
        ax.describe_standard_window(died, rect(200., 0., 100., 100.));
        let (mut state, sink) = state_with(ax);

        let kept_wid = state
            .register_window(kept, peer(kept, rect(0., 0., 100., 100.)))
            .expect("registers")
            .1;
        let died_wid = state
            .register_window(died, peer(died, rect(200., 0., 100., 100.)))
            .expect("registers")
            .1;

        // The element goes: every read about it now fails the way Accessibility reports a dead one.
        state.ax.frames.remove(&died);
        state.ax.roles.remove(&died);
        sink.0.lock().clear();

        state.remove_stale_windows();

        assert!(state.windows.contains_key(&kept_wid), "the live window stays");
        assert!(!state.windows.contains_key(&died_wid), "the dead one goes");
        let announced = sink.0.lock();
        assert_eq!(announced.len(), 1, "told exactly once: {announced:?}");
        assert!(
            matches!(announced[0], Event::WindowDestroyed(w) if w == died_wid),
            "{announced:?}"
        );
    }

    /// A busy application is not a dead one. Every read failing with "could not complete" must leave
    /// the window registered, because that is what an application under load looks like.
    #[test]
    fn the_sweep_keeps_a_window_whose_application_is_merely_busy() {
        let window = 7;
        let (mut state, sink) =
            state_with(FakeAx::with_one_window(window, rect(0., 0., 100., 100.)));
        let wid = state
            .register_window(window, peer(window, rect(0., 0., 100., 100.)))
            .expect("registers")
            .1;
        sink.0.lock().clear();

        // The application goes under load AFTER the window is known: every read now fails with
        // "could not complete", which is what a wedged or busy application looks like.
        state.ax.busy = true;
        state.remove_stale_windows();

        assert!(
            state.windows.contains_key(&wid),
            "a busy application has not lost its windows"
        );
        assert!(sink.0.lock().is_empty(), "and nothing is announced");
    }

    #[test]
    fn another_applications_window_is_not_found_rather_than_a_panic() {
        let (mut state, _sink, wid) = raisable(7);
        let foreign = WindowId::new(PID + 1, wid.idx.get());
        assert!(matches!(state.window(foreign), Err(AxError::NotFound)));
        assert!(matches!(state.window_mut(foreign), Err(AxError::NotFound)));
        assert!(state.window(wid).is_ok());
    }

    /// A write goes to the element the window has now, and records its txid; another application's
    /// window and one the thread never took on are no target, and record nothing.
    #[test]
    fn a_write_targets_only_a_tracked_window_of_this_application() {
        let (mut state, _sink, wid) = raisable(7);
        let txid = TransactionId::default().next();

        assert_eq!(
            state.write_target(wid, txid),
            Some((7, Some(WindowServerId::new(7))))
        );
        assert_eq!(state.windows[&wid].last_seen_txid, txid);
        assert_eq!(state.write_target(WindowId::new(PID + 1, 7), txid), None);
        assert_eq!(state.write_target(WindowId::new(PID, 9), txid), None);
    }

    /// An invalid element retires its window: the record goes, the reactor hears it destroyed and
    /// the main window is read again.
    #[test]
    fn a_read_whose_element_died_retires_the_window() {
        let (mut state, sink, wids) = app_with(&[7, 8]);
        state.ax.main_window = Some(8);
        state.ax.frames.remove(&7);

        let read = state.ax.frame(&7);
        assert!(matches!(state.handle_ax_result(wids[0], read), Ok(None)));

        assert!(!state.windows.contains_key(&wids[0]));
        let events = announced(&sink);
        assert!(
            matches!(
                events.as_slice(),
                [
                    Event::WindowDestroyed(gone),
                    Event::ApplicationMainWindowChanged(PID, Some(main), Quiet::No),
                ] if *gone == wids[0] && *main == wids[1]
            ),
            "{events:?}"
        );
    }

    /// A read the application is too busy to answer leaves the window, and one that fails another
    /// way goes back to the caller.
    #[test]
    fn a_busy_read_is_ignored_and_any_other_failure_handed_back() {
        let (mut state, sink, wid) = raisable(7);

        let busy = Err::<(), _>(AxError::Ax(AXError::CannotComplete));
        assert!(matches!(state.handle_ax_result(wid, busy), Ok(None)));
        let other = Err::<(), _>(AxError::Ax(AXError::Failure));
        assert!(matches!(
            state.handle_ax_result(wid, other),
            Err(AxError::Ax(AXError::Failure))
        ));

        assert!(state.windows.contains_key(&wid), "the window stays");
        assert!(announced(&sink).is_empty());
    }
}
