//! What a capture is asked for: the port the reactor, the flight engine and the switcher name a window
//! to picture through, and the id a window rini does not manage is pictured under.

use objc2_core_foundation::CGSize;
use rini_core::ids::{WindowId, WindowServerId};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureTarget {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// The window's full size in points, as the layout intends it.
    pub size: CGSize,
}

/// A stable [`WindowId`] for a window rini does not manage, derived from its window server id: a border
/// window, or any window the debug commands picture. Pid 0 keeps it clear of real ids.
pub fn synthetic_window_id(server_id: WindowServerId) -> WindowId {
    WindowId {
        pid: 0,
        idx: std::num::NonZeroU32::new(server_id.as_u32().max(1)).unwrap(),
    }
}
