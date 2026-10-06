//! What a capture is asked for: the port the reactor, the flight engine and the switcher name a window
//! to picture through.

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::ScreenSize;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureTarget {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// The window's full size, as the layout intends it.
    pub size: ScreenSize,
}
