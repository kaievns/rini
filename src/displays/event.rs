//! What the displays context tells the application. The reactor converts these into its own
//! event type; this crate never imports the reactor.
use rini_core::ids::WindowServerId;

use rini_core::ids::SpaceId;
use rini_core::ids::WindowId;
use crate::displays::screen::CoordinateConverter;
use crate::displays::domain::topology::{ForwardedSpaceState, SpaceEventKind};

#[derive(Debug)]
pub enum Event {
    /// The authoritative screens/spaces snapshot. Sent after every accepted topology change.
    SpaceStateUpdated(ForwardedSpaceState, CoordinateConverter),
    ActiveDisplayChanged { menu_bar_space: Option<SpaceId>, command_space: Option<SpaceId> },
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
