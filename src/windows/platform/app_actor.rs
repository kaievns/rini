#![allow(non_upper_case_globals, reason = "Accessibility's notification names")]
//! One actor per running app, on its own thread: observes the app through Accessibility and
//! carries out the reactor's `Request`s (frames, raises, close). Emits `crate::windows::event::Event`.

use std::cell::RefCell;
use std::fmt::Debug;
use std::num::NonZeroU32;
use std::sync::LazyLock;
use std::thread;
use std::time::{Duration, Instant};

use objc2_app_kit::NSRunningApplication;
use objc2_application_services::AXError;
use objc2_core_foundation::CGRect;
use rini_core::ids::{WindowId, WindowServerId, pid_t};
use rini_runloop::channel as channels;
use rini_runloop::executor::Executor;
use rini_runloop::timer::Timer;
use rustc_hash::FxHashMap as HashMap;
use tokio::select;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug, info, instrument, trace, warn};

use crate::windows::domain::admissible;
use crate::windows::domain::ax_events::{
    AxFailure, AxNotificationKind, Handling, decode_notification_data, encode_notification_data,
    handling, is_gone,
};
use crate::windows::domain::frame_write::{FrameWrite, frame_write};
use crate::windows::domain::info::{AppInfo, WindowInfo, WindowServerInfo};
use crate::windows::domain::request::{AppThreadHandle, Quiet, Request};
use crate::windows::domain::transaction::{Requested, TransactionId, WindowTxStore};
use crate::windows::event::{Event, EventSink};
use crate::windows::platform::app::NSRunningApplicationExt;
use crate::windows::platform::ax::ask_again::AskAgain;
use crate::windows::platform::ax::element::{
    AX_STANDARD_WINDOW_SUBROLE, AX_WINDOW_ROLE, AXUIElement, Error as AxError,
};
use crate::windows::platform::ax::observer::Observer;
use crate::windows::platform::ax::world::{AxWorld, MacAx};
use crate::windows::platform::process::ProcessInfo;

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

const APP_NOTIFICATIONS: &[(AxNotificationKind, &str)] = &[
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

struct RaiseRequest(Vec<WindowId>, CancellationToken, u64, Quiet);

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

struct State<W: AxWorld> {
    pid: pid_t,
    bundle_id: Option<String>,
    /// Everything this thread asks of Accessibility. Production installs `MacAx`; a test installs a
    /// fake whose elements are plain numbers. See `platform/ax/world.rs`.
    ax: W,
    events_tx: Box<dyn EventSink>,
    windows: HashMap<WindowId, AppWindowState<W::Element>>,
    elem_to_wid: HashMap<W::Element, WindowId>,
    last_window_idx: u32,
    main_window: Option<WindowId>,
    last_activated: Option<(Instant, Quiet, Option<WindowId>, oneshot::Sender<()>)>,
    pending_activation_quiet: Option<(Instant, Quiet)>,
    is_hidden: bool,
    is_frontmost: bool,
    raises_tx: channels::Sender<RaiseRequest>,
    tx_store: Option<WindowTxStore>,
}

struct AppWindowState<E> {
    pub elem: E,
    last_seen_txid: TransactionId,
    hidden_by_app: bool,
    window_server_id: Option<WindowServerId>,
    title: String,
}

impl<W: AxWorld> State<W> {
    fn refresh_visible_windows(&mut self) -> Result<(), AxError> {
        let window_elems = match self.ax.windows(&self.ax.app()) {
            Ok(elems) => elems,
            Err(e) => {
                self.send_event(Event::WindowsDiscovered {
                    pid: self.pid,
                    new: Default::default(),
                    known_visible: Default::default(),
                });
                return Err(e);
            }
        };
        let server_info_by_id = self.visible_window_server_info_map(&window_elems);
        let mut new = Vec::with_capacity(window_elems.len());
        let mut known_visible = Vec::with_capacity(window_elems.len());

        for elem in window_elems {
            let wsid = self.ax.window_server_id(&elem);
            let hint = wsid.and_then(|id| server_info_by_id.get(&id).copied());
            let mut info = match self.ax.window_info(&elem, hint) {
                Ok((info, _)) => info,
                Err(err) => {
                    let id = self.id(&elem).ok();
                    trace!(?id, ?err, "Failed to refresh window info; will retry later");
                    continue;
                }
            };
            if !Self::has_visible_cg_peer(wsid, hint) && !info.is_minimized {
                trace!(pid = ?self.pid, ?wsid, "Ignoring AX window without a visible CG window");
                continue;
            }

            let wid = if let Ok(wid) = self.id(&elem) {
                self.complete_info(&elem, &mut info);
                // The WindowServer id is stable across sleep/display transitions, but
                // the corresponding AXUIElement is not. `id` intentionally resolves the
                // fresh element to the existing wid by that stable id; refresh the actor's
                // handle as well or subsequent frame writes keep targeting the pre-wake
                // element and can never recover.
                self.rebind_window_element(wid, elem, &info);
                wid
            } else {
                let Some((registered, wid, _)) = self.register_window(elem, hint) else {
                    continue;
                };
                info = registered;
                wid
            };

            if !info.is_minimized {
                known_visible.push(wid);
            }
            new.push((wid, info));
        }

        self.send_event(Event::WindowsDiscovered {
            pid: self.pid,
            new,
            known_visible,
        });
        self.on_main_window_changed(None, true);
        Ok(())
    }

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

    async fn handle_raises(this: &RefCell<Self>, mut rx: channels::Receiver<RaiseRequest>) {
        while let Some((span, raise)) = rx.recv().await {
            let RaiseRequest(wids, token, sequence_id, quiet) = raise;
            if let Err(e) = Self::handle_raise_request(this, wids, &token, sequence_id, quiet)
                .instrument(span)
                .await
            {
                debug!("Raise request failed: {e:?}");
            }
        }
    }

    #[instrument(skip_all, fields(?info))]
    #[must_use]
    fn init(&mut self, handle: AppThreadHandle, info: AppInfo, patience: Duration) -> bool {
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

    #[instrument(skip_all, fields(pid = ?self.pid, ?request))]
    fn handle_request(&mut self, request: Request) -> Result<bool, AxError> {
        match request {
            Request::Terminate => {
                self.send_event(Event::ApplicationThreadTerminated(self.pid));
                return Ok(true);
            }
            Request::WindowMaybeDestroyed(wid) => {
                if wid.pid != self.pid {
                    return Ok(false);
                }

                // If we don't know this window, nothing to verify.
                if !self.windows.contains_key(&wid) {
                    return Ok(false);
                }

                // Trigger a visible windows refresh. If the window is gone, the reactor
                // will detect it via missing membership and tear down state.
                self.refresh_visible_windows()?;
                return Ok(false);
            }
            Request::CloseWindow(window_server_id) => {
                if let Some(wsid) = window_server_id
                    && let Err(err) = self.ax.server().make_key_window(self.pid, wsid)
                {
                    warn!(pid = self.pid, ?wsid, ?err, "Failed to focus close target");
                    return Ok(false);
                }
                if !self.ax.post_command_w() {
                    warn!(pid = self.pid, ?window_server_id, "Failed to post Command-W");
                }
            }
            Request::GetVisibleWindows => {
                self.refresh_visible_windows()?;
            }
            Request::ApplicationGloballyActivated(pid) => {
                if pid == self.pid {
                    self.on_global_activation()?;
                }
            }
            Request::SetWindowFrame(wid, desired, txid, _) => {
                let (elem, server_id) = match self.window_mut(wid) {
                    Ok(window) => {
                        window.last_seen_txid = txid;
                        (window.elem.clone(), window.window_server_id)
                    }
                    Err(err) => match err {
                        AxError::Ax(code) => {
                            if self.handle_ax_error(wid, &code) {
                                return Ok(false);
                            }
                            return Err(AxError::Ax(code));
                        }
                        AxError::NotFound => return Ok(false),
                    },
                };

                let frame = match self.write_frame(wid, &elem, server_id, desired)? {
                    Some(frame) => frame,
                    None => return Ok(false),
                };

                self.send_event(Event::WindowFrameChanged(
                    wid,
                    frame,
                    Some(txid),
                    Requested(true),
                    None,
                ));
            }
            Request::SetBatchWindowFrame(frames, txid, _) => {
                for (wid, desired) in frames {
                    let (elem, server_id) = match self.window_mut(wid) {
                        Ok(window) => {
                            window.last_seen_txid = txid;
                            (window.elem.clone(), window.window_server_id)
                        }
                        Err(err) => match err {
                            AxError::Ax(code) => {
                                if self.handle_ax_error(wid, &code) {
                                    continue;
                                }
                                return Err(AxError::Ax(code));
                            }
                            AxError::NotFound => continue,
                        },
                    };

                    let frame = match self.write_frame(wid, &elem, server_id, desired)? {
                        Some(frame) => frame,
                        None => continue,
                    };

                    self.send_event(Event::WindowFrameChanged(
                        wid,
                        frame,
                        Some(txid),
                        Requested(true),
                        None,
                    ));
                }
            }
            Request::SetWorkspaceSwitchPositions(positions, txid, _) => {
                for (wid, position) in positions {
                    let elem = match self.window_mut(wid) {
                        Ok(window) => {
                            window.last_seen_txid = txid;
                            window.elem.clone()
                        }
                        Err(err) => match err {
                            AxError::Ax(code) => {
                                if self.handle_ax_error(wid, &code) {
                                    continue;
                                }
                                return Err(AxError::Ax(code));
                            }
                            AxError::NotFound => continue,
                        },
                    };

                    let _ = self.ax.set_position(&elem, position);

                    // Preserve the existing per-window acknowledgement semantics. In
                    // particular, report the frame AX actually accepted rather than the
                    // requested position combined with a cached size.
                    let frame = match self
                        .handle_ax_result(wid, trace("frame", wid, || self.ax.frame(&elem)))?
                    {
                        Some(frame) => frame,
                        None => continue,
                    };

                    self.send_event(Event::WindowFrameChanged(
                        wid,
                        frame,
                        Some(txid),
                        Requested(true),
                        None,
                    ));
                }
            }
            Request::Raise(wids, token, sequence_id, quiet) => {
                self.raises_tx.send(RaiseRequest(wids, token, sequence_id, quiet));
            }
        }
        Ok(false)
    }

    /// Puts the window at `desired` and answers the frame read back after the last write.
    fn write_frame(
        &mut self,
        wid: WindowId,
        elem: &W::Element,
        server_id: Option<WindowServerId>,
        desired: CGRect,
    ) -> Result<Option<CGRect>, AxError> {
        let current = server_id
            .and_then(|id| self.ax.server().server_frame(id))
            .map(|frame| frame.size);
        if frame_write(current, desired.size) == FrameWrite::PositionOnly {
            let _ = self.ax.set_position(elem, desired.origin);
            let Some(landed) =
                self.handle_ax_result(wid, trace("frame", wid, || self.ax.frame(elem)))?
            else {
                return Ok(None);
            };
            if frame_write(Some(landed.size), desired.size) == FrameWrite::PositionOnly {
                return Ok(Some(landed));
            }
        }
        let _ = self.ax.set_size(elem, desired.size);
        let _ = self.ax.set_position(elem, desired.origin);
        let _ = self.ax.set_size(elem, desired.size);
        self.handle_ax_result(wid, trace("frame", wid, || self.ax.frame(elem)))
    }

    #[instrument(skip_all, fields(pid = ?self.pid, ?notif))]
    fn handle_notification(
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
            AxNotificationKind::WindowCreated => {
                if self.id(&elem).is_ok() {
                    return;
                }
                let Some((window, wid, window_server_info)) = self.register_window(elem, None)
                else {
                    return;
                };
                let window_server_info = window_server_info
                    .or_else(|| window.sys_id.and_then(|id| self.ax.server().get_window(id)));
                self.send_event(Event::WindowCreated(
                    wid,
                    window,
                    window_server_info,
                    self.ax.mouse_state(),
                ));
            }
            AxNotificationKind::MenuOpened => self.send_event(Event::MenuOpened(self.pid)),
            AxNotificationKind::MenuClosed => self.send_event(Event::MenuClosed(self.pid)),
            AxNotificationKind::WindowDestroyed => {
                let Ok(wid) = self.wid_for_notification(&elem, hinted_wid) else {
                    return;
                };
                // A refreshed AXUIElement can reuse the same stable WindowServer-backed
                // WindowId. Removing by the callback's encoded wid would then let a late
                // destroy notification for the superseded element tear down the replacement.
                // Only the element currently bound to this wid owns its lifetime.
                if !self.is_current_window_element(wid, &elem) {
                    trace!(?wid, "Ignoring destroy notification for superseded AX element");
                    return;
                }
                if self.remove_window(wid).is_none() {
                    return;
                }
                self.send_event(Event::WindowDestroyed(wid));

                self.on_main_window_changed(Some(wid), false);
            }
            AxNotificationKind::WindowMoved | AxNotificationKind::WindowResized => {
                let Ok(wid) = self.wid_for_notification(&elem, hinted_wid) else {
                    return;
                };
                if !self.is_current_window_element(wid, &elem) {
                    trace!(?wid, ?notif, "Ignoring notification for superseded AX element");
                    return;
                }

                let mouse_state = self.ax.mouse_state();
                let txid = match self.window(wid) {
                    Ok(window) => self.txid_for_window_state(window),
                    Err(err) => {
                        match err {
                            AxError::Ax(code) => {
                                if self.handle_ax_error(wid, &code) {
                                    return;
                                }
                            }
                            AxError::NotFound => {}
                        }
                        return;
                    }
                };
                let frame = match self.ax.frame(&elem) {
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
                self.send_event(Event::WindowFrameChanged(
                    wid,
                    frame,
                    txid,
                    Requested(false),
                    mouse_state,
                ));
            }
            AxNotificationKind::WindowMiniaturized => {
                let Ok(wid) = self.wid_for_notification(&elem, hinted_wid) else {
                    return;
                };
                let Some(window) = self.windows.get_mut(&wid).filter(|window| window.elem == elem)
                else {
                    trace!(?wid, "Ignoring miniaturize for superseded AX element");
                    return;
                };
                window.hidden_by_app = false;
                self.send_event(Event::WindowMinimized(wid));
            }
            AxNotificationKind::WindowDeminiaturized => {
                let Ok(wid) = self.wid_for_notification(&elem, hinted_wid) else {
                    return;
                };
                let Some(window) = self.windows.get_mut(&wid).filter(|window| window.elem == elem)
                else {
                    trace!(?wid, "Ignoring deminiaturize for superseded AX element");
                    return;
                };
                window.hidden_by_app = false;
                self.send_event(Event::WindowDeminiaturized(wid));
            }
            AxNotificationKind::TitleChanged => {
                let Ok(wid) = self.wid_for_notification(&elem, hinted_wid) else {
                    return;
                };
                if !self.is_current_window_element(wid, &elem) {
                    trace!(?wid, "Ignoring title change for superseded AX element");
                    return;
                }
                match self.ax.title(&elem) {
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
        }
    }
}

#[derive(Debug)]
#[allow(dead_code, reason = "used by Debug impls")]
enum RaiseError {
    RaiseCancelled,
    AXError(AxError),
}

impl From<AxError> for RaiseError {
    fn from(value: AxError) -> Self { Self::AXError(value) }
}

impl<W: AxWorld> State<W> {
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
    async fn handle_raise_request(
        this_ref: &RefCell<Self>,
        wids: Vec<WindowId>,
        token: &CancellationToken,
        sequence_id: u64,
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
        let is_standard = {
            let this = this_ref.borrow();
            let window = this.window(first)?;
            this.ax
                .subrole(&window.elem)
                .map(|s| s == AX_STANDARD_WINDOW_SUBROLE)
                .unwrap_or(false)
        };

        check_cancel()?;

        static MUTEX: LazyLock<parking_lot::Mutex<()>> =
            LazyLock::new(|| parking_lot::Mutex::new(()));
        let mut mutex_guard = Some(MUTEX.lock());
        check_cancel()?;
        let mut this = this_ref.borrow_mut();

        let app = this.ax.app();
        let is_frontmost = trace("is_frontmost", this.pid, || this.ax.frontmost(&app))?;

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

        let window_server_id = this.ax.window_server_id(&this.window(first)?.elem);
        if window_server_id.is_none() {
            debug!(
                ?first,
                "Skipping make-key request because window has no server id yet"
            );
        }
        let make_key_result =
            window_server_id.map(|wsid| this.ax.server().make_key_window(this.pid, wsid));
        if let Some(Err(err)) = &make_key_result {
            warn!(?this.pid, ?err, "Failed to activate app");
        }

        let waits_for_activation =
            !is_frontmost && make_key_result.as_ref().is_some_and(Result::is_ok) && is_standard;
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
            trace("raise before activation wait", first, || {
                this.ax.raise(&window.elem)
            })?;

            if wids.len() == 1 {
                // `quiet` only applies if the first window is also the last.
                let quiet_window_change = (quiet == Quiet::Yes).then_some(first);
                Self::wait_for_activation(this_ref, this, quiet, quiet_window_change, token)
                    .await?;
            } else {
                // Windows before the last are always quiet.
                Self::wait_for_activation(this_ref, this, Quiet::Yes, Some(first), token).await?;
            }
            this = this_ref.borrow_mut();
        } else {
            trace!(
                "Not awaiting activation event. is_frontmost={is_frontmost:?} \
                make_key_result={make_key_result:?} is_standard={is_standard:?}"
            )
        }

        for (i, &wid) in wids.iter().enumerate() {
            if waits_for_activation && i == 0 {
                trace!(?wid, "Skipping duplicate raise after activation wait");
            } else {
                let window = this.window(wid)?;
                trace("raise", wid, || this.ax.raise(&window.elem))?;
            }

            trace!("Sending completion");
            this.send_event(Event::RaiseCompleted { window_id: wid, sequence_id });

            let is_last = i + 1 == wids.len();
            let quiet_if = if is_last {
                mutex_guard.take();
                (quiet == Quiet::Yes).then_some(wid)
            } else {
                None
            };

            if is_last {
                let main_window = this.on_main_window_changed(quiet_if, true);
                if main_window != Some(wid) {
                    warn!(
                        "Raise request failed to raise {desired:?}; instead got main_window={main_window:?}",
                        desired = this.window(wid).map(|w| &w.elem).ok(),
                    );
                }
            }
        }

        Ok(())
    }

    fn on_main_window_changed(
        &mut self,
        quiet_if: Option<WindowId>,
        allow_register: bool,
    ) -> Option<WindowId> {
        let elem = match trace("main_window", self.pid, || self.ax.main_window(&self.ax.app())) {
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

    fn on_ax_activation_changed(&mut self) -> Result<(), AxError> {
        let is_frontmost = trace("is_frontmost", self.pid, || self.ax.frontmost(&self.ax.app()))?;
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

    fn on_global_activation(&mut self) -> Result<(), AxError> {
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

    #[allow(
        clippy::await_holding_refcell_ref,
        reason = "the RefMut is dropped before the first await"
    )]
    async fn wait_for_activation(
        this_ref: &RefCell<Self>,
        mut this: std::cell::RefMut<'_, Self>,
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
            let minimized = match trace("minimized", wid, || self.ax.minimized(&window.elem)) {
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

    #[must_use]
    fn register_window(
        &mut self,
        elem: W::Element,
        server_info_hint: Option<WindowServerInfo>,
    ) -> Option<(WindowInfo, WindowId, Option<WindowServerInfo>)> {
        let Ok((mut info, server_info)) = self.ax.window_info(&elem, server_info_hint) else {
            return None;
        };
        let candidate = admissible::Candidate {
            has_visible_peer: Self::has_visible_cg_peer(info.sys_id, server_info),
            is_minimized: info.is_minimized,
            bundle_id: info.bundle_id.as_deref(),
            path: info.path.as_ref().and_then(|p| p.to_str()),
            ax_role: info.ax_role.as_deref(),
        };
        if let Some(reason) = admissible::rejection(&candidate) {
            trace!(
                pid = ?self.pid,
                sys_id = ?info.sys_id,
                bundle_id = ?info.bundle_id,
                role = ?info.ax_role,
                ?reason,
                "Not registering this AX window"
            );
            return None;
        }

        self.complete_info(&elem, &mut info);

        let window_server_id = info.sys_id.filter(|sid| sid.as_nonzero().is_some()).or_else(|| {
            let id = self.ax.window_server_id(&elem);
            if id.is_none() {
                info!(pid = ?self.pid, "Could not get window server id for a new AX window");
            }
            id
        });

        let idx = window_server_id.and_then(WindowServerId::as_nonzero).unwrap_or_else(|| {
            self.last_window_idx += 1;
            NonZeroU32::new(self.last_window_idx).unwrap()
        });
        let wid = WindowId { pid: self.pid, idx };
        if self.windows.contains_key(&wid) {
            trace!(?wid, "Window already registered; skipping duplicate");
            return None;
        }

        if !self.register_window_notifications(&elem, wid) {
            return None;
        }
        let hidden_by_app = self.is_hidden;
        let last_seen_txid = self.txid_from_store(window_server_id).unwrap_or_default();

        let old = self.windows.insert(wid, AppWindowState {
            elem: elem.clone(),
            last_seen_txid,
            hidden_by_app,
            window_server_id,
            title: info.title.clone(),
        });
        debug_assert!(old.is_none(), "Duplicate window id {wid:?}");
        self.elem_to_wid.insert(elem, wid);
        if hidden_by_app {
            self.send_event(Event::WindowMinimized(wid));
        }
        Some((info, wid, server_info))
    }

    /// What Accessibility's own read leaves out of `info`: whether a window of an application
    /// `admissible::needs_title_element_to_be_standard` names is standard, and whether the window
    /// server gives it a parent.
    fn complete_info(&self, elem: &W::Element, info: &mut WindowInfo) {
        if admissible::needs_title_element_to_be_standard(self.bundle_id.as_deref())
            && self.ax.read_attribute(elem, "AXTitleUIElement").is_err()
        {
            info.is_standard = false;
        }
        info.is_root =
            info.sys_id.is_none_or(|wsid| self.ax.server().window_parent(wsid).is_none());
    }

    fn register_window_notifications(&self, elem: &W::Element, wid: WindowId) -> bool {
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

    fn rebind_window_element(&mut self, wid: WindowId, elem: W::Element, info: &WindowInfo) {
        let Some(old_elem) = self.windows.get(&wid).map(|window| window.elem.clone()) else {
            return;
        };
        if old_elem == elem {
            return;
        }

        // Move observer ownership before replacing the handle. Removing from an
        // invalid old element can legitimately fail; Observer retains its callback
        // context in that case, so a late notification remains memory-safe and its
        // encoded wid still resolves to this logical window.
        self.remove_window_notifications(&old_elem);
        if !self.register_window_notifications(&elem, wid) {
            // Keep the last usable binding and restore its notifications when the
            // replacement cannot yet be observed. A later AXWindows refresh retries.
            self.remove_window_notifications(&elem);
            let _ = self.register_window_notifications(&old_elem, wid);
            return;
        }

        self.elem_to_wid.remove(&old_elem);
        self.elem_to_wid.insert(elem.clone(), wid);
        if let Some(window) = self.windows.get_mut(&wid) {
            window.elem = elem;
            window.window_server_id = info.sys_id.or(window.window_server_id);
            window.title = info.title.clone();
        }
        debug!(?wid, "Rebound window to refreshed AX element");
    }

    fn remove_window_notifications(&self, elem: &W::Element) {
        for &(_, notif) in WINDOW_NOTIFICATIONS {
            self.ax.unwatch(elem, notif);
        }
    }

    fn visible_window_server_info_map(
        &self,
        window_elements: &[W::Element],
    ) -> HashMap<WindowServerId, WindowServerInfo> {
        let wsids: Vec<WindowServerId> = window_elements
            .iter()
            .filter_map(|elem| self.ax.window_server_id(elem))
            .collect();
        let mut info_by_id = HashMap::with_capacity_and_hasher(wsids.len(), Default::default());
        for info in self.ax.server().get_windows(&wsids) {
            info_by_id.insert(info.id, info);
        }
        info_by_id
    }

    #[inline]
    fn has_visible_cg_peer(wsid: Option<WindowServerId>, hint: Option<WindowServerInfo>) -> bool {
        admissible::has_visible_peer(wsid.is_some(), hint.is_some())
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

    fn handle_ax_error(&mut self, wid: WindowId, err: &AXError) -> bool {
        if is_gone(Self::failure_of(&AxError::Ax(*err))) {
            if self.remove_window(wid).is_some() {
                self.send_event(Event::WindowDestroyed(wid));
                self.on_main_window_changed(Some(wid), false);
            }
            return true;
        }

        false
    }

    fn handle_ax_result<T>(
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
                if let AxError::Ax(code) = error {
                    self.handle_ax_error(wid, &code);
                }
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

    fn remove_stale_windows(&mut self) {
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

    fn send_event(&self, event: Event) { self.events_tx.send(event); }

    fn window(&self, wid: WindowId) -> Result<&AppWindowState<W::Element>, AxError> {
        self.check_own(wid)?;
        self.windows.get(&wid).ok_or(AxError::NotFound)
    }

    fn window_mut(&mut self, wid: WindowId) -> Result<&mut AppWindowState<W::Element>, AxError> {
        self.check_own(wid)?;
        self.windows.get_mut(&wid).ok_or(AxError::NotFound)
    }

    /// Another application's window is not found here, and says so in the log.
    fn check_own(&self, wid: WindowId) -> Result<(), AxError> {
        if wid.pid == self.pid {
            return Ok(());
        }
        warn!(?wid, pid = ?self.pid, "asked about another application's window");
        Err(AxError::NotFound)
    }

    fn id(&self, elem: &W::Element) -> Result<WindowId, AxError> {
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

    fn wid_for_notification(
        &self,
        elem: &W::Element,
        hinted_wid: Option<WindowId>,
    ) -> Result<WindowId, AxError> {
        hinted_wid
            .filter(|wid| wid.pid == self.pid)
            .or_else(|| self.id(elem).ok())
            .ok_or(AxError::NotFound)
    }

    fn is_current_window_element(&self, wid: WindowId, elem: &W::Element) -> bool {
        self.windows.get(&wid).is_some_and(|window| window.elem == *elem)
    }

    fn remove_window(&mut self, wid: WindowId) -> Option<AppWindowState<W::Element>> {
        let window = self.windows.remove(&wid)?;
        self.elem_to_wid.remove(&window.elem);
        Some(window)
    }
}

impl<W: AxWorld> Drop for State<W> {
    fn drop(&mut self) {
        if let Some((_, _, _, tx)) = self.last_activated.take() {
            let _ = tx.send(());
        }
        self.ax.restore_enhanced_ui_if_needed();
    }
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

/// Time an Accessibility call and say what happened when it fails.
///
/// `about` names what the call was about — a window id, or the pid for an application-level call. It
/// is deliberately NOT the element.
///
/// Formatting an `AXUIElement` is an Accessibility round-trip: its `Debug` delegates to the Core
/// Foundation description, which queries the element for its role and title. So logging one costs a
/// call to the application, and on a wedged application it blocks. The hot-path `trace!` had the
/// field commented out for exactly that reason, but the error arm below still formatted the element —
/// in the branch reached when the application is hung, which is the worst possible moment for another
/// round-trip. Nine call sites threaded an element through this function to serve that one line.
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
    use std::cell::RefCell;
    use std::rc::Rc;

    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_core_graphics::CGError;

    use super::*;
    use crate::windows::platform::ax::world::FakeAx;
    use crate::windows::platform::mouse::MouseState;

    /// Collects what the actor told the reactor.
    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<Event>>>);

    // The actor's thread owns its sink; a test drives it on one thread and never sends it anywhere.
    unsafe impl Send for Sink {}

    impl EventSink for Sink {
        fn send(&self, event: Event) { self.0.borrow_mut().push(event); }
    }

    const PID: pid_t = 501;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    /// The window server's own record of a window, which the real path reads before registering.
    /// Without one `admissible::has_visible_peer` refuses a window that HAS a server id, because an
    /// id the server does not report back means the window is not on screen.
    fn peer(window: u32, frame: CGRect) -> Option<WindowServerInfo> {
        Some(WindowServerInfo {
            id: WindowServerId::new(window),
            pid: PID,
            layer: 0,
            frame,
            min_frame: CGSize::ZERO,
            max_frame: CGSize::ZERO,
        })
    }

    fn state_with(ax: FakeAx) -> (State<FakeAx>, Sink) {
        let sink = Sink::default();
        let (raises_tx, _raises_rx) = channels::channel();
        let state = State {
            pid: PID,
            bundle_id: Some("com.example.app".to_owned()),
            ax,
            events_tx: Box::new(sink.clone()),
            windows: HashMap::default(),
            elem_to_wid: HashMap::default(),
            last_window_idx: 0,
            main_window: None,
            last_activated: None,
            pending_activation_quiet: None,
            is_hidden: false,
            is_frontmost: false,
            raises_tx,
            tx_store: None,
        };
        (state, sink)
    }

    /// A frame write that changes the size is size, then position, then size AGAIN.
    ///
    /// Not a typo and not belt-and-braces. AppKit clamps a size against the window's CURRENT position
    /// — a window near the right edge of a screen cannot grow until it has moved — so sizing before
    /// the move can be refused, and sizing after it is what actually lands. The first size is what
    /// lets the move succeed for a window that has to grow and shift at once.
    ///
    /// Nothing pinned this order before: the file had no tests, and the three lines look redundant
    /// enough that a tidy-up would drop one.
    #[test]
    fn a_resize_sizes_then_moves_then_sizes_again() {
        let window = 7;
        let (mut state, _sink) =
            state_with(FakeAx::with_one_window(window, rect(0., 0., 100., 100.)));
        state
            .ax
            .server
            .set_server_frame(WindowServerId::new(window), Some(rect(0., 0., 100., 100.)));
        let wid = state
            .register_window(window, peer(window, rect(0., 0., 100., 100.)))
            .expect("the window registers")
            .1;
        state.ax.positions.borrow_mut().clear();
        state.ax.sizes.borrow_mut().clear();

        let desired = rect(500., 300., 800., 600.);
        state
            .handle_request(Request::SetWindowFrame(
                wid,
                desired,
                TransactionId::default(),
                false,
            ))
            .expect("the write succeeds");

        assert_eq!(
            state.ax.sizes.borrow().as_slice(),
            &[(window, desired.size), (window, desired.size)],
            "sized twice, before and after the move"
        );
        assert_eq!(
            state.ax.positions.borrow().as_slice(),
            &[(window, desired.origin)],
            "moved once, between the two sizings"
        );
    }

    /// A move that keeps the window's size writes the position alone. See "Writing a frame" in
    /// `specs/windows.md`.
    #[test]
    fn a_same_size_move_writes_the_position_alone() {
        let window = 7;
        let (mut state, _sink) =
            state_with(FakeAx::with_one_window(window, rect(0., 0., 800., 600.)));
        state
            .ax
            .server
            .set_server_frame(WindowServerId::new(window), Some(rect(0., 0., 800., 600.)));
        let wid = state
            .register_window(window, peer(window, rect(0., 0., 800., 600.)))
            .expect("the window registers")
            .1;
        state.ax.positions.borrow_mut().clear();
        state.ax.sizes.borrow_mut().clear();

        let desired = rect(500., 300., 800., 600.);
        state
            .handle_request(Request::SetWindowFrame(
                wid,
                desired,
                TransactionId::default(),
                false,
            ))
            .expect("the write succeeds");

        assert!(state.ax.sizes.borrow().is_empty(), "never sized");
        assert_eq!(
            state.ax.positions.borrow().as_slice(),
            &[(window, desired.origin)],
            "moved once"
        );
    }

    /// A same-size move whose read-back size is off is written again in full, and the reactor hears
    /// the frame read after that, not the one the move left.
    #[test]
    fn a_same_size_move_that_lands_at_another_size_is_written_in_full() {
        let window = 7;
        let (mut state, sink) =
            state_with(FakeAx::with_one_window(window, rect(0., 0., 800., 600.)));
        state
            .ax
            .server
            .set_server_frame(WindowServerId::new(window), Some(rect(0., 0., 800., 600.)));
        let wid = state
            .register_window(window, peer(window, rect(0., 0., 800., 600.)))
            .expect("the window registers")
            .1;
        state.ax.positions.borrow_mut().clear();
        state.ax.sizes.borrow_mut().clear();
        sink.0.borrow_mut().clear();

        let desired = rect(2000., 0., 800., 600.);
        let clamped = rect(2000., 0., 640., 600.);
        state.ax.frame_reads.borrow_mut().insert(window, [clamped, desired].into());
        state
            .handle_request(Request::SetWindowFrame(
                wid,
                desired,
                TransactionId::default(),
                false,
            ))
            .expect("the write succeeds");

        assert_eq!(
            state.ax.positions.borrow().as_slice(),
            &[(window, desired.origin), (window, desired.origin)],
            "moved alone, then moved again between the two sizings"
        );
        assert_eq!(
            state.ax.sizes.borrow().as_slice(),
            &[(window, desired.size), (window, desired.size)],
            "sized twice once the move came back at another size"
        );
        let announced = sink.0.borrow();
        assert!(
            matches!(
                announced.as_slice(),
                [Event::WindowFrameChanged(w, frame, ..)] if *w == wid && *frame == desired
            ),
            "{announced:?}"
        );
    }

    /// A batch decides each window on its own size: the one the window server has at the desired
    /// size moves alone, the one whose size cannot be read is sized around its move.
    #[test]
    fn a_batch_writes_each_window_by_its_own_size() {
        let (kept, unread) = (7, 8);
        let mut ax = FakeAx::with_one_window(kept, rect(0., 0., 800., 600.));
        ax.windows.push(unread);
        ax.describe_standard_window(unread, rect(800., 0., 400., 600.));
        ax.server
            .set_server_frame(WindowServerId::new(kept), Some(rect(0., 0., 800., 600.)));
        let (mut state, _sink) = state_with(ax);
        let kept_wid = state
            .register_window(kept, peer(kept, rect(0., 0., 800., 600.)))
            .expect("registers")
            .1;
        let unread_wid = state
            .register_window(unread, peer(unread, rect(800., 0., 400., 600.)))
            .expect("registers")
            .1;
        state.ax.positions.borrow_mut().clear();
        state.ax.sizes.borrow_mut().clear();

        let (to_kept, to_unread) = (rect(400., 0., 800., 600.), rect(1200., 0., 400., 600.));
        state
            .handle_request(Request::SetBatchWindowFrame(
                vec![(kept_wid, to_kept), (unread_wid, to_unread)],
                TransactionId::default(),
                false,
            ))
            .expect("the writes succeed");

        assert_eq!(
            state.ax.sizes.borrow().as_slice(),
            &[(unread, to_unread.size), (unread, to_unread.size)],
            "only the window of unknown size is sized"
        );
        assert_eq!(
            state.ax.positions.borrow().as_slice(),
            &[(kept, to_kept.origin), (unread, to_unread.origin)],
            "both move, in the batch's order"
        );
    }

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
        sink.0.borrow_mut().clear();

        state.remove_stale_windows();

        assert!(state.windows.contains_key(&kept_wid), "the live window stays");
        assert!(!state.windows.contains_key(&died_wid), "the dead one goes");
        let announced = sink.0.borrow();
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
        sink.0.borrow_mut().clear();

        // The application goes under load AFTER the window is known: every read now fails with
        // "could not complete", which is what a wedged or busy application looks like.
        state.ax.busy = true;
        state.remove_stale_windows();

        assert!(
            state.windows.contains_key(&wid),
            "a busy application has not lost its windows"
        );
        assert!(sink.0.borrow().is_empty(), "and nothing is announced");
    }

    /// The rule `admissible::needs_title_element_to_be_standard` names, end to end: for the
    /// applications it lists, a window whose `AXTitleUIElement` cannot be read is not standard.
    #[test]
    fn a_window_without_a_readable_title_element_is_not_standard() {
        let window = 7;
        let mut ax = FakeAx::with_one_window(window, rect(0., 0., 100., 100.));
        ax.without_title_element.push(window);
        let (mut state, _sink) = state_with(ax);
        state.bundle_id = Some("com.googlecode.iterm2".to_owned());

        let (info, _, _) = state
            .register_window(window, peer(window, rect(0., 0., 100., 100.)))
            .expect("registers");

        assert!(!info.is_standard, "no readable title element means not standard");
    }

    /// A close makes the window key through the window server, then posts Command-W, so the
    /// application closes that window its own way.
    #[test]
    fn a_close_makes_the_window_key_then_posts_command_w() {
        let (mut state, _sink, _) = raisable(7);

        state
            .handle_request(Request::CloseWindow(Some(WindowServerId::new(7))))
            .expect("handled");

        assert_eq!(state.ax.made_key(), [(PID, WindowServerId::new(7))]);
        assert_eq!(state.ax.command_w_posts.get(), 1, "Command-W posted once");
    }

    /// Command-W closes whichever window is key, so a close whose window could not be made key
    /// posts none.
    #[test]
    fn a_close_whose_window_cannot_be_made_key_posts_no_command_w() {
        let (mut state, _sink, _) = raisable(7);
        state.ax.server.fail_make_key_window(Some(CGError::Failure));

        state
            .handle_request(Request::CloseWindow(Some(WindowServerId::new(7))))
            .expect("handled");

        assert_eq!(state.ax.made_key(), [(PID, WindowServerId::new(7))], "asked");
        assert_eq!(state.ax.command_w_posts.get(), 0, "and nothing posted");
    }

    #[test]
    fn a_close_of_a_window_with_no_server_id_posts_command_w_alone() {
        let (mut state, _sink, _) = raisable(7);

        state.handle_request(Request::CloseWindow(None)).expect("handled");

        assert!(state.ax.made_key().is_empty(), "nothing to make key");
        assert_eq!(state.ax.command_w_posts.get(), 1);
    }

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
                [Event::WindowCreated(_, _, Some(info), Some(MouseState::Down))]
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

    /// A window the window server gives a parent is a child window, not a root one.
    #[test]
    fn a_window_the_window_server_gives_a_parent_is_not_a_root_window() {
        let window = 7;
        let frame = rect(0., 0., 100., 100.);
        let ax = FakeAx::with_one_window(window, frame);
        ax.server
            .set_window_parent(WindowServerId::new(window), Some(WindowServerId::new(3)));
        let (mut state, _sink) = state_with(ax);

        let (info, _, _) = state.register_window(window, peer(window, frame)).expect("registers");

        assert!(!info.is_root);
    }

    /// A refresh takes on the windows the window server reports, and leaves out one it does not,
    /// which is not on screen.
    #[test]
    fn a_refresh_takes_on_only_the_windows_the_window_server_reports() {
        let (reported, unreported) = (7, 8);
        let frame = rect(0., 0., 100., 100.);
        let mut ax = FakeAx::with_one_window(reported, frame);
        ax.windows.push(unreported);
        ax.describe_standard_window(unreported, frame);
        ax.server.set_window(WindowServerId::new(reported), peer(reported, frame));
        let (mut state, sink) = state_with(ax);

        state.handle_request(Request::GetVisibleWindows).expect("refreshed");

        let reported_wid = state.id(&reported).expect("taken on");
        assert!(state.id(&unreported).is_err(), "left out");
        assert!(
            sink.0.borrow().iter().any(|event| matches!(
                event,
                Event::WindowsDiscovered { new, .. }
                    if new.iter().map(|(w, _)| *w).eq([reported_wid])
            )),
            "{:?}",
            sink.0.borrow()
        );
    }

    /// What the refresh `GetVisibleWindows` asks for reports: each window it found, with its info,
    /// and the ones of those not minimized.
    fn refreshed(
        state: &mut State<FakeAx>,
        sink: &Sink,
    ) -> (Vec<(WindowId, WindowInfo)>, Vec<WindowId>) {
        sink.0.borrow_mut().clear();
        state.handle_request(Request::GetVisibleWindows).expect("refreshed");
        sink.0
            .borrow()
            .iter()
            .find_map(|event| match event {
                Event::WindowsDiscovered { new, known_visible, .. } => {
                    Some((new.clone(), known_visible.clone()))
                }
                _ => None,
            })
            .expect("a refresh reports what it found")
    }

    /// A window a refresh takes on is reported once, not once as registered and again as found.
    #[test]
    fn a_refresh_reports_a_window_it_takes_on_once() {
        let window = 7;
        let frame = rect(0., 0., 100., 100.);
        let ax = FakeAx::with_one_window(window, frame);
        ax.server.set_window(WindowServerId::new(window), peer(window, frame));
        let (mut state, sink) = state_with(ax);

        let (new, known_visible) = refreshed(&mut state, &sink);

        let wid = state.id(&window).expect("taken on");
        assert_eq!(new.iter().map(|(w, _)| *w).collect::<Vec<_>>(), [wid]);
        assert_eq!(known_visible, [wid]);
    }

    /// What registration decides about a window holds at every refresh after: a window of an
    /// application that needs a title element and has none stays non-standard, and one the window
    /// server gives a parent stays off the root.
    #[test]
    fn a_refresh_reports_a_known_window_as_registration_decided() {
        let (untitled, child) = (7, 8);
        let frame = rect(0., 0., 100., 100.);
        let mut ax = FakeAx::with_one_window(untitled, frame);
        ax.windows.push(child);
        ax.describe_standard_window(child, frame);
        ax.without_title_element.push(untitled);
        for window in [untitled, child] {
            ax.server.set_window(WindowServerId::new(window), peer(window, frame));
        }
        ax.server
            .set_window_parent(WindowServerId::new(child), Some(WindowServerId::new(3)));
        let (mut state, sink) = state_with(ax);
        state.bundle_id = Some("com.googlecode.iterm2".to_owned());
        refreshed(&mut state, &sink);

        let (new, _) = refreshed(&mut state, &sink);

        let info = |window| {
            let wid = state.id(&window).expect("known");
            new.iter()
                .find(|(w, _)| *w == wid)
                .map(|(_, info)| info.clone())
                .expect("reported")
        };
        assert!(!info(untitled).is_standard, "no title element, not standard");
        assert!(!info(child).is_root, "a parent, not a root window");
        assert_eq!(new.len(), 2, "each reported once: {new:?}");
    }

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
        raises_tx.send(RaiseRequest(
            vec![raised],
            CancellationToken::new(),
            1,
            Quiet::No,
        ));
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

    /// A registered window of a frontmost application, with nothing announced yet.
    fn raisable(window: u32) -> (State<FakeAx>, Sink, WindowId) {
        let (mut state, sink) =
            state_with(FakeAx::with_one_window(window, rect(0., 0., 100., 100.)));
        let wid = state
            .register_window(window, peer(window, rect(0., 0., 100., 100.)))
            .expect("registers")
            .1;
        sink.0.borrow_mut().clear();
        (state, sink, wid)
    }

    fn raised(sink: &Sink) -> bool {
        sink.0
            .borrow()
            .iter()
            .any(|event| matches!(event, Event::RaiseCompleted { .. }))
    }

    fn completed(sink: &Sink) -> Vec<WindowId> {
        sink.0
            .borrow()
            .iter()
            .filter_map(|event| match event {
                Event::RaiseCompleted { window_id, .. } => Some(*window_id),
                _ => None,
            })
            .collect()
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
            1,
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

    #[test]
    fn another_applications_window_is_not_found_rather_than_a_panic() {
        let (mut state, _sink, wid) = raisable(7);
        let foreign = WindowId::new(PID + 1, wid.idx.get());
        assert!(matches!(state.window(foreign), Err(AxError::NotFound)));
        assert!(matches!(state.window_mut(foreign), Err(AxError::NotFound)));
        assert!(state.window(wid).is_ok());
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
                .borrow()
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
        assert!(sink.0.borrow().is_empty(), "and the reactor hears nothing");
    }

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
            sink.0.borrow().iter().any(|event| matches!(
                event,
                Event::WindowsDiscovered { new, .. } if new.iter().any(|(w, _)| *w == wid)
            )),
            "and the reactor hears of it"
        );
    }

    /// An application with `windows` registered, each stated on the window server, frontmost, with
    /// nothing announced yet.
    fn app_with(windows: &[u32]) -> (State<FakeAx>, Sink, Vec<WindowId>) {
        let frame = rect(0., 0., 100., 100.);
        let mut ax = FakeAx::with_one_window(windows[0], frame);
        for &window in &windows[1..] {
            ax.windows.push(window);
            ax.describe_standard_window(window, frame);
        }
        for &window in windows {
            ax.server.set_window(WindowServerId::new(window), peer(window, frame));
        }
        let (mut state, sink) = state_with(ax);
        let wids = windows
            .iter()
            .map(|&window| state.register_window(window, peer(window, frame)).expect("registers").1)
            .collect();
        sink.0.borrow_mut().clear();
        (state, sink, wids)
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

    fn announced(sink: &Sink) -> Vec<Event> { sink.0.borrow_mut().drain(..).collect() }

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
                [Event::WindowFrameChanged(w, frame, None, Requested(false), Some(MouseState::Down))]
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

    fn minimized_and_restored(events: &[Event]) -> (Vec<WindowId>, Vec<WindowId>) {
        let mut minimized: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::WindowMinimized(wid) => Some(*wid),
                _ => None,
            })
            .collect();
        let mut restored: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::WindowDeminiaturized(wid) => Some(*wid),
                _ => None,
            })
            .collect();
        minimized.sort_by_key(|wid| wid.idx);
        restored.sort_by_key(|wid| wid.idx);
        (minimized, restored)
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

    #[test]
    fn a_window_taken_on_while_the_application_is_hidden_is_announced_minimized() {
        let (mut state, sink, _) = app_with(&[7]);
        state.handle_notification(1, AxNotificationKind::ApplicationHidden, None);
        state.ax.describe_standard_window(8, rect(0., 0., 100., 100.));
        announced(&sink);

        let (_, wid, _) =
            state.register_window(8, peer(8, rect(0., 0., 100., 100.))).expect("registers");

        assert_eq!(minimized_and_restored(&announced(&sink)), (vec![wid], vec![]));
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

    fn poll_once<T>(future: std::pin::Pin<&mut impl Future<Output = T>>) -> std::task::Poll<T> {
        future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    }

    fn main_window_changes(sink: &Sink) -> Vec<(Option<WindowId>, Quiet)> {
        sink.0
            .borrow()
            .iter()
            .filter_map(|event| match event {
                Event::ApplicationMainWindowChanged(PID, wid, quiet) => Some((*wid, *quiet)),
                _ => None,
            })
            .collect()
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
            1,
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
            1,
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
            1,
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
            1,
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
            1,
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
            1,
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
            1,
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
            1,
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
            1,
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
                result = State::handle_raise_request(&raising, raised, &token, 1, Quiet::No) => {
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
            1,
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
            1,
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
            1,
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
            1,
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
            1,
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
