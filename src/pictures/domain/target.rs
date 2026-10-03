//! What a capture is asked for: the port the reactor, the flight engine and the switcher name a window
//! to picture through.

use objc2_core_foundation::CGSize;
use rini_core::ids::{WindowId, WindowServerId};

#[derive(Debug, Clone, Copy)]
pub struct PictureTarget {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// The window's full size in points, as the layout intends it.
    pub size: CGSize,
}
