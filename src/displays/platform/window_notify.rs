//! The actor behind the SkyLight notification port. Space events go to the spaces actor; a
//! window's moves, resizes, reorders, hides, unhides and closes, and each window the application
//! introduces, are read from the window server on one frames thread, in the order they came, and
//! handed to the application's [`OnServerRead`] callbacks, only while the application is
//! [`Following`] them; window-server focus edges are coalesced and resolved into
//! `WindowServerFocusChanged`.
use std::sync::mpsc;
use std::time::{Duration, Instant};

use objc2_core_foundation::CGRect;
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, trace};

use crate::windows::platform::window_server;
use rini_core::ids::WindowServerId;
use rini_skylight_sys::{CGSEventType, KnownCGSEvent};
use rustc_hash::FxHashSet as HashSet;

use crate::displays::domain::frames;
use crate::displays::event::{
    Event, EventSink, Following, OnServerRead, OrderChange, ServerFrame, ServerIntro, ServerOrder,
    ServerRead, StackLevel, ToRead,
};
use crate::displays::platform::cgs_notify;
use crate::displays::platform::spaces;
use rini_core::ids::SpaceId;

#[derive(Debug)]
pub enum Request {
    Subscribe(CGSEventType),
    UpdateWindowNotifications(Vec<u32>),
    Stop,
}

pub type Sender = rini_runloop::channel::Sender<Request>;
pub type Receiver = rini_runloop::channel::Receiver<Request>;

const FOCUS_WAKE_DEBOUNCE: Duration = Duration::from_millis(1);

#[derive(Clone)]
struct FocusWakeSender {
    wake: mpsc::SyncSender<()>,
}

impl FocusWakeSender {
    fn notify(&self) {
        let _ = self.wake.try_send(());
    }
}

pub struct WindowNotify {
    spaces_tx: spaces::Sender,
    requests_rx: Option<Receiver>,
    subscribed: HashSet<CGSEventType>,
    initial_events: Vec<CGSEventType>,
    focus_wake: FocusWakeSender,
    following: Following,
}

impl WindowNotify {
    pub fn new(
        displays_tx: impl EventSink + 'static,
        spaces_tx: spaces::Sender,
        requests_rx: Receiver,
        initial_events: &[CGSEventType],
        on_read: Vec<OnServerRead>,
        following: Following,
        reads: mpsc::Receiver<ToRead>,
    ) -> Self {
        let (focus_wake_tx, focus_wake_rx) = mpsc::sync_channel(1);
        Self::spawn_focus_resolver(displays_tx, focus_wake_rx);
        spawn_frame_reader(LIVE, on_read, reads);
        Self {
            spaces_tx,
            requests_rx: Some(requests_rx),
            subscribed: HashSet::default(),
            initial_events: initial_events.iter().copied().collect(),
            focus_wake: FocusWakeSender { wake: focus_wake_tx },
            following,
        }
    }

    pub async fn run(mut self) {
        let mut requests_rx = match self.requests_rx.take() {
            Some(rx) => rx,
            None => return,
        };
        for event in self.initial_events.drain(..) {
            match Self::subscribe(
                event,
                self.spaces_tx.clone(),
                self.focus_wake.clone(),
                self.following.clone(),
            ) {
                Ok(()) => {
                    self.subscribed.insert(event);
                    debug!("initial subscription succeeded for event {}", event);
                }
                Err(code) => {
                    debug!("initial subscribe for {} failed (res={})", event, code);
                }
            }
        }

        while let Some((span, request)) = requests_rx.recv().await {
            let _g = span.enter();
            if let Request::Stop = request {
                debug!("received Stop request");
                break;
            }
            self.handle_request(request);
        }

        debug!("WindowNotify actor exiting");
    }

    fn handle_request(&mut self, request: Request) {
        match request {
            Request::Subscribe(event) => {
                if self.subscribed.contains(&event) {
                    debug!("already subscribed to event {}", event);
                    return;
                }
                match Self::subscribe(
                    event,
                    self.spaces_tx.clone(),
                    self.focus_wake.clone(),
                    self.following.clone(),
                ) {
                    Ok(()) => {
                        self.subscribed.insert(event);
                        debug!("subscribed to event {}", event);
                    }
                    Err(code) => {
                        debug!("failed to register event {} (res={})", event, code);
                    }
                }
            }
            Request::UpdateWindowNotifications(window_ids) => {
                cgs_notify::update_window_notifications(&window_ids);
            }

            Request::Stop => {}
        }
    }

    fn subscribe(
        event: CGSEventType,
        spaces_tx: spaces::Sender,
        focus_wake: FocusWakeSender,
        following: Following,
    ) -> Result<(), i32> {
        let res = cgs_notify::init(event);
        if res != 0 {
            return Err(res);
        }

        let mut rx = cgs_notify::take_receiver(event);

        std::thread::spawn(move || {
            while let Some((_span, evt)) = rx.blocking_recv() {
                trace!(?event, ?evt, "got event");

                match event {
                    CGSEventType::Known(KnownCGSEvent::SpaceDestroyed) => {
                        if let Some(space_id) = evt.space_id {
                            spaces_tx
                                .send(spaces::Notification::SpaceDestroyed(SpaceId::new(space_id)));
                        }
                    }
                    CGSEventType::Known(KnownCGSEvent::SpaceCreated) => {
                        if let Some(space_id) = evt.space_id {
                            spaces_tx
                                .send(spaces::Notification::SpaceCreated(SpaceId::new(space_id)));
                        }
                    }
                    CGSEventType::Known(KnownCGSEvent::SpaceCurrentChanged) => {
                        spaces_tx.send(spaces::Notification::ActiveSpaceChanged);
                    }
                    CGSEventType::Known(KnownCGSEvent::ManagedSpaceMembershipUpdated)
                    | CGSEventType::Known(
                        KnownCGSEvent::SpaceWindowManagementCapabilitiesChanged,
                    ) => {
                        spaces_tx.send(spaces::Notification::SpaceInventoryChanged);
                    }
                    CGSEventType::Known(KnownCGSEvent::SpaceWindowDestroyed) => {
                        focus_wake.notify();
                        let (Some(window_id), Some(space_id)) = (evt.window_id, evt.space_id)
                        else {
                            continue;
                        };
                        // This is not just "window left the current active-space snapshot".
                        // CGS emits SpaceWindowDestroyed when the window's connection drops
                        // out of the WindowServer membership for that space.
                        spaces_tx.send(spaces::Notification::WindowServerDestroyed(
                            WindowServerId::new(window_id),
                            SpaceId::new(space_id),
                        ))
                    }
                    CGSEventType::Known(KnownCGSEvent::SpaceWindowCreated) => {
                        focus_wake.notify();
                        let (Some(window_id), Some(space_id)) = (evt.window_id, evt.space_id)
                        else {
                            continue;
                        };
                        spaces_tx.send(spaces::Notification::WindowServerAppeared(
                            WindowServerId::new(window_id),
                            SpaceId::new(space_id),
                        ));
                    }
                    CGSEventType::Known(KnownCGSEvent::WindowReordered)
                    | CGSEventType::Known(KnownCGSEvent::WindowUnhidden)
                    | CGSEventType::Known(KnownCGSEvent::WindowHidden) => {
                        focus_wake.notify();
                        note(&following, event, evt.window_id);
                    }
                    CGSEventType::Known(KnownCGSEvent::WindowClosed) => {
                        note(&following, event, evt.window_id)
                    }
                    CGSEventType::Known(
                        KnownCGSEvent::WindowManagerSpaceFrontConnectionChanged,
                    )
                    | CGSEventType::Known(
                        KnownCGSEvent::WindowManagerGlobalFrontConnectionChanged,
                    ) => focus_wake.notify(),
                    CGSEventType::Known(KnownCGSEvent::WindowMoved)
                    | CGSEventType::Known(KnownCGSEvent::WindowResized) => {
                        note(&following, event, evt.window_id)
                    }
                    _ => {}
                }
            }
        });

        Ok(())
    }

    fn spawn_focus_resolver(
        displays_tx: impl EventSink + 'static,
        focus_wake_rx: mpsc::Receiver<()>,
    ) {
        std::thread::Builder::new()
            .name("window-focus-resolver".to_string())
            .spawn(move || {
                while focus_wake_rx.recv().is_ok() {
                    let mut wake_count = 1;
                    loop {
                        // WindowServer commonly emits 808 and 815 back-to-back.
                        // The bounded channel absorbs both into this one wake.
                        std::thread::sleep(FOCUS_WAKE_DEBOUNCE);
                        while focus_wake_rx.try_recv().is_ok() {
                            wake_count += 1;
                        }

                        let query_started = Instant::now();
                        let space = crate::displays::platform::space_query::active_space();
                        let window = window_server::key_focused_window(space);
                        let query_elapsed = query_started.elapsed();

                        // A wake queued during the SPI means this result may already
                        // be stale. Resolve once more before publishing it.
                        if focus_wake_rx.try_recv().is_ok() {
                            wake_count += 1;
                            continue;
                        }

                        trace!(
                            wake_count,
                            ?query_elapsed,
                            ?space,
                            ?window,
                            "resolved coalesced WindowServer focus"
                        );
                        if let Some(window) = window {
                            displays_tx.send(Event::WindowServerFocusChanged(window, space));
                        }
                        break;
                    }
                }
            })
            .expect("failed to spawn WindowServer focus resolver");
    }
}

/// Queues for the frames thread what the window server's `event` asks of `window`, while
/// `following`: its frame after a move or a resize, its order otherwise.
fn note(following: &Following, event: CGSEventType, window: Option<u32>) {
    let Some(window) = window.map(WindowServerId::new) else { return };
    let read = match event {
        CGSEventType::Known(KnownCGSEvent::WindowMoved | KnownCGSEvent::WindowResized) => ToRead::Frame(window),
        CGSEventType::Known(KnownCGSEvent::WindowReordered) => ToRead::Order(window, OrderChange::Reordered),
        CGSEventType::Known(KnownCGSEvent::WindowHidden) => ToRead::Order(window, OrderChange::Hidden),
        CGSEventType::Known(KnownCGSEvent::WindowUnhidden) => ToRead::Order(window, OrderChange::Unhidden),
        CGSEventType::Known(KnownCGSEvent::WindowClosed) => ToRead::Order(window, OrderChange::Closed),
        _ => return,
    };
    following.ask(read);
}

/// What the frames thread asks the window server, one call each.
#[derive(Clone, Copy)]
struct Reader {
    frame: fn(WindowServerId) -> Option<CGRect>,
    level: fn(WindowServerId) -> StackLevel,
    ordered_in: fn(WindowServerId) -> Option<bool>,
    radius: fn(WindowServerId) -> Option<f64>,
}

const LIVE: Reader = Reader {
    frame: window_server::server_frame,
    level: stack_level,
    ordered_in: window_server::window_ordered_in,
    radius: window_server::corner_radius,
};

fn stack_level(window: WindowServerId) -> StackLevel {
    let (level, sub_level) = window_server::stack_level(window);
    StackLevel { level, sub_level }
}

/// What one burst of the queue reads, in the order it was asked: the frames first, each moved
/// window read once after its last notification (`frames::latest`), then each order and each
/// introduction as they came. A window's level is read once a burst, for its reorders and unhides
/// and its introduction alike: every read in a burst comes after every notification in it.
fn read_burst(read: &Reader, burst: &[ToRead]) -> Vec<ServerRead> {
    let moved: Vec<u32> = burst
        .iter()
        .filter_map(|asked| match asked {
            ToRead::Frame(window) => Some(window.as_u32()),
            _ => None,
        })
        .collect();
    let frames: Vec<ServerFrame> = frames::latest(&moved)
        .into_iter()
        .map(WindowServerId::new)
        .filter_map(|window| Some(ServerFrame { window, frame: (read.frame)(window)? }))
        .collect();
    let mut levels: HashMap<WindowServerId, StackLevel> = HashMap::default();
    let mut level = |window| *levels.entry(window).or_insert_with(|| (read.level)(window));
    let mut out: Vec<ServerRead> = Vec::new();
    if !frames.is_empty() {
        out.push(ServerRead::Frames(frames));
    }
    for asked in burst {
        match *asked {
            ToRead::Frame(_) => {}
            ToRead::Order(window, change) => {
                let leveled = matches!(change, OrderChange::Reordered | OrderChange::Unhidden);
                out.push(ServerRead::Order(ServerOrder { window, change, level: leveled.then(|| level(window)) }));
            }
            ToRead::Intro(window) => out.push(ServerRead::Intro(ServerIntro {
                window,
                frame: (read.frame)(window),
                ordered_in: (read.ordered_in)(window),
                radius: (read.radius)(window),
                level: level(window),
            })),
        }
    }
    out
}

/// The one thread that reads the window server for the application, so nothing read earlier is
/// published after something newer: 806, 807, 808, 815, 816 and 804 arrive on a thread each,
/// unordered against each other, and an introduction comes from the application's own thread. Each
/// wake drains what has queued and reads it as one burst (`read_burst`).
fn spawn_frame_reader(
    read: Reader,
    on_read: Vec<OnServerRead>,
    reads: mpsc::Receiver<ToRead>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("window-frames".to_string())
        .spawn(move || {
            while let Ok(first) = reads.recv() {
                let burst: Vec<ToRead> = std::iter::once(first).chain(reads.try_iter()).collect();
                for served in read_burst(&read, &burst) {
                    for on_read in &on_read {
                        on_read(served.clone());
                    }
                }
            }
        })
        .expect("failed to spawn the window frames thread")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};

    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use rini_core::ids::WindowServerId;

    use rini_skylight_sys::{CGSEventType, KnownCGSEvent};

    use super::{FocusWakeSender, Reader, note, read_burst, spawn_frame_reader};
    use crate::displays::event::{
        Following, OnServerRead, OrderChange, ServerFrame, ServerIntro, ServerOrder, ServerRead, StackLevel,
        ToRead,
    };

    fn following() -> (Following, mpsc::Receiver<ToRead>) {
        let (following, reads) = Following::new();
        following.set(true);
        (following, reads)
    }

    fn ws(id: u32) -> WindowServerId { WindowServerId::new(id) }

    const AT: CGRect = CGRect::new(CGPoint::new(10., 20.), CGSize::new(300., 400.));
    const LEVEL: StackLevel = StackLevel { level: Some(3), sub_level: 1 };

    fn bounds(_: WindowServerId) -> Option<CGRect> { Some(AT) }
    fn level(_: WindowServerId) -> StackLevel { LEVEL }
    fn ordered_in(_: WindowServerId) -> Option<bool> { Some(false) }
    fn radius(_: WindowServerId) -> Option<f64> { Some(16.0) }

    const STUB: Reader = Reader { frame: bounds, level, ordered_in, radius };

    /// The reader on its own thread over `queued`, run to the queue's end, and everything it
    /// handed on.
    fn run(read: Reader, queued: mpsc::Receiver<ToRead>) -> Vec<ServerRead> {
        let published = Arc::new(Mutex::new(Vec::new()));
        let sink = published.clone();
        let on_read: Vec<OnServerRead> = vec![Box::new(move |read| sink.lock().unwrap().push(read))];
        spawn_frame_reader(read, on_read, queued).join().unwrap();
        Arc::try_unwrap(published).unwrap().into_inner().unwrap()
    }

    #[test]
    fn a_reorder_a_hide_an_unhide_and_a_close_publish_their_window() {
        let (following, reads) = following();
        for event in [
            KnownCGSEvent::WindowReordered,
            KnownCGSEvent::WindowHidden,
            KnownCGSEvent::WindowUnhidden,
            KnownCGSEvent::WindowClosed,
        ] {
            note(&following, CGSEventType::Known(event), Some(56));
        }
        note(&following, CGSEventType::Known(KnownCGSEvent::WindowReordered), None);
        note(&following, CGSEventType::Known(KnownCGSEvent::SpaceCreated), Some(56));
        drop(following);
        let window = ws(56);
        let order = |change, level| ServerRead::Order(ServerOrder { window, change, level });
        assert_eq!(run(STUB, reads), vec![
            order(OrderChange::Reordered, Some(LEVEL)),
            order(OrderChange::Hidden, None),
            order(OrderChange::Unhidden, Some(LEVEL)),
            order(OrderChange::Closed, None),
        ]);
    }

    /// Nothing follows the window server's frames and orders while no window wears a band: a move, an
    /// order and an introduction queue nothing to be read, until the switch is on again.
    #[test]
    fn nothing_is_read_or_handed_on_while_nothing_follows() {
        let (off, reads) = Following::new();
        assert!(!off.on(), "off until turned on");
        note(&off, CGSEventType::Known(KnownCGSEvent::WindowReordered), Some(56));
        note(&off, CGSEventType::Known(KnownCGSEvent::WindowMoved), Some(56));
        off.ask(ToRead::Intro(ws(56)));
        assert_eq!(reads.try_iter().count(), 0, "nothing queued to be read");
        let on = off.clone();
        on.set(true);
        assert!(off.on(), "one switch");
        note(&off, CGSEventType::Known(KnownCGSEvent::WindowReordered), Some(56));
        note(&off, CGSEventType::Known(KnownCGSEvent::WindowMoved), Some(56));
        on.ask(ToRead::Intro(ws(56)));
        assert_eq!(reads.try_iter().collect::<Vec<_>>(), vec![
            ToRead::Order(ws(56), OrderChange::Reordered),
            ToRead::Frame(ws(56)),
            ToRead::Intro(ws(56)),
        ], "one queue");
    }

    static FRAME_READS: AtomicUsize = AtomicUsize::new(0);

    fn counted(window: WindowServerId) -> Option<CGRect> {
        FRAME_READS.fetch_add(1, Ordering::SeqCst);
        bounds(window)
    }

    #[test]
    fn a_move_and_a_resize_of_one_window_publish_one_frame() {
        let (following, reads) = following();
        note(&following, CGSEventType::Known(KnownCGSEvent::WindowMoved), Some(7));
        note(&following, CGSEventType::Known(KnownCGSEvent::WindowResized), Some(7));
        drop(following);
        let published = run(Reader { frame: counted, ..STUB }, reads);
        assert_eq!(published, vec![ServerRead::Frames(vec![ServerFrame { window: ws(7), frame: AT }])]);
        assert_eq!(FRAME_READS.load(Ordering::SeqCst), 1, "one read for the two");
    }

    /// A window new to the rest windows is read on the frames thread, never on the main thread that
    /// asked: its frame, whether it is ordered in, its corner and its place in the stack.
    #[test]
    fn an_introduced_window_is_read_whole_on_the_frames_thread() {
        let (following, reads) = following();
        following.ask(ToRead::Intro(ws(9)));
        drop(following);
        assert_eq!(run(STUB, reads), vec![ServerRead::Intro(ServerIntro {
            window: ws(9),
            frame: Some(AT),
            ordered_in: Some(false),
            radius: Some(16.0),
            level: LEVEL,
        })]);
    }

    static LEVEL_READS: AtomicUsize = AtomicUsize::new(0);

    fn counted_level(window: WindowServerId) -> StackLevel {
        LEVEL_READS.fetch_add(1, Ordering::SeqCst);
        level(window)
    }

    /// A raise is several 808s of one window: its level and sub-level are read once for them, and
    /// once for the window introduced beside them.
    #[test]
    fn a_reorder_of_a_followed_window_carries_its_stack_level() {
        let order = |id, change| ToRead::Order(ws(id), change);
        let burst = [
            order(7, OrderChange::Reordered),
            order(7, OrderChange::Reordered),
            order(7, OrderChange::Hidden),
            ToRead::Intro(ws(7)),
            order(8, OrderChange::Unhidden),
        ];
        let read = read_burst(&Reader { level: counted_level, ..STUB }, &burst);
        let levels: Vec<Option<StackLevel>> = read
            .iter()
            .map(|read| match read {
                ServerRead::Order(order) => order.level,
                ServerRead::Intro(intro) => Some(intro.level),
                ServerRead::Frames(_) => None,
            })
            .collect();
        assert_eq!(levels, vec![Some(LEVEL), Some(LEVEL), None, Some(LEVEL), Some(LEVEL)]);
        assert_eq!(LEVEL_READS.load(Ordering::SeqCst), 2, "once a window a burst");
    }

    /// 816 and 815 arrive on a thread each; queued in the order they came, they are published in it,
    /// whatever else the burst holds.
    #[test]
    fn a_hide_then_an_unhide_publish_in_order() {
        let (following, reads) = following();
        note(&following, CGSEventType::Known(KnownCGSEvent::WindowHidden), Some(5));
        note(&following, CGSEventType::Known(KnownCGSEvent::WindowMoved), Some(5));
        note(&following, CGSEventType::Known(KnownCGSEvent::WindowUnhidden), Some(5));
        drop(following);
        let changes: Vec<OrderChange> = run(STUB, reads)
            .into_iter()
            .filter_map(|read| match read {
                ServerRead::Order(order) => Some(order.change),
                _ => None,
            })
            .collect();
        assert_eq!(changes, vec![OrderChange::Hidden, OrderChange::Unhidden]);
    }

    #[test]
    fn focus_wakes_coalesce_to_one_signal() {
        let (wake, rx) = std::sync::mpsc::sync_channel(1);
        let sender = FocusWakeSender { wake };

        sender.notify();
        sender.notify();
        sender.notify();

        assert_eq!(rx.try_iter().count(), 1);
    }
}
