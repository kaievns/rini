//! The actor over a `FakeAx`, and what it told the reactor, for the tests of every job.

use std::sync::Arc;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use parking_lot::Mutex;
use rini_core::ids::{WindowId, WindowServerId, pid_t};
use rini_runloop::channel as channels;
use rustc_hash::FxHashMap as HashMap;

use super::State;
use crate::windows::domain::info::WindowServerInfo;
use crate::windows::domain::request::Quiet;
use crate::windows::event::{Event, EventSink};
use crate::windows::platform::ax::world::FakeAx;

/// Collects what the actor told the reactor.
#[derive(Clone, Default)]
pub(super) struct Sink(pub(super) Arc<Mutex<Vec<Event>>>);

impl EventSink for Sink {
    fn send(&self, event: Event) { self.0.lock().push(event); }
}

pub(super) const PID: pid_t = 501;

pub(super) fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

/// The window server's own record of a window, which the real path reads before registering.
/// Without one `admissible::has_visible_peer` refuses a window that HAS a server id, because an
/// id the server does not report back means the window is not on screen.
pub(super) fn peer(window: u32, frame: CGRect) -> Option<WindowServerInfo> {
    Some(WindowServerInfo {
        id: WindowServerId::new(window),
        pid: PID,
        layer: 0,
        frame,
        min_frame: CGSize::ZERO,
        max_frame: CGSize::ZERO,
    })
}

pub(super) fn state_with(ax: FakeAx) -> (State<FakeAx>, Sink) {
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

/// A registered window of a frontmost application, with nothing announced yet.
pub(super) fn raisable(window: u32) -> (State<FakeAx>, Sink, WindowId) {
    let (mut state, sink) = state_with(FakeAx::with_one_window(window, rect(0., 0., 100., 100.)));
    let wid = state
        .register_window(window, peer(window, rect(0., 0., 100., 100.)))
        .expect("registers")
        .1;
    sink.0.lock().clear();
    (state, sink, wid)
}

/// An application with `windows` registered, each stated on the window server, frontmost, with
/// nothing announced yet.
pub(super) fn app_with(windows: &[u32]) -> (State<FakeAx>, Sink, Vec<WindowId>) {
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
    sink.0.lock().clear();
    (state, sink, wids)
}

pub(super) fn announced(sink: &Sink) -> Vec<Event> { sink.0.lock().drain(..).collect() }

pub(super) fn poll_once<T>(
    future: std::pin::Pin<&mut impl Future<Output = T>>,
) -> std::task::Poll<T> {
    future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
}

/// The windows reported raised, in the order they were.
pub(super) fn completed(sink: &Sink) -> Vec<WindowId> {
    sink.0
        .lock()
        .iter()
        .filter_map(|event| match event {
            Event::RaiseCompleted { window_id, .. } => Some(*window_id),
            _ => None,
        })
        .collect()
}

pub(super) fn main_window_changes(sink: &Sink) -> Vec<(Option<WindowId>, Quiet)> {
    sink.0
        .lock()
        .iter()
        .filter_map(|event| match event {
            Event::ApplicationMainWindowChanged(PID, wid, quiet) => Some((*wid, *quiet)),
            _ => None,
        })
        .collect()
}

/// The windows announced minimized and the ones announced restored, each in id order.
pub(super) fn minimized_and_restored(events: &[Event]) -> (Vec<WindowId>, Vec<WindowId>) {
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
