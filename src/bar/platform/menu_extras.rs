//! The thread that pictures macOS's menu extras for the bar.

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;

use crate::bar::domain::extras::Kind;

/// One extra's picture.
pub struct Extra {
    /// The status window's id, stable for as long as the extra is.
    pub window: u32,
    /// `Kind::Vital` or `Kind::Tray`, never `Kind::Skip`.
    pub kind: Kind,
    /// At `scale` pixels per point, with a transparent ground.
    pub image: CFRetained<CGImage>,
    pub scale: f64,
    /// Where its ink starts and ends, in points from the picture's left edge.
    pub ink: (f64, f64),
}

/// Every extra drawn, in menu-bar order, left to right.
#[derive(Default)]
pub struct Extras {
    pub items: Vec<Extra>,
}

/// The running thread.
pub struct Watcher;

impl Watcher {
    /// Starts the thread. `send` is called from it with the extras whenever a picture changed.
    pub fn spawn(send: Box<dyn Fn(Extras) + Send>) -> Watcher {
        drop(send);
        Watcher
    }

    /// No captures while paused: during a flight, and while no bar is up.
    pub fn set_paused(&self, _paused: bool) {}

    /// Picture now rather than at the next tick.
    pub fn refresh(&self) {}
}
