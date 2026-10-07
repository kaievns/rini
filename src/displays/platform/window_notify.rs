//! The actor behind the SkyLight notification port. Space events go to the spaces actor; a
//! window's moves and resizes are read back from the window server on one frames thread and handed
//! to the application's [`OnServerFrames`] callbacks; window-server focus edges are coalesced and
//! resolved into `WindowServerFocusChanged`.
use std::sync::mpsc;
use std::time::{Duration, Instant};

use objc2_core_foundation::CGRect;
use tracing::{debug, trace};

use crate::windows::platform::window_server;
use rini_core::ids::WindowServerId;
use rini_skylight_sys::{CGSEventType, KnownCGSEvent};
use rustc_hash::FxHashSet as HashSet;

use crate::displays::domain::frames;
use crate::displays::event::{Event, EventSink, OnServerFrames, ServerFrame};
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
    moved: mpsc::Sender<u32>,
}

impl WindowNotify {
    pub fn new(
        displays_tx: impl EventSink + 'static,
        spaces_tx: spaces::Sender,
        requests_rx: Receiver,
        initial_events: &[CGSEventType],
        on_frames: Vec<OnServerFrames>,
    ) -> Self {
        let (focus_wake_tx, focus_wake_rx) = mpsc::sync_channel(1);
        Self::spawn_focus_resolver(displays_tx, focus_wake_rx);
        let (moved, moved_rx) = mpsc::channel();
        spawn_frame_reader(window_server::server_frame, on_frames, moved_rx);
        Self {
            spaces_tx,
            requests_rx: Some(requests_rx),
            subscribed: HashSet::default(),
            initial_events: initial_events.iter().copied().collect(),
            focus_wake: FocusWakeSender { wake: focus_wake_tx },
            moved,
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
                self.moved.clone(),
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
                    self.moved.clone(),
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
        moved: mpsc::Sender<u32>,
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
                    | CGSEventType::Known(KnownCGSEvent::WindowHidden)
                    | CGSEventType::Known(
                        KnownCGSEvent::WindowManagerSpaceFrontConnectionChanged,
                    )
                    | CGSEventType::Known(
                        KnownCGSEvent::WindowManagerGlobalFrontConnectionChanged,
                    ) => focus_wake.notify(),
                    CGSEventType::Known(KnownCGSEvent::WindowMoved)
                    | CGSEventType::Known(KnownCGSEvent::WindowResized) => {
                        if let Some(window) = evt.window_id {
                            let _ = moved.send(window);
                        }
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

/// The one thread that reads moved windows' frames, so a frame read earlier is never published after
/// a newer one: 806 and 807 arrive on a thread each, unordered against each other. Each wake drains
/// what has queued and reads every window in it once, after its last notification.
fn spawn_frame_reader(
    read: fn(WindowServerId) -> Option<CGRect>,
    on_frames: Vec<OnServerFrames>,
    moved: mpsc::Receiver<u32>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("window-frames".to_string())
        .spawn(move || {
            while let Ok(first) = moved.recv() {
                let burst: Vec<u32> = std::iter::once(first).chain(moved.try_iter()).collect();
                let batch: Vec<ServerFrame> = frames::latest(&burst)
                    .into_iter()
                    .map(WindowServerId::new)
                    .filter_map(|window| Some(ServerFrame { window, frame: read(window)? }))
                    .collect();
                if batch.is_empty() {
                    continue;
                }
                for on_frames in &on_frames {
                    on_frames(&batch);
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

    use super::{FocusWakeSender, spawn_frame_reader};
    use crate::displays::event::ServerFrame;

    static READS: AtomicUsize = AtomicUsize::new(0);

    fn bounds(_: WindowServerId) -> Option<CGRect> {
        READS.fetch_add(1, Ordering::SeqCst);
        Some(CGRect::new(CGPoint::new(10., 20.), CGSize::new(300., 400.)))
    }

    #[test]
    fn a_move_and_a_resize_of_one_window_publish_one_frame() {
        let (moved, moved_rx) = mpsc::channel();
        moved.send(7).unwrap();
        moved.send(7).unwrap();
        drop(moved);
        let published = Arc::new(Mutex::new(Vec::new()));
        let sink = published.clone();
        spawn_frame_reader(
            bounds,
            vec![Box::new(move |frames: &[ServerFrame]| {
                sink.lock().unwrap().push(frames.to_vec())
            })],
            moved_rx,
        )
        .join()
        .unwrap();

        let frame = bounds(WindowServerId::new(7)).unwrap();
        assert_eq!(
            *published.lock().unwrap(),
            vec![vec![ServerFrame {
                window: WindowServerId::new(7),
                frame
            }]]
        );
        assert_eq!(
            READS.load(Ordering::SeqCst),
            2,
            "one read by the thread, one by the test"
        );
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
