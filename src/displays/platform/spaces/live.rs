//! What the spaces actor reads of displays and spaces besides the `WindowServer` port, and the
//! readers production hands it.
use rini_core::ids::SpaceId;
use rustc_hash::FxHashMap as HashMap;

use crate::displays::domain::snapshot::SpaceKinds;
use crate::windows::platform::window_server;

impl SpaceKinds {
    /// What the window server says. The only classifier production uses.
    pub fn from_window_server() -> Self {
        Self {
            is_fullscreen: |space| {
                crate::displays::platform::space_query::space_is_fullscreen(space.get())
            },
            is_user: |space| crate::displays::platform::space_query::space_is_user(space.get()),
        }
    }
}

/// What the window server says about displays and spaces right now.
///
/// Injected rather than called, as with [`SpaceKinds`]: a rule reads what the test names;
/// `SpacesActor::new` installs the production reader.
#[derive(Clone, Copy)]
pub struct LiveDisplays {
    /// The display whose menu bar macOS considers active.
    pub(crate) active_menu_bar_display: fn() -> Option<String>,
    /// The space currently showing on that display.
    pub(crate) active_space: fn() -> Option<SpaceId>,
    /// Every managed space, per display UUID.
    pub(crate) spaces_per_display: fn() -> HashMap<String, Vec<SpaceId>>,
    /// No window has moved, appeared or gone for `WINDOWSERVER_QUIET_US`. The clock behind it is
    /// process-wide and every reactor test sets it, so a test that read it would see the others.
    pub(crate) window_server_quiet: fn() -> bool,
}

impl LiveDisplays {
    /// The window server. The only reader production uses.
    pub fn from_window_server() -> Self {
        Self {
            active_menu_bar_display: crate::displays::screen::active_menu_bar_display_uuid,
            active_space: crate::displays::screen::get_active_space_number,
            spaces_per_display: crate::displays::screen::managed_display_space_ids,
            window_server_quiet: || {
                window_server::windowserver_quiet_for_us(window_server::WINDOWSERVER_QUIET_US)
            },
        }
    }
}
