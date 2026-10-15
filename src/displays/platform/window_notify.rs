//! The actor behind the SkyLight notification port. Space events go to the spaces actor; a
//! window's moves, resizes, reorders, hides, unhides and closes, and each window the application
//! introduces, are read from the window server on one frames thread, in the order they came, and
//! handed to the application's [`OnServerRead`] callbacks, only while the application is
//! [`Following`] them; window-server focus edges are coalesced and resolved into
//! `WindowServerFocusChanged`.
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use rini_core::ids::{SpaceId, WindowServerId};
use rini_skylight_sys::KnownCGSEvent;
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, trace};

use crate::displays::domain::frames;
use crate::displays::event::{
    Event, EventSink, Following, OnServerRead, OrderChange, ServerFrame, ServerIntro, ServerOrder,
    ServerRead, StackLevel, ToRead,
};
use crate::displays::platform::{cgs_notify, spaces};
use crate::windows::platform::window_server::{self, WindowServer};

const FOCUS_WAKE_DEBOUNCE: Duration = Duration::from_millis(1);

#[derive(Clone)]
struct FocusWakeSender {
    wake: mpsc::SyncSender<()>,
}

impl FocusWakeSender {
    fn notify(&self) { let _ = self.wake.try_send(()); }
}

pub struct WindowNotify {
    spaces_tx: spaces::Sender,
    events: Vec<KnownCGSEvent>,
    focus_wake: FocusWakeSender,
    following: Following,
}

impl WindowNotify {
    pub fn new(
        displays_tx: impl EventSink + 'static,
        spaces_tx: spaces::Sender,
        events: &[KnownCGSEvent],
        on_read: Vec<OnServerRead>,
        following: Following,
        reads: mpsc::Receiver<ToRead>,
    ) -> Self {
        let window_server = window_server::live();
        let (focus_wake_tx, focus_wake_rx) = mpsc::sync_channel(1);
        Self::spawn_focus_resolver(window_server.clone(), displays_tx, focus_wake_rx);
        spawn_frame_reader(window_server, on_read, reads);
        Self {
            spaces_tx,
            events: events.to_vec(),
            focus_wake: FocusWakeSender { wake: focus_wake_tx },
            following,
        }
    }

    /// Subscribes to every event it was given, then never returns: `supervise` takes an actor that
    /// returns for one that died.
    pub async fn run(self) {
        for event in self.events {
            match Self::subscribe(
                event,
                self.spaces_tx.clone(),
                self.focus_wake.clone(),
                self.following.clone(),
            ) {
                Ok(()) => debug!("subscribed to event {}", event),
                Err(code) => debug!("subscribe for {} failed (res={})", event, code),
            }
        }
        std::future::pending::<()>().await
    }

    fn subscribe(
        event: KnownCGSEvent,
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
                    KnownCGSEvent::SpaceDestroyed => {
                        if let Some(space_id) = evt.space_id {
                            spaces_tx
                                .send(spaces::Notification::SpaceDestroyed(SpaceId::new(space_id)));
                        }
                    }
                    KnownCGSEvent::SpaceCreated => {
                        if let Some(space_id) = evt.space_id {
                            spaces_tx
                                .send(spaces::Notification::SpaceCreated(SpaceId::new(space_id)));
                        }
                    }
                    KnownCGSEvent::ManagedSpaceMembershipUpdated
                    | KnownCGSEvent::SpaceWindowManagementCapabilitiesChanged => {
                        spaces_tx.send(spaces::Notification::SpaceInventoryChanged);
                    }
                    KnownCGSEvent::SpaceWindowDestroyed => {
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
                    KnownCGSEvent::SpaceWindowCreated => {
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
                    KnownCGSEvent::WindowReordered
                    | KnownCGSEvent::WindowUnhidden
                    | KnownCGSEvent::WindowHidden => {
                        focus_wake.notify();
                        note(&following, event, evt.window_id);
                    }
                    KnownCGSEvent::WindowClosed => note(&following, event, evt.window_id),
                    KnownCGSEvent::WindowManagerSpaceFrontConnectionChanged
                    | KnownCGSEvent::WindowManagerGlobalFrontConnectionChanged => {
                        focus_wake.notify()
                    }
                    KnownCGSEvent::WindowMoved | KnownCGSEvent::WindowResized => {
                        note(&following, event, evt.window_id)
                    }
                }
            }
        });

        Ok(())
    }

    fn spawn_focus_resolver(
        window_server: Arc<dyn WindowServer>,
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
                        let window = window_server.key_focused_window(space);
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
fn note(following: &Following, event: KnownCGSEvent, window: Option<u32>) {
    let Some(window) = window.map(WindowServerId::new) else {
        return;
    };
    let read = match event {
        KnownCGSEvent::WindowMoved | KnownCGSEvent::WindowResized => ToRead::Frame(window),
        KnownCGSEvent::WindowReordered => ToRead::Order(window, OrderChange::Reordered),
        KnownCGSEvent::WindowHidden => ToRead::Order(window, OrderChange::Hidden),
        KnownCGSEvent::WindowUnhidden => ToRead::Order(window, OrderChange::Unhidden),
        KnownCGSEvent::WindowClosed => ToRead::Order(window, OrderChange::Closed),
        _ => return,
    };
    following.ask(read);
}

fn stack_level(read: &dyn WindowServer, window: WindowServerId) -> StackLevel {
    let (level, sub_level) = read.stack_level(window);
    StackLevel { level, sub_level }
}

/// What one burst of the queue reads, in the order it was asked: the frames first, each moved
/// window read once after its last notification (`frames::latest`), then each order and each
/// introduction as they came. A window's level is read once a burst, for its reorders and unhides
/// and its introduction alike: every read in a burst comes after every notification in it.
fn read_burst(read: &dyn WindowServer, burst: &[ToRead]) -> Vec<ServerRead> {
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
        .filter_map(|window| {
            Some(ServerFrame {
                window,
                frame: read.server_frame(window)?,
            })
        })
        .collect();
    let mut levels: HashMap<WindowServerId, StackLevel> = HashMap::default();
    let mut level = |window| *levels.entry(window).or_insert_with(|| stack_level(read, window));
    let mut out: Vec<ServerRead> = Vec::new();
    if !frames.is_empty() {
        out.push(ServerRead::Frames(frames));
    }
    for asked in burst {
        match *asked {
            ToRead::Frame(_) => {}
            ToRead::Order(window, change) => {
                let leveled = matches!(change, OrderChange::Reordered | OrderChange::Unhidden);
                out.push(ServerRead::Order(ServerOrder {
                    window,
                    change,
                    level: leveled.then(|| level(window)),
                }));
            }
            ToRead::Intro(window) => out.push(ServerRead::Intro(ServerIntro {
                window,
                frame: read.server_frame(window),
                ordered_in: read.window_ordered_in(window),
                radius: read.corner_radius(window),
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
    read: Arc<dyn WindowServer>,
    on_read: Vec<OnServerRead>,
    reads: mpsc::Receiver<ToRead>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("window-frames".to_string())
        .spawn(move || {
            while let Ok(first) = reads.recv() {
                let burst: Vec<ToRead> = std::iter::once(first).chain(reads.try_iter()).collect();
                for served in read_burst(&*read, &burst) {
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
    use std::sync::{Arc, Mutex, mpsc};

    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use rini_core::ids::WindowServerId;
    use rini_skylight_sys::KnownCGSEvent;

    use super::{FocusWakeSender, note, read_burst, spawn_frame_reader};
    use crate::displays::event::{
        Following, OnServerRead, OrderChange, ServerFrame, ServerIntro, ServerOrder, ServerRead,
        StackLevel, ToRead,
    };
    use crate::windows::domain::info::WindowServerInfo;
    use crate::windows::platform::window_server::FakeWindowServer;

    fn following() -> (Following, mpsc::Receiver<ToRead>) {
        let (following, reads) = Following::new();
        following.set(true);
        (following, reads)
    }

    fn ws(id: u32) -> WindowServerId { WindowServerId::new(id) }

    const AT: CGRect = CGRect::new(CGPoint::new(10., 20.), CGSize::new(300., 400.));
    const LEVEL: StackLevel = StackLevel {
        level: Some(3),
        sub_level: Some(1),
    };

    /// A window server with each of `windows` at `AT`, ordered out, rounded at 16 and at `LEVEL`.
    fn server(windows: &[u32]) -> Arc<FakeWindowServer> {
        let server = Arc::new(FakeWindowServer::default());
        for &id in windows {
            let info = WindowServerInfo {
                id: ws(id),
                pid: 1,
                layer: 3,
                frame: AT,
                min_frame: CGSize::ZERO,
                max_frame: CGSize::ZERO,
            };
            server
                .set_window(ws(id), Some(info))
                .set_window_sub_level(ws(id), Some(1))
                .set_server_frame(ws(id), Some(AT))
                .set_window_ordered_in(ws(id), Some(false))
                .set_corner_radii(ws(id), Some(vec![16.0; 4]));
        }
        server
    }

    /// The reader on its own thread over `queued`, run to the queue's end, and everything it
    /// handed on.
    fn run(read: Arc<FakeWindowServer>, queued: mpsc::Receiver<ToRead>) -> Vec<ServerRead> {
        let published = Arc::new(Mutex::new(Vec::new()));
        let sink = published.clone();
        let on_read: Vec<OnServerRead> =
            vec![Box::new(move |read| sink.lock().unwrap().push(read))];
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
            note(&following, event, Some(56));
        }
        note(&following, KnownCGSEvent::WindowReordered, None);
        note(&following, KnownCGSEvent::SpaceCreated, Some(56));
        drop(following);
        let window = ws(56);
        let order = |change, level| ServerRead::Order(ServerOrder { window, change, level });
        assert_eq!(run(server(&[56]), reads), vec![
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
        note(&off, KnownCGSEvent::WindowReordered, Some(56));
        note(&off, KnownCGSEvent::WindowMoved, Some(56));
        off.ask(ToRead::Intro(ws(56)));
        assert_eq!(reads.try_iter().count(), 0, "nothing queued to be read");
        let on = off.clone();
        on.set(true);
        assert!(off.on(), "one switch");
        note(&off, KnownCGSEvent::WindowReordered, Some(56));
        note(&off, KnownCGSEvent::WindowMoved, Some(56));
        on.ask(ToRead::Intro(ws(56)));
        assert_eq!(
            reads.try_iter().collect::<Vec<_>>(),
            vec![
                ToRead::Order(ws(56), OrderChange::Reordered),
                ToRead::Frame(ws(56)),
                ToRead::Intro(ws(56)),
            ],
            "one queue"
        );
    }

    #[test]
    fn a_move_and_a_resize_of_one_window_publish_one_frame() {
        let (following, reads) = following();
        note(&following, KnownCGSEvent::WindowMoved, Some(7));
        note(&following, KnownCGSEvent::WindowResized, Some(7));
        drop(following);
        let read = server(&[7]);
        let published = run(read.clone(), reads);
        assert_eq!(published, vec![ServerRead::Frames(vec![ServerFrame {
            window: ws(7),
            frame: AT
        }])]);
        assert_eq!(read.reads(), 1, "one read for the two");
    }

    /// A window new to the rest windows is read on the frames thread, never on the main thread that
    /// asked: its frame, whether it is ordered in, its corner and its place in the stack.
    #[test]
    fn an_introduced_window_is_read_whole_on_the_frames_thread() {
        let (following, reads) = following();
        following.ask(ToRead::Intro(ws(9)));
        drop(following);
        assert_eq!(run(server(&[9]), reads), vec![ServerRead::Intro(ServerIntro {
            window: ws(9),
            frame: Some(AT),
            ordered_in: Some(false),
            radius: Some(16.0),
            level: LEVEL,
        })]);
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
        let server = server(&[7, 8]);
        let read = read_burst(&*server, &burst);
        let levels: Vec<Option<StackLevel>> = read
            .iter()
            .map(|read| match read {
                ServerRead::Order(order) => order.level,
                ServerRead::Intro(intro) => Some(intro.level),
                ServerRead::Frames(_) => None,
            })
            .collect();
        assert_eq!(levels, vec![
            Some(LEVEL),
            Some(LEVEL),
            None,
            Some(LEVEL),
            Some(LEVEL)
        ]);
        assert_eq!(
            server.reads(),
            3 + 2 * 2,
            "the introduction's frame, order and corner, and each window's level and sub-level \
             once a burst"
        );
    }

    /// 816 and 815 arrive on a thread each; queued in the order they came, they are published in it,
    /// whatever else the burst holds.
    #[test]
    fn a_hide_then_an_unhide_publish_in_order() {
        let (following, reads) = following();
        note(&following, KnownCGSEvent::WindowHidden, Some(5));
        note(&following, KnownCGSEvent::WindowMoved, Some(5));
        note(&following, KnownCGSEvent::WindowUnhidden, Some(5));
        drop(following);
        let changes: Vec<OrderChange> = run(server(&[5]), reads)
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
