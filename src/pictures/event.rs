//! What the pictures service tells its clients: every capture the cache took, for any window, whoever
//! asked for it; and to each client, under its tag, the pictures it asked for or watched, and the
//! windows that never landed.

use objc2_core_foundation::CGRect;
use rini_core::ids::WindowId;

use crate::pictures::domain::watch::{Tag, Why};
use crate::pictures::platform::picture::Picture;

#[derive(Debug, Clone)]
pub enum Event {
    /// The cache took this picture of `window`. The only kind a client may draw.
    Taken { window: WindowId, picture: Picture },
    /// Pictures `tag` asked for or watched, each with the server frame it was taken at; all of them
    /// pictures the cache took. A frame-zero delivery comes even with none.
    Delivered {
        tag: Tag,
        why: Why,
        pictures: Vec<(WindowId, Picture, CGRect)>,
    },
    /// A window `tag` watched land was not where it ends by the deadline.
    Lost { tag: Tag, window: WindowId },
}
