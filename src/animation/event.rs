//! What the renderer tells the flight engine of each drain of batches it played: committed, with
//! when, or not drawn for want of an overlay.

use std::time::{Duration, Instant};

use rini_core::ids::WindowServerId;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Every batch up to `seq` is committed and flushed to the render server. `overlay` is the
    /// overlay's window as the window server numbers it, on every drain once it has: AppKit gives a
    /// window no number until the window server has numbered it. `played` is what the drain did;
    /// `sent` when the oldest batch in it was sent, `began` when the renderer began playing it and
    /// `flushed` when it was flushed.
    Committed {
        seq: u64,
        overlay: Option<WindowServerId>,
        played: Played,
        sent: Instant,
        began: Instant,
        flushed: Instant,
    },
    /// The drain up to `seq` dressed or posed a flight, and there is no overlay to draw it on.
    Unavailable { seq: u64 },
}

/// Which way a drain turned the overlay, when it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayTurn {
    Shown,
    Lifted,
}

/// Whether a drain placed what is drawn at rest where its windows are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestPlacement {
    Placed,
    Unplaced,
}

/// What a committed drain did: turned the overlay up or down, or left it as it was (`None`), placed
/// what is drawn at rest or not, and what it spent on crop-drawn tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Played {
    pub turn: Option<OverlayTurn>,
    pub rest: RestPlacement,
    pub crops: Crops,
}

/// What a drain spent on crop-drawn tiles: how many times it filled a crop grid, a tile dressed on
/// it, swapped onto it or cut onto a repaint at a leg's corner, one tile counting once a fill, and
/// how long reading their pictures' grounds took (`Picture::ground`), each read once a picture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Crops {
    pub fills: u32,
    pub ground: Duration,
}

impl Crops {
    /// One tile drawn: a fill of its crop grid, its ground read in `ground`, or stretched (`None`).
    pub fn add(&mut self, ground: Option<Duration>) {
        if let Some(ground) = ground {
            self.fills += 1;
            self.ground += ground;
        }
    }
}
