//! What the displays context tells the application. The reactor converts these into its own
//! event type; this crate never imports the reactor.
use rini_skylight_sys::WindowServerId;

use crate::ids::SpaceId;
use crate::screen::CoordinateConverter;
use crate::topology::{ForwardedSpaceState, SpaceEventKind};

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
