//! What the motion and transition tests stage with: the built-in display, a deterministic generator of
//! frames on it, a window on a surface and a tile.

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::WindowId;

use super::surface::{SurfaceGeometry, TileGeometry};

/// The built-in display the generator draws frames on.
pub(crate) const DISPLAY: CGRect = CGRect {
    origin: CGPoint { x: 0.0, y: 0.0 },
    size: CGSize { width: 1728.0, height: 1117.0 },
};

/// How many cases a property test draws.
pub(crate) const RUNS: usize = 200;

pub(crate) fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

/// A small deterministic generator, so a failure names its seed and replays.
pub(crate) struct Gen(pub u64);

impl Gen {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }

    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    pub(crate) fn pt(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.below((hi - lo) as u64 + 1) as f64
    }

    pub(crate) fn coin(&mut self) -> bool {
        self.below(2) == 0
    }

    /// A window frame with a real share of the display showing: a column on the strip.
    pub(crate) fn on_screen(&mut self) -> CGRect {
        let w = self.pt(400.0, 1720.0);
        let h = self.pt(600.0, 1081.0);
        let x = self.pt(-w / 4.0, DISPLAY.size.width - w * 0.75);
        rect(x, 32.0, w, h)
    }

    /// The layout's park for a scrolled-off window: 1pt showing in a bottom corner.
    pub(crate) fn park(&mut self, size: CGSize) -> CGRect {
        let x = if self.coin() {
            DISPLAY.size.width - 1.0
        } else {
            DISPLAY.origin.x - size.width + 1.0
        };
        rect(x, DISPLAY.size.height - 1.0, size.width, size.height)
    }
}

/// A window on a surface, as a test stages it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OnSurface {
    pub window: WindowId,
    pub frame: CGRect,
    pub pinned: bool,
    pub floating: bool,
    pub from: Option<CGRect>,
}

impl SurfaceGeometry for OnSurface {
    fn window(&self) -> WindowId { self.window }

    fn frame(&self) -> CGRect { self.frame }

    fn pinned(&self) -> bool { self.pinned }

    fn floating(&self) -> bool { self.floating }

    fn from(&self) -> Option<CGRect> { self.from }
}

/// A tile, as a test stages it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StagedTile {
    pub window: WindowId,
    pub from: CGRect,
    pub to: CGRect,
    pub floating: bool,
    pub companion: Option<WindowId>,
}

impl TileGeometry for StagedTile {
    fn window(&self) -> WindowId { self.window }

    fn from(&self) -> CGRect { self.from }

    fn to(&self) -> CGRect { self.to }

    fn floating(&self) -> bool { self.floating }

    fn companion(&self) -> Option<WindowId> { self.companion }
}
