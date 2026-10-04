//! What the renderer tells the flight engine of each drain of batches it played: committed, or not
//! drawn for want of an overlay.

use rini_core::ids::WindowServerId;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Every batch up to `seq` is committed and flushed to the render server. `overlay` is the
    /// overlay's window as the window server numbers it, on every drain once it has: AppKit gives a
    /// window no number until the window server has numbered it.
    Committed { seq: u64, overlay: Option<WindowServerId> },
    /// The drain up to `seq` dressed or posed a flight, and there is no overlay to draw it on.
    Unavailable { seq: u64 },
}
