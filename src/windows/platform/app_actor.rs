//! One actor per running app, on its own thread: observes the app through Accessibility and
//! carries out the reactor's `Request`s (frames, raises, close). Emits `crate::windows::event::Event`.
//!
//! This file is the thread's state and its loop; each job it does is a file under `app_actor/`,
//! listed in `src/windows/docs/README.md`.

mod activation;
#[cfg(test)]
mod fixtures;
mod launch;
mod lookup;
mod notifications;
mod raise;
mod registration;
mod requests;
mod subscriptions;

use std::cell::RefCell;
use std::time::Instant;

use objc2_application_services::AXError;
use rini_core::ids::{WindowId, WindowServerId, pid_t};
use rini_runloop::channel as channels;
use rustc_hash::FxHashMap as HashMap;
use tokio::select;
use tokio::sync::oneshot;
use tracing::{Span, debug, trace, warn};

pub use self::launch::spawn_app_thread;
use self::raise::RaiseRequest;
use crate::windows::domain::ax_events::AxNotificationKind;
use crate::windows::domain::request::{Quiet, Request};
use crate::windows::domain::transaction::{TransactionId, WindowTxStore};
use crate::windows::event::{Event, EventSink};
use crate::windows::platform::ax::element::Error as AxError;
use crate::windows::platform::ax::world::AxWorld;

struct State<W: AxWorld> {
    pid: pid_t,
    bundle_id: Option<String>,
    /// Everything this thread asks of the platform: Accessibility, the window server through
    /// `server()`, the pointer state and Command-W. Production installs `MacAx`; a test installs a
    /// fake whose elements are plain numbers. See `platform/ax/world.rs`.
    ax: W,
    events_tx: Box<dyn EventSink>,
    windows: HashMap<WindowId, AppWindowState<W::Element>>,
    elem_to_wid: HashMap<W::Element, WindowId>,
    last_window_idx: u32,
    main_window: Option<WindowId>,
    last_activated: Option<Activation>,
    pending_activation_quiet: Option<(Instant, Quiet)>,
    is_hidden: bool,
    is_frontmost: bool,
    raises_tx: channels::Sender<RaiseRequest>,
    tx_store: Option<WindowTxStore>,
}

/// An activation this thread asked for: when, whether it and the main-window change it brings are
/// quiet, and who waits for macOS to report it.
struct Activation {
    at: Instant,
    quiet: Quiet,
    quiet_window_change: Option<WindowId>,
    done: oneshot::Sender<()>,
}

struct AppWindowState<E> {
    pub elem: E,
    last_seen_txid: TransactionId,
    hidden_by_app: bool,
    window_server_id: Option<WindowServerId>,
    title: String,
}

impl<W: AxWorld> State<W> {
    fn txid_from_store(&self, wsid: Option<WindowServerId>) -> Option<TransactionId> {
        let store = self.tx_store.as_ref()?;
        let wsid = wsid?;
        let record = store.get(&wsid)?;
        record.target.map(|_| record.txid)
    }

    fn txid_for_window_state(&self, window: &AppWindowState<W::Element>) -> Option<TransactionId> {
        self.txid_from_store(window.window_server_id)
            .or_else(|| Self::some_txid(window.last_seen_txid))
    }

    fn some_txid(txid: TransactionId) -> Option<TransactionId> {
        if txid == TransactionId::default() {
            None
        } else {
            Some(txid)
        }
    }

    /// Ends when the requests and notifications do, dropping any raise under way. Raises never end
    /// on their own, since their sender lives in the state, and the thread lives as long as this task.
    async fn run(
        self,
        requests_rx: channels::Receiver<Request>,
        notifications_rx: channels::Receiver<(W::Element, AxNotificationKind, Option<WindowId>)>,
        raises_rx: channels::Receiver<RaiseRequest>,
    ) {
        let this = RefCell::new(self);
        select! {
            biased;
            () = Self::handle_incoming(&this, requests_rx, notifications_rx) => {}
            () = Self::handle_raises(&this, raises_rx) => {}
        }
    }

    async fn handle_incoming(
        this: &RefCell<Self>,
        mut requests_rx: channels::Receiver<Request>,
        mut notifications_rx: channels::Receiver<(
            W::Element,
            AxNotificationKind,
            Option<WindowId>,
        )>,
    ) {
        loop {
            let batch = select! {
                biased;
                req = requests_rx.recv() => {
                    let Some(req) = req else { break };
                    let mut batch = vec![req];
                    while let Ok(req) = requests_rx.try_recv() {
                        batch.push(req);
                    }
                    batch
                }
                notif = notifications_rx.recv() => {
                    let Some((_, (elem, notif, hinted_wid))) = notif else { break };
                    this.borrow_mut().handle_notification(elem, notif, hinted_wid);
                    continue;
                }
            };
            if Self::handle_request_batch(this, batch) {
                break;
            }
        }
    }

    fn handle_request_batch(this: &RefCell<Self>, batch: Vec<(Span, Request)>) -> bool {
        // All requests in this actor target the same application. Coalesce EUI
        // suppression across the entire drained burst instead of toggling the
        // app-level attribute once per window/request. Animation leases nest
        // with this batch lease through the same refcount.
        let disable_enhanced_ui = batch.iter().any(|(_, req)| req.disables_enhanced_ui());
        if disable_enhanced_ui {
            let mut state = this.borrow_mut();
            state.ax.suppress_enhanced_ui();
        }

        let mut should_terminate = false;
        for (span, request) in batch {
            let mut state = this.borrow_mut();
            let _guard = span.enter();
            debug!(?state.bundle_id, ?state.pid, ?request, "Got request");
            let request_dbg = format!("{request:?}");
            match state.handle_request(request) {
                Ok(true) => {
                    should_terminate = true;
                    break;
                }
                Ok(false) => (),
                Err(AxError::Ax(AXError::CannotComplete)) if state.ax.app_has_quit() => {
                    warn!(?state.bundle_id, ?state.pid, "Application terminated without notification");
                    state.send_event(Event::ApplicationThreadTerminated(state.pid));
                    should_terminate = true;
                    break;
                }
                Err(err) => {
                    warn!(?state.bundle_id, ?state.pid, request = %request_dbg, "Error handling request: {:?}", err);
                }
            }
        }

        if disable_enhanced_ui {
            let mut state = this.borrow_mut();
            state.ax.restore_enhanced_ui();
        }

        should_terminate
    }

    fn send_event(&self, event: Event) { self.events_tx.send(event); }
}

impl<W: AxWorld> Drop for State<W> {
    fn drop(&mut self) {
        if let Some(activation) = self.last_activated.take() {
            let _ = activation.done.send(());
        }
        self.ax.restore_enhanced_ui_if_needed();
    }
}

/// Time an Accessibility call and say what happened when it fails.
///
/// `about` names what the call was about — a window id, or the pid for an application-level call. It
/// is deliberately NOT the element.
///
/// Formatting an `AXUIElement` is an Accessibility round-trip: its `Debug` delegates to the Core
/// Foundation description, which queries the element for its role and title. So logging one costs a
/// call to the application, and on a wedged application it blocks.
fn trace<T>(
    desc: &str,
    about: impl std::fmt::Debug,
    f: impl FnOnce() -> Result<T, AxError>,
) -> Result<T, AxError> {
    let start = Instant::now();
    let out = f();
    let elapsed = start.elapsed();
    trace!(time = ?elapsed, ?about, "{desc:12}");
    if let Err(err) = &out {
        match err {
            AxError::Ax(ax_err)
                if matches!(
                    *ax_err,
                    AXError::CannotComplete | AXError::InvalidUIElement | AXError::Failure
                ) =>
            {
                debug!("{desc} failed with {err} - app may have quit or become unresponsive");
            }
            _ => {
                debug!("{desc} failed with {err} for {about:?}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::fixtures::{PID, Sink, raisable};
    use super::*;
    use crate::windows::domain::request::RaiseSequence;
    use crate::windows::platform::ax::world::FakeAx;

    /// Runs the actor over `requests` with a raise of `raised` queued ahead of them, and answers
    /// whether its task finished. Every sender stays open, so only a request can end it, and one poll
    /// is all it takes when everything it needs is already queued: a task still pending after that is
    /// waiting for more, which is a thread that never ends.
    fn run_with_a_raise_queued(
        mut state: State<FakeAx>,
        raised: WindowId,
        requests: Vec<Request>,
    ) -> bool {
        let (requests_tx, requests_rx) = channels::channel();
        let (_notifications_tx, notifications_rx) = channels::channel();
        let (raises_tx, raises_rx) = channels::channel();
        raises_tx.send(RaiseRequest {
            windows: vec![raised],
            token: CancellationToken::new(),
            sequence_id: RaiseSequence(1),
            quiet: Quiet::No,
        });
        state.raises_tx = raises_tx;
        for request in requests {
            requests_tx.send(request);
        }
        let mut task = std::pin::pin!(state.run(requests_rx, notifications_rx, raises_rx));
        let finished = task
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_ready();
        drop(requests_tx);
        finished
    }

    fn raised(sink: &Sink) -> bool {
        sink.0
            .borrow()
            .iter()
            .any(|event| matches!(event, Event::RaiseCompleted { .. }))
    }

    fn told_the_thread_ended(sink: &Sink) -> bool {
        sink.0
            .borrow()
            .iter()
            .any(|event| matches!(event, Event::ApplicationThreadTerminated(PID)))
    }

    /// The reactor's `Terminate` ends the thread's task, and a raise still queued goes with it rather
    /// than keep the thread, its observer, its elements and its window map for the life of rini.
    #[test]
    fn terminating_ends_the_task_with_a_raise_still_queued() {
        let (state, sink, wid) = raisable(7);

        assert!(
            run_with_a_raise_queued(state, wid, vec![Request::Terminate]),
            "the task finished"
        );
        assert!(told_the_thread_ended(&sink), "and the reactor heard so");
        assert!(!raised(&sink), "the queued raise was dropped, not carried out");
    }

    /// An application that quits without notice ends the thread's task at the first request that
    /// fails for it, a raise still queued or not.
    #[test]
    fn a_request_failing_with_the_application_gone_ends_the_task_with_a_raise_still_queued() {
        let (mut state, sink, wid) = raisable(7);
        state.ax.busy = true;
        state.ax.has_quit = true;

        assert!(
            run_with_a_raise_queued(state, wid, vec![Request::GetVisibleWindows]),
            "the task finished"
        );
        assert!(told_the_thread_ended(&sink), "and the reactor heard so");
        assert!(!raised(&sink), "the queued raise was dropped, not carried out");
    }
}
