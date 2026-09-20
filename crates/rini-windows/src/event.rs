//! What the windows context tells the application: one event per thing the per-app actor learns
//! from Accessibility. The reactor converts these into its own event type; this crate never
//! imports the reactor.
use objc2_core_foundation::CGRect;

use crate::app::{AppInfo, WindowInfo};
use crate::app_actor::{AppThreadHandle, Quiet};
use crate::ids::{WindowId, pid_t};
use crate::mouse::MouseState;
use crate::transaction::{Requested, TransactionId};
use crate::window_server::WindowServerInfo;

#[derive(Debug)]
pub enum Event {
    /// Sent once per running app at startup and on every launch, with its windows as found.
    ApplicationLaunched {
        pid: pid_t,
        info: AppInfo,
        handle: AppThreadHandle,
        is_frontmost: bool,
        main_window: Option<WindowId>,
        visible_windows: Vec<(WindowId, WindowInfo)>,
        window_server_info: Vec<WindowServerInfo>,
    },
    ApplicationThreadTerminated(pid_t),
    ApplicationActivated(pid_t, Quiet),
    ApplicationDeactivated(pid_t),
    ApplicationMainWindowChanged(pid_t, Option<WindowId>, Quiet),
    WindowsDiscovered {
        pid: pid_t,
        new: Vec<(WindowId, WindowInfo)>,
        known_visible: Vec<WindowId>,
    },
    WindowCreated(WindowId, WindowInfo, Option<WindowServerInfo>, Option<MouseState>),
    WindowDestroyed(WindowId),
    WindowMinimized(WindowId),
    WindowDeminiaturized(WindowId),
    /// `Requested` says whether the frame is the echo of a frame rini asked for.
    WindowFrameChanged(WindowId, CGRect, Option<TransactionId>, Requested, Option<MouseState>),
    WindowTitleChanged(WindowId, String),
    MenuOpened(pid_t),
    MenuClosed(pid_t),
    RaiseCompleted { window_id: WindowId, sequence_id: u64 },
    /// A raise sequence ran past the raise manager's deadline; its pending raises are dropped.
    RaiseTimeout { sequence_id: u64 },
}

/// Where the per-app actor delivers its events. Implemented for any channel whose message type
/// can be built from [`Event`], so the application wraps without a forwarding hop.
pub trait EventSink: Send {
    fn send(&self, event: Event);
}

impl<T: From<Event> + Send> EventSink for rini_runloop::channel::Sender<T> {
    fn send(&self, event: Event) {
        rini_runloop::channel::Sender::send(self, event.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    enum AppEvent {
        Window(Event),
    }
    impl From<Event> for AppEvent {
        fn from(event: Event) -> Self {
            AppEvent::Window(event)
        }
    }

    #[test]
    fn a_channel_of_any_wrapping_type_is_a_sink() {
        let (tx, mut rx) = rini_runloop::channel::channel::<AppEvent>();
        let sink: Box<dyn EventSink> = Box::new(tx);
        sink.send(Event::MenuOpened(42));
        let (_, AppEvent::Window(Event::MenuOpened(pid))) = rx.try_recv().unwrap() else {
            panic!("event not delivered through the sink");
        };
        assert_eq!(pid, 42);
    }
}
