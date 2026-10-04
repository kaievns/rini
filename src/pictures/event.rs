//! What the pictures service tells its clients: every background capture that comes back, for any
//! window, whoever asked for it.

use rini_core::ids::WindowId;

use crate::pictures::platform::picture::Picture;

#[derive(Debug, Clone)]
pub enum Event {
    /// The cache took this picture of `window`. The only kind a client may draw.
    Taken { window: WindowId, picture: Picture },
    /// A capture that covered `window`, refused for the picture the cache holds of it: one that
    /// carries the window's blur.
    Refused { window: WindowId },
}
