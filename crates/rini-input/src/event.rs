//! What the input context tells the application: a binding fired, or the pointer did something the
//! window manager reacts to. The application converts; this crate never imports the reactor.
use rini_windows::ids::WindowServerId;

use crate::binding::WmCommand;

#[derive(Debug)]
pub enum Event {
    /// A key binding or gesture fired.
    Command(WmCommand),
    /// A mouse button was released while the tap was processing mouse events.
    MouseUp,
    /// The pointer moved into a different window than the one it was in.
    PointerEnteredWindow(WindowServerId),
}

/// Where the taps deliver their events. Implemented for any channel whose message type can be
/// built from [`Event`].
pub trait EventSink: Send {
    fn send(&self, event: Event);
}

impl<T: From<Event> + Send> EventSink for rini_runloop::channel::Sender<T> {
    fn send(&self, event: Event) {
        rini_runloop::channel::Sender::send(self, event.into())
    }
}
