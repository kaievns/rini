//! Raising a batch of one application's windows, on a task of its own beside the requests: make
//! the first key, wait for the application to come to the front when it is in the background, and
//! raise the rest in order, last on top.

use std::cell::{RefCell, RefMut};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use parking_lot::MutexGuard;
use rini_core::ids::WindowId;
use rini_runloop::channel as channels;
use rini_runloop::timer::Timer;
use tokio::select;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, trace, warn};

use super::State;
use crate::windows::domain::request::{Quiet, RaiseSequence};
use crate::windows::event::Event;
use crate::windows::platform::ax::element::{AX_STANDARD_WINDOW_SUBROLE, Error as AxError};
use crate::windows::platform::ax::world::AxWorld;

pub(super) struct RaiseRequest {
    pub(super) windows: Vec<WindowId>,
    pub(super) token: CancellationToken,
    pub(super) sequence_id: RaiseSequence,
    pub(super) quiet: Quiet,
}

#[derive(Debug)]
#[allow(dead_code, reason = "used by Debug impls")]
pub(super) enum RaiseError {
    RaiseCancelled,
    AXError(AxError),
}

impl From<AxError> for RaiseError {
    fn from(value: AxError) -> Self { Self::AXError(value) }
}

/// One raise at a time across every application's thread, its activation wait included: a raise in
/// another application blocks its thread on this until the one holding it has raised its last window.
static RAISING: LazyLock<parking_lot::Mutex<()>> = LazyLock::new(|| parking_lot::Mutex::new(()));

/// The quiet of the activation a raise of `len` windows waits for with `first`, and the window
/// whose becoming main is quiet: the raise's own when `first` is also the last, and always quiet
/// for a window before the last.
fn activation_quiet(quiet: Quiet, first: WindowId, len: usize) -> (Quiet, Option<WindowId>) {
    if len == 1 {
        (quiet, (quiet == Quiet::Yes).then_some(first))
    } else {
        (Quiet::Yes, Some(first))
    }
}

impl<W: AxWorld> State<W> {
    pub(super) async fn handle_raises(
        this: &RefCell<Self>,
        mut rx: channels::Receiver<RaiseRequest>,
    ) {
        while let Some((span, raise)) = rx.recv().await {
            let RaiseRequest {
                windows,
                token,
                sequence_id,
                quiet,
            } = raise;
            if let Err(e) = Self::handle_raise_request(this, windows, &token, sequence_id, quiet)
                .instrument(span)
                .await
            {
                debug!("Raise request failed: {e:?}");
            }
        }
    }

    /// `wids` without another application's windows, each dropped with a warning.
    fn own_windows(&self, wids: Vec<WindowId>) -> Vec<WindowId> {
        let (own, foreign): (Vec<_>, Vec<_>) =
            wids.into_iter().partition(|wid| wid.pid == self.pid);
        if !foreign.is_empty() {
            warn!(pid = ?self.pid, ?foreign, "dropped another application's windows from a raise");
        }
        own
    }

    #[allow(
        clippy::await_holding_refcell_ref,
        reason = "the RefMut moves into wait_for_activation, which drops it before its first await"
    )]
    pub(super) async fn handle_raise_request(
        this_ref: &RefCell<Self>,
        wids: Vec<WindowId>,
        token: &CancellationToken,
        sequence_id: RaiseSequence,
        quiet: Quiet,
    ) -> Result<(), RaiseError> {
        let check_cancel = || {
            if token.is_cancelled() {
                return Err(RaiseError::RaiseCancelled);
            }
            Ok(())
        };
        check_cancel()?;

        let wids = this_ref.borrow().own_windows(wids);
        let Some(&first) = wids.first() else {
            warn!("Got empty list of wids to raise; this might misbehave");
            return Ok(());
        };
        let is_standard = this_ref.borrow().is_standard_window(first)?;

        check_cancel()?;

        let raising = Some(RAISING.lock());
        check_cancel()?;
        let mut this = this_ref.borrow_mut();

        let app = this.ax.app();
        let is_frontmost = super::trace("is_frontmost", this.pid, || this.ax.frontmost(&app))?;

        // Focus-follows-mouse can enqueue a final hover transition while the
        // pointer is moving off a window (for example, into the menu bar).
        // Reissuing make-key/raise for the window that is already focused is
        // not only redundant: it can visibly pulse focus and pull it back from
        // transient system UI. Complete the request without touching focus.
        //
        // Only elide a single-window request. Multi-window batches still need
        // their raises to establish the requested stacking order.
        if is_frontmost && this.main_window == Some(first) && wids.len() == 1 {
            trace!(?first, "Skipping raise for already focused window");
            this.send_event(Event::RaiseCompleted { window_id: first, sequence_id });
            return Ok(());
        }

        let made_key = this.make_key(first)?;
        let waits_for_activation = !is_frontmost && made_key && is_standard;
        if waits_for_activation {
            // Keep the WindowServer make-key request and AX raise adjacent, matching
            // yabai's focus ordering. If we wait for process activation first, macOS
            // can temporarily make the application's previous key window authoritative.
            // For apps with windows on multiple displays that produces a spurious
            // active-display hop before the requested window is finally raised.
            //
            // This deliberately starts the focus operation before the cancellable
            // activation wait. Cancellation can stop follow-up batch raises, but it
            // must not leave process activation detached from its target window.
            let window = this.window(first)?;
            super::trace("raise before activation wait", first, || {
                this.ax.raise(&window.elem)
            })?;

            let (quiet_activation, quiet_window_change) =
                activation_quiet(quiet, first, wids.len());
            Self::wait_for_activation(this_ref, this, quiet_activation, quiet_window_change, token)
                .await?;
            this = this_ref.borrow_mut();
        } else {
            trace!(
                "Not awaiting activation event. is_frontmost={is_frontmost:?} \
                made_key={made_key:?} is_standard={is_standard:?}"
            )
        }

        this.raise_in_order(&wids, waits_for_activation, sequence_id, quiet, raising)?;
        Ok(())
    }

    fn is_standard_window(&self, wid: WindowId) -> Result<bool, AxError> {
        let window = self.window(wid)?;
        Ok(self
            .ax
            .subrole(&window.elem)
            .map(|s| s == AX_STANDARD_WINDOW_SUBROLE)
            .unwrap_or(false))
    }

    /// Makes `wid` key through the window server, and answers whether it was. A window with no
    /// window-server id yet is not asked about.
    fn make_key(&self, wid: WindowId) -> Result<bool, AxError> {
        let Some(wsid) = self.ax.window_server_id(&self.window(wid)?.elem) else {
            debug!(
                ?wid,
                "Skipping make-key request because window has no server id yet"
            );
            return Ok(false);
        };
        if let Err(err) = self.ax.server().make_key_window(self.pid, wsid) {
            warn!(?self.pid, ?err, "Failed to activate app");
            return Ok(false);
        }
        Ok(true)
    }

    /// Raises `wids` in order and reports each, the first not again when the activation wait raised
    /// it. `raising` is let go after the last is reported, before the main window is read.
    fn raise_in_order(
        &mut self,
        wids: &[WindowId],
        first_raised: bool,
        sequence_id: RaiseSequence,
        quiet: Quiet,
        mut raising: Option<MutexGuard<'static, ()>>,
    ) -> Result<(), AxError> {
        for (i, &wid) in wids.iter().enumerate() {
            if first_raised && i == 0 {
                trace!(?wid, "Skipping duplicate raise after activation wait");
            } else {
                let window = self.window(wid)?;
                super::trace("raise", wid, || self.ax.raise(&window.elem))?;
            }

            trace!("Sending completion");
            self.send_event(Event::RaiseCompleted { window_id: wid, sequence_id });

            let is_last = i + 1 == wids.len();
            let quiet_if = if is_last {
                raising.take();
                (quiet == Quiet::Yes).then_some(wid)
            } else {
                None
            };

            if is_last {
                let main_window = self.on_main_window_changed(quiet_if, true);
                if main_window != Some(wid) {
                    warn!(
                        "Raise request failed to raise {desired:?}; instead got main_window={main_window:?}",
                        desired = self.window(wid).map(|w| &w.elem).ok(),
                    );
                }
            }
        }
        Ok(())
    }

    #[allow(
        clippy::await_holding_refcell_ref,
        reason = "the RefMut is dropped before the first await"
    )]
    async fn wait_for_activation(
        this_ref: &RefCell<Self>,
        mut this: RefMut<'_, Self>,
        quiet_activation: Quiet,
        quiet_window_change: Option<WindowId>,
        token: &CancellationToken,
    ) -> Result<(), RaiseError> {
        let app = this.ax.app();
        let (tx, rx) = oneshot::channel();
        if let Some((_, _, _, prev_tx)) =
            this.last_activated
                .replace((Instant::now(), quiet_activation, quiet_window_change, tx))
        {
            let _ = prev_tx.send(());
        }
        drop(this);
        trace!("Awaiting activation");
        tokio::pin!(rx);
        loop {
            select! {
                _ = &mut rx => break,
                _ = token.cancelled() => {
                    debug!("Raise cancelled while awaiting activation event");
                    return Err(RaiseError::RaiseCancelled);
                }
                _ = Timer::sleep(Duration::from_millis(10)) => {
                    if this_ref.borrow().ax.frontmost(&app).unwrap_or(false) {
                        trace!("Activation observed via frontmost polling");
                        break;
                    }
                }
            }
        }
        trace!("Activation complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use objc2_core_graphics::CGError;
    use rini_core::ids::WindowServerId;
    use rini_runloop::executor::Executor;

    use super::super::fixtures::{
        PID, app_with, completed, main_window_changes, poll_once, raisable,
    };
    use super::*;
    use crate::windows::domain::ax_events::AxNotificationKind;

    #[test]
    fn only_a_raise_s_last_window_takes_its_quiet_and_every_window_before_it_is_quiet() {
        let wid = WindowId::new(PID, 7);
        assert_eq!(activation_quiet(Quiet::Yes, wid, 1), (Quiet::Yes, Some(wid)));
        assert_eq!(activation_quiet(Quiet::No, wid, 1), (Quiet::No, None));
        for quiet in [Quiet::Yes, Quiet::No] {
            assert_eq!(activation_quiet(quiet, wid, 2), (Quiet::Yes, Some(wid)));
        }
    }

    /// A raise batch holding another application's window drops it and raises the rest, rather than
    /// end the thread.
    #[test]
    fn a_raise_holding_another_applications_window_drops_it_and_raises_the_rest() {
        let (state, sink, wid) = raisable(7);
        let foreign = WindowId::new(PID + 1, 9);
        let state = RefCell::new(state);
        let token = CancellationToken::new();

        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            vec![foreign, wid],
            &token,
            RaiseSequence(1),
            Quiet::No
        ));
        let done = raise
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));

        assert!(
            matches!(done, std::task::Poll::Ready(Ok(()))),
            "the raise finished"
        );
        assert_eq!(
            *state.borrow().ax.raised.borrow(),
            [7],
            "only its own window was raised"
        );
        assert_eq!(
            completed(&sink),
            [wid],
            "and only its own window reported raised"
        );
    }

    /// Raising the window that already has focus would pulse focus and can pull it back from
    /// transient system UI, so a raise of it alone is reported done and nothing is asked.
    #[test]
    fn a_raise_of_the_window_already_focused_is_skipped() {
        let (mut state, sink, wids) = app_with(&[7]);
        state.main_window = Some(wids[0]);
        let state = RefCell::new(state);
        let token = CancellationToken::new();

        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            wids.clone(),
            &token,
            RaiseSequence(1),
            Quiet::No
        ));

        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        assert!(state.borrow().ax.raised.borrow().is_empty(), "nothing raised");
        assert!(state.borrow().ax.made_key().is_empty(), "nothing made key");
        assert_eq!(completed(&sink), wids);
    }

    /// A batch needs every raise for the stacking it asks, the focused window's included.
    #[test]
    fn a_batch_holding_the_focused_window_is_still_raised() {
        let (mut state, sink, wids) = app_with(&[7, 8]);
        state.main_window = Some(wids[0]);
        let state = RefCell::new(state);
        let token = CancellationToken::new();

        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            wids.clone(),
            &token,
            RaiseSequence(1),
            Quiet::No
        ));

        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        assert_eq!(*state.borrow().ax.raised.borrow(), [7, 8]);
        assert_eq!(completed(&sink), wids);
    }

    /// Raising is last-wins, so a batch's order is the stacking it makes: each window is raised and
    /// reported in the order asked, and only the first is made key.
    #[test]
    fn a_batch_is_raised_and_reported_in_its_order() {
        let (state, sink, wids) = app_with(&[7, 8, 9]);
        let order = vec![wids[2], wids[0], wids[1]];
        let state = RefCell::new(state);
        let token = CancellationToken::new();

        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            order.clone(),
            &token,
            RaiseSequence(1),
            Quiet::No
        ));

        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        assert_eq!(*state.borrow().ax.raised.borrow(), [9, 7, 8]);
        assert_eq!(completed(&sink), order);
        assert_eq!(state.borrow().ax.made_key(), [(PID, WindowServerId::new(9))]);
    }

    /// A quiet raise makes the main-window change it causes quiet only for the last window.
    #[test]
    fn a_quiet_raise_is_quiet_when_its_last_window_becomes_main() {
        let (mut state, sink, wids) = app_with(&[7, 8]);
        state.ax.main_window = Some(8);
        let state = RefCell::new(state);
        let token = CancellationToken::new();

        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            wids.clone(),
            &token,
            RaiseSequence(1),
            Quiet::Yes
        ));

        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        assert_eq!(main_window_changes(&sink), [(Some(wids[1]), Quiet::Yes)]);
    }

    #[test]
    fn a_quiet_raise_that_leaves_another_window_main_announces_it_unquietly() {
        let (mut state, sink, wids) = app_with(&[7, 8]);
        state.ax.main_window = Some(7);
        let state = RefCell::new(state);
        let token = CancellationToken::new();

        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            wids.clone(),
            &token,
            RaiseSequence(1),
            Quiet::Yes
        ));

        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        assert_eq!(main_window_changes(&sink), [(Some(wids[0]), Quiet::No)]);
    }

    /// An application in the background is made key and its window raised at once, before the raise
    /// waits for the application to come to the front: waiting first lets macOS make its previous key
    /// window authoritative.
    #[test]
    fn a_raise_in_a_background_application_raises_then_waits_for_activation() {
        let (mut state, sink, wids) = app_with(&[7]);
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

        assert!(poll_once(raise.as_mut()).is_pending(), "waiting for activation");
        assert_eq!(state.borrow().ax.made_key(), [(PID, WindowServerId::new(7))]);
        assert_eq!(*state.borrow().ax.raised.borrow(), [7]);
        assert!(completed(&sink).is_empty(), "not reported done while waiting");
    }

    /// A background application keeps its main window, so being main is not being focused: a raise
    /// of a background application's main window is made key and raised like any other.
    #[test]
    fn a_raise_of_a_background_application_s_main_window_is_not_skipped() {
        let (mut state, sink, wids) = app_with(&[7]);
        state.ax.frontmost = false;
        state.main_window = Some(wids[0]);
        let state = RefCell::new(state);
        let token = CancellationToken::new();

        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            wids.clone(),
            &token,
            RaiseSequence(1),
            Quiet::No
        ));

        assert!(poll_once(raise.as_mut()).is_pending(), "waiting for activation");
        assert_eq!(state.borrow().ax.made_key(), [(PID, WindowServerId::new(7))]);
        assert_eq!(*state.borrow().ax.raised.borrow(), [7]);
        assert!(completed(&sink).is_empty(), "not reported done while waiting");
    }

    /// The application's activation ends the wait, and the window already raised is not raised again.
    #[test]
    fn a_raise_waiting_for_activation_finishes_when_the_application_activates() {
        let (mut state, sink, wids) = app_with(&[7]);
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

        state.borrow_mut().ax.frontmost = true;
        state
            .borrow_mut()
            .handle_notification(1, AxNotificationKind::ApplicationActivated, None);

        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        assert_eq!(*state.borrow().ax.raised.borrow(), [7], "raised once");
        assert_eq!(completed(&sink), wids);
    }

    /// A raise cancelled while it waits ends unreported. The make-key and raise it already made stay
    /// made: the activation they started must not lose its window.
    #[test]
    fn cancelling_a_raise_waiting_for_activation_ends_it_unreported() {
        let (mut state, sink, wids) = app_with(&[7]);
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

        token.cancel();

        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Err(RaiseError::RaiseCancelled))
        ));
        assert!(completed(&sink).is_empty(), "never reported done");
        assert_eq!(*state.borrow().ax.raised.borrow(), [7]);
    }

    #[test]
    fn a_raise_cancelled_before_it_starts_asks_nothing() {
        let (state, sink, wids) = app_with(&[7]);
        let state = RefCell::new(state);
        let token = CancellationToken::new();
        token.cancel();

        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            wids.clone(),
            &token,
            RaiseSequence(1),
            Quiet::No
        ));

        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Err(RaiseError::RaiseCancelled))
        ));
        assert!(state.borrow().ax.raised.borrow().is_empty());
        assert!(state.borrow().ax.made_key().is_empty());
        assert!(completed(&sink).is_empty());
    }

    /// An activation the application gives no notification of is seen by asking it, every 10ms,
    /// whether it is frontmost.
    #[test]
    fn activation_is_seen_through_frontmost_polling() {
        let (state, sink, wids) = app_with(&[7]);
        state.ax.frontmost_reads.borrow_mut().push_back(false);
        let state = Rc::new(RefCell::new(state));
        let outcome = Rc::new(RefCell::new(None));

        let (raising, finished, raised) = (state.clone(), outcome.clone(), wids.clone());
        Executor::run(async move {
            let token = CancellationToken::new();
            select! {
                result = State::handle_raise_request(&raising, raised, &token, RaiseSequence(1), Quiet::No) => {
                    *finished.borrow_mut() = Some(result.is_ok());
                }
                () = Timer::sleep(Duration::from_secs(5)) => {}
            }
        });

        assert_eq!(*outcome.borrow(), Some(true), "the raise finished");
        assert_eq!(
            *state.borrow().ax.raised.borrow(),
            [7],
            "raised once, before the wait"
        );
        assert!(
            state.borrow().last_activated.is_some(),
            "no notification ended it"
        );
        assert_eq!(completed(&sink), wids);
    }

    /// A raise waits for activation only after a make-key that worked, so one whose window could not
    /// be made key is raised at once.
    #[test]
    fn a_raise_whose_window_cannot_be_made_key_does_not_wait() {
        let (mut state, sink, wids) = app_with(&[7]);
        state.ax.frontmost = false;
        state.ax.server.fail_make_key_window(Some(CGError::Failure));
        let state = RefCell::new(state);
        let token = CancellationToken::new();

        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            wids.clone(),
            &token,
            RaiseSequence(1),
            Quiet::No
        ));

        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        assert_eq!(*state.borrow().ax.raised.borrow(), [7]);
        assert_eq!(completed(&sink), wids);
    }

    #[test]
    fn a_raise_of_a_window_with_no_server_id_is_not_made_key_and_does_not_wait() {
        let (mut state, sink, wids) = app_with(&[7]);
        state.ax.frontmost = false;
        state.ax.server_ids.remove(&7);
        let state = RefCell::new(state);
        let token = CancellationToken::new();

        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            wids.clone(),
            &token,
            RaiseSequence(1),
            Quiet::No
        ));

        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        assert!(state.borrow().ax.made_key().is_empty());
        assert_eq!(*state.borrow().ax.raised.borrow(), [7]);
        assert_eq!(completed(&sink), wids);
    }

    /// Only a standard window's raise waits for activation, so a dialog's is made key and raised at
    /// once.
    #[test]
    fn a_raise_of_a_window_that_is_not_standard_does_not_wait() {
        let (mut state, sink, wids) = app_with(&[7]);
        state.ax.frontmost = false;
        state.ax.subroles.insert(7, "AXDialog".to_owned());
        let state = RefCell::new(state);
        let token = CancellationToken::new();

        let mut raise = std::pin::pin!(State::handle_raise_request(
            &state,
            wids.clone(),
            &token,
            RaiseSequence(1),
            Quiet::No
        ));

        assert!(matches!(
            poll_once(raise.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        assert_eq!(state.borrow().ax.made_key(), [(PID, WindowServerId::new(7))]);
        assert_eq!(completed(&sink), wids);
    }
}
