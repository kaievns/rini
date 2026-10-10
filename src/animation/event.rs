//! What the renderer tells the flight engine of each drain of batches it played: committed, with
//! when, or not drawn for want of an overlay.

use std::time::{Duration, Instant};

use rini_core::ids::WindowServerId;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Every batch up to `seq` is committed and flushed to the render server. `overlay` is the
    /// overlay's window as the window server numbers it, on every drain once it has: AppKit gives a
    /// window no number until the window server has numbered it. `shown` is whether the drain put the
    /// overlay up (`Some(true)`), down, or left it as it was, and `places` whether it placed what is
    /// drawn at rest; `crops` what it spent on crop-drawn tiles; `sent` when the oldest batch in it
    /// was sent, `began` when the renderer began playing it and `flushed` when it was flushed.
    Committed {
        seq: u64,
        overlay: Option<WindowServerId>,
        shown: Option<bool>,
        places: bool,
        crops: Crops,
        sent: Instant,
        began: Instant,
        flushed: Instant,
    },
    /// The drain up to `seq` dressed or posed a flight, and there is no overlay to draw it on.
    Unavailable { seq: u64 },
}

/// What a drain spent on crop-drawn tiles: how many tiles it drew on the crop grid, dressed, swapped
/// onto it or cut onto a repaint at a leg's corner, and how long reading their pictures' grounds
/// took (`Picture::ground`), each read once a picture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Crops {
    pub tiles: u32,
    pub ground: Duration,
}

impl Crops {
    /// One tile drawn: on the crop grid, its ground read in `ground`, or stretched (`None`).
    pub fn add(&mut self, ground: Option<Duration>) {
        if let Some(ground) = ground {
            self.tiles += 1;
            self.ground += ground;
        }
    }
}
