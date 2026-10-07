//! What the displays context tells the application. The reactor converts these into its own
//! event type; this crate never imports the reactor.
use objc2_core_foundation::CGRect;
use rini_core::ids::WindowServerId;
use rini_geometry::CGRectDef;
use serde::{Deserialize, Serialize};

use crate::displays::domain::screen::CoordinateConverter;
use crate::displays::domain::topology::{ForwardedSpaceState, SpaceEventKind};
use rini_core::ids::SpaceId;
use rini_core::ids::WindowId;

#[derive(Debug)]
pub enum Event {
    /// The authoritative screens/spaces snapshot. Sent after every accepted topology change.
    SpaceStateUpdated(ForwardedSpaceState, CoordinateConverter),
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

/// Where the frames thread hands every batch of [`ServerFrame`]s: each tracked window that moved or
/// resized since the last batch, once, at the frame read after its last notification.
pub type OnServerFrames = Box<dyn Fn(&[ServerFrame]) + Send>;

/// What the window server did to a window's place in its stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderChange {
    /// 808: it was ordered against the others.
    Reordered,
    /// 815: it was ordered out, minimised or its app hidden.
    Hidden,
    /// 816: it was ordered back in.
    Unhidden,
}

/// A window the window server reordered, hid or unhid, as its own notifications name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerOrder {
    pub window: WindowServerId,
    pub change: OrderChange,
}

/// Where each [`ServerOrder`] is handed, on the thread its notification arrives on.
pub type OnServerOrder = Box<dyn Fn(ServerOrder) + Send + Sync>;

/// Where the spaces actor delivers its events. Implemented for any channel whose message type
/// can be built from [`Event`].
pub trait EventSink: Send {
    fn send(&self, event: Event);
}

impl<T: From<Event> + Send> EventSink for rini_runloop::channel::Sender<T> {
    fn send(&self, event: Event) {
        rini_runloop::channel::Sender::send(self, event.into())
    }
}
