//! What the reactor asks of the machine, handed to it as one value: the window server, the pointer,
//! Low Power Mode, the displays' bounds, display churn, the space kinds and the running
//! applications, with the two notes it hands back, the window-server activity clock and the windows
//! it wants notifications for.
//!
//! Production builds the live value in `Reactor::new`; a test reactor gets [`SystemReads::fake`],
//! where every read answers "unknown" until the test states it on the `FakeWindowServer` or swaps
//! one field. A reducer takes the answer, or the port, as a parameter and never reads the machine.

use std::sync::Arc;

use objc2_core_foundation::CGRect;
use rini_core::ids::{ScreenId, WindowServerId, pid_t};

use crate::displays::platform::spaces::SpaceKinds;
use crate::windows::domain::info::{AppInfo, WindowServerInfo};
use crate::windows::domain::manageable::Facts;
use crate::windows::platform::mouse::MouseState;
#[cfg(test)]
use crate::windows::platform::window_server::FakeWindowServer;
use crate::windows::platform::window_server::{self, WindowServer};

pub(crate) struct SystemReads {
    pub(crate) window_server: Arc<dyn WindowServer>,
    /// Whether the pointer's button is down, as the event tap last saw it.
    pub(crate) mouse: fn() -> Option<MouseState>,
    pub(crate) low_power: fn() -> bool,
    /// A display's whole bounds, menu bar and Dock included, in the overlay's coordinates.
    pub(crate) display_bounds: fn(ScreenId) -> CGRect,
    /// Whether the spaces actor is mid display reconfiguration. The production flag is
    /// process-wide, so a test that read it would be quarantined by a spaces test beside it.
    pub(crate) display_churn: fn() -> bool,
    pub(crate) space_kinds: SpaceKinds,
    /// The running application `pid` is, for a window-server window of an app with no thread yet.
    pub(crate) running_app: fn(pid_t) -> Option<AppInfo>,
    /// That the window server just moved a window: the spaces actor waits for it to go quiet
    /// before it trusts a snapshot.
    pub(crate) note_activity: fn(u32),
    /// The windows the window server reports moves and closes of.
    pub(crate) watch_windows: fn(&[u32]),
}

impl SystemReads {
    pub(crate) fn live() -> Self { Self::live_with(window_server::live()) }

    /// The live reads around `window_server`, which a test build can only give as a fake.
    pub(crate) fn live_with(window_server: Arc<dyn WindowServer>) -> Self {
        SystemReads {
            window_server,
            mouse: crate::windows::platform::mouse::get_mouse_state,
            low_power: crate::app::power::is_low_power_mode_enabled,
            display_bounds: |screen| objc2_core_graphics::CGDisplayBounds(screen.as_u32()),
            display_churn: crate::displays::platform::display_churn::is_active,
            space_kinds: SpaceKinds::from_window_server(),
            running_app: |pid| {
                objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                    .map(|app| AppInfo::from(&*app))
            },
            note_activity: window_server::note_windowserver_activity,
            watch_windows: crate::displays::platform::cgs_notify::update_window_notifications,
        }
    }

    /// Reads that know nothing: the window server is `window_server`, the pointer's state is
    /// unknown, every display's bounds are empty, no application is running, nothing is in Low
    /// Power Mode or churning, and the notes go nowhere.
    #[cfg(test)]
    pub(crate) fn fake(window_server: Arc<FakeWindowServer>) -> Self {
        SystemReads {
            window_server,
            mouse: || None,
            low_power: || false,
            display_bounds: |_| CGRect::ZERO,
            display_churn: || false,
            space_kinds: SpaceKinds::for_tests(),
            running_app: |_| None,
            note_activity: |_| {},
            watch_windows: |_| {},
        }
    }
}

/// `facts` with the window server's answers about `id` in, `known` the catalogue's record of a
/// window; none asked for a window with no window-server id.
pub(crate) fn server_facts(
    window_server: &dyn WindowServer,
    facts: Facts,
    id: Option<WindowServerId>,
    known: impl FnOnce(WindowServerId) -> Option<WindowServerInfo>,
) -> Facts {
    match id {
        Some(id) => window_server.with_server_facts(facts, id, known(id).as_ref()),
        None => facts,
    }
}
