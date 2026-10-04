//! What the pictures service tells its clients: every capture the cache took, for any window, whoever
//! asked for it; and to each client, under its tag, the pictures it asked for or watched, and the
//! landings whose picture will not come.

use std::time::Instant;

use rini_core::ids::WindowId;
use rini_geometry::space::ScreenRect;

use crate::pictures::domain::watch::{Tag, Why};
use crate::pictures::platform::picture::Picture;

#[derive(Debug, Clone)]
pub enum Event {
    /// The cache took this picture of `window`. The only kind a client may draw.
    Taken { window: WindowId, picture: Picture },
    /// Pictures `tag` asked for or watched, each with the server frame it was taken at; all of them
    /// pictures the cache took. A frame-zero delivery comes even with none. `sent` is when the service
    /// decided it, once the captures behind it were done: from a picture's `taken` to `sent` is the
    /// round that took it.
    Delivered {
        tag: Tag,
        why: Why,
        sent: Instant,
        pictures: Vec<(WindowId, Picture, ScreenRect)>,
    },
    /// A window `tag` watched land whose picture will not come: it was not where it ends by the
    /// deadline, it never settled, or the cache refused its capture for the picture it holds.
    Lost { tag: Tag, window: WindowId },
}
