//! What the input context tells the application: a binding fired, or the pointer did something the
//! window manager reacts to. The application converts; this crate never imports the reactor.
use std::time::Instant;

use objc2_core_foundation::CGPoint;
use rini_core::ids::WindowServerId;

use crate::input::domain::binding::WmCommand;

#[derive(Debug)]
pub enum Event {
    /// A key binding or gesture fired.
    Command(WmCommand),
    /// A switcher session opened, moved, committed or was cancelled.
    ///
    /// On the same channel as `Command`, so an Open, its Steps and its Commit cannot be reordered
    /// relative to each other however long the far side takes to read them.
    Switch(crate::input::domain::switch_session::Signal),
    /// A mouse button was released while the tap was processing mouse events.
    MouseUp,
    /// The pointer moved into a different window than the one it was in.
    PointerEnteredWindow(WindowServerId),
}

/// The left button going down, handed to [`OnLeftPress`] rather than to the tap's sink.
#[derive(Debug, Clone, Copy)]
pub struct LeftPress {
    /// In global display coordinates, origin top left.
    pub location: CGPoint,
    /// When the press happened, by the event's own timestamp.
    pub at: Instant,
    /// Whether it is the second click of a double-click.
    pub double_click: bool,
}

/// Where the session tap sends every [`LeftPress`], called on the input thread while the press
/// waits for the tap: whatever the app it lands on does in answer comes later.
pub type OnLeftPress = Box<dyn Fn(LeftPress) + Send>;

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
