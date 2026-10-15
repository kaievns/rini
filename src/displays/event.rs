//! What the displays context tells the application. The reactor converts these into its own
//! event type; this crate never imports the reactor.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use objc2_core_foundation::CGRect;
use rini_core::ids::{SpaceId, WindowId, WindowServerId};
use rini_geometry::CGRectDef;
use serde::{Deserialize, Serialize};

use crate::displays::domain::screen::CoordinateConverter;
use crate::displays::domain::topology::{ForwardedSpaceState, SpaceEventKind};

#[derive(Debug)]
pub enum Event {
    /// The authoritative screens/spaces snapshot. Sent after every accepted topology change.
    SpaceStateUpdated(Box<ForwardedSpaceState>, CoordinateConverter),
    ActiveDisplayChanged {
        menu_bar_space: Option<SpaceId>,
        command_space: Option<SpaceId>,
    },
    SpaceCreated(SpaceId),
    SpaceDestroyed(SpaceId),
    WindowServerAppeared(WindowServerId, SpaceId, SpaceEventKind),
    WindowServerDestroyed(WindowServerId, SpaceId, SpaceEventKind),
    SystemWillSleep,
    SystemWoke,
    SessionDidResignActive,
    SessionDidBecomeActive,
    DisplayChurnBegin,
    /// The Dock reported Mission Control (or App Exposé / Show Desktop) opening.
    MissionControlEntered,
    MissionControlExited,
    /// The window WindowServer reports as key on the active space, after the burst of reorder
    /// notifications one focus change produces has been coalesced.
    WindowServerFocusChanged(WindowId, SpaceId),
}

/// Where the window server has a window now, as its own move and resize notifications report it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ServerFrame {
    pub window: WindowServerId,
    #[serde(with = "CGRectDef")]
    pub frame: CGRect,
}

/// What the window server did to a window's place in its stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderChange {
    /// 808: it was ordered against the others.
    Reordered,
    /// 816: it was ordered out, minimised or its app hidden.
    Hidden,
    /// 815: it was ordered back in.
    Unhidden,
    /// 804: it was closed.
    Closed,
}

/// A window's level and sub-level as the window server has them: what a window of rini's own takes
/// to be ordered directly above it. Each is none when the window server answered none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackLevel {
    pub level: Option<isize>,
    pub sub_level: Option<i32>,
}

/// A window the window server reordered, hid, unhid or closed, as its own notifications name it,
/// with its level and sub-level read after a reorder or an unhide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerOrder {
    pub window: WindowServerId,
    pub change: OrderChange,
    pub level: Option<StackLevel>,
}

/// A window the application introduced to the frames thread, read whole: its frame, whether it is
/// ordered in, its corner radius, and its level and sub-level. Each is none where the window server
/// answered none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ServerIntro {
    pub window: WindowServerId,
    pub frame: Option<CGRect>,
    pub ordered_in: Option<bool>,
    pub radius: Option<f64>,
    pub level: StackLevel,
}

/// What the frames thread asks the window server, queued in the order it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToRead {
    /// 806 or 807: where it is now.
    Frame(WindowServerId),
    /// 808, 815, 816 or 804, read for its level after a reorder or an unhide.
    Order(WindowServerId, OrderChange),
    /// A window new to the application: all of it (`ServerIntro`).
    Intro(WindowServerId),
}

/// What the frames thread read, handed on in the order it was asked for: every tracked window that
/// moved or resized since the last batch, once, at the frame read after its last notification; each
/// order; each introduction.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerRead {
    Frames(Vec<ServerFrame>),
    Order(ServerOrder),
    Intro(ServerIntro),
}

/// Where the frames thread hands each [`ServerRead`], on that thread.
pub type OnServerRead = Box<dyn Fn(ServerRead) + Send>;

/// Whether the application follows the window server's frames and orders now, and the frames
/// thread's queue. While it does not, nothing is queued, so no window is read and no
/// [`ServerRead`] is handed on: a notification is dropped where it arrives. Off until the
/// application turns it on; every clone is the one switch and the one queue.
#[derive(Clone, Debug)]
pub struct Following {
    on: Arc<AtomicBool>,
    reads: mpsc::Sender<ToRead>,
}

impl Following {
    /// The switch, off, and the queue's end the frames thread reads.
    pub fn new() -> (Following, mpsc::Receiver<ToRead>) {
        let (reads, queue) = mpsc::channel();
        (Following { on: Arc::default(), reads }, queue)
    }

    pub fn set(&self, on: bool) { self.on.store(on, Ordering::Relaxed) }

    pub fn on(&self) -> bool { self.on.load(Ordering::Relaxed) }

    /// Queues `read` for the frames thread, while on.
    pub fn ask(&self, read: ToRead) {
        if self.on() {
            let _ = self.reads.send(read);
        }
    }
}

/// A switch whose queue nobody reads, for an application wired without the frames thread.
impl Default for Following {
    fn default() -> Following { Following::new().0 }
}

/// Where the spaces actor delivers its events. Implemented for any channel whose message type
/// can be built from [`Event`].
pub trait EventSink: Send {
    fn send(&self, event: Event);
}

impl<T: From<Event> + Send> EventSink for rini_runloop::channel::Sender<T> {
    fn send(&self, event: Event) { rini_runloop::channel::Sender::send(self, event.into()) }
}
