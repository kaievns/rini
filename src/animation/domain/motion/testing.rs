//! What the motion and transition tests stage with: the built-in display, a deterministic generator of
//! frames and passes on it, a window on a surface and a tile.

use rini_core::ids::WindowId;
use rini_geometry::space::{
    OverlayRect, Point, Rect, ScreenRect, ScreenVector, Size, SurfaceRect,
};
use rini_geometry::stretch::is_a_resize;

use super::surface::SurfaceGeometry;

/// The built-in display the generator draws frames on.
pub(crate) const DISPLAY: ScreenRect = Rect::new(Point::new(0.0, 0.0), Size::new(1728.0, 1117.0));

/// The overlay over `DISPLAY`: at the origin, so its points are the display's.
pub(crate) const VIEWPORT: OverlayRect = Rect::new(Point::new(0.0, 0.0), Size::new(1728.0, 1117.0));

/// How many cases a property test draws.
pub(crate) const RUNS: usize = 200;

pub(crate) fn rect<U>(x: f64, y: f64, w: f64, h: f64) -> Rect<U> {
    Rect::new(Point::new(x, y), Size::new(w, h))
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
    pub(crate) fn on_screen<U>(&mut self) -> Rect<U> {
        let w = self.pt(400.0, 1720.0);
        let h = self.pt(600.0, 1081.0);
        let x = self.pt(-w / 4.0, DISPLAY.size.width - w * 0.75);
        rect(x, 32.0, w, h)
    }

    /// The layout's park for a scrolled-off window: 1pt showing in a bottom corner.
    pub(crate) fn park<U>(&mut self, size: Size<U>) -> Rect<U> {
        let x = if self.coin() {
            DISPLAY.size.width - 1.0
        } else {
            DISPLAY.origin.x - size.width + 1.0
        };
        rect(x, DISPLAY.size.height - 1.0, size.width, size.height)
    }
}

/// A random pass: 1-12 windows over 1-4 distinct vectors (4pt apart on some axis, and from zero)
/// plus ±1pt jitter; some still, resizing, or floating. Requests on `DISPLAY`, as `reflow_plan`
/// takes them.
pub(crate) fn random_requests(rng: &mut Gen) -> Vec<(WindowId, ScreenRect, ScreenRect, bool)> {
    let shifted = |frame: ScreenRect, dx: f64, dy: f64| {
        rect(
            frame.origin.x + dx,
            frame.origin.y + dy,
            frame.size.width,
            frame.size.height,
        )
    };
    let mut palette: Vec<ScreenVector> = Vec::new();
    let wanted = 1 + rng.below(4) as usize;
    while palette.len() < wanted {
        let v = ScreenVector::new(
            rng.pt(-12.0, 12.0) * 50.0,
            if rng.coin() {
                0.0
            } else {
                rng.pt(-6.0, 6.0) * 50.0
            },
        );
        if (v.x == 0.0 && v.y == 0.0) || palette.contains(&v) {
            continue;
        }
        palette.push(v);
    }
    let count = 1 + rng.below(12) as usize;
    (0..count)
        .map(|i| {
            let from: ScreenRect = rng.on_screen();
            let window = WindowId::new(7, i as u32 + 1);
            match rng.below(10) {
                0 => (window, from, from, false),
                1 => {
                    let to = rect(
                        from.origin.x,
                        from.origin.y,
                        from.size.width + rng.pt(-300.0, 300.0),
                        from.size.height,
                    );
                    let to = if is_a_resize(from.size, to.size) {
                        to
                    } else {
                        rect(to.origin.x, to.origin.y, from.size.width + 200.0, to.size.height)
                    };
                    (window, from, to, false)
                }
                2 => {
                    let v = palette[rng.below(palette.len() as u64) as usize];
                    (window, from, shifted(from, v.x, v.y), true)
                }
                _ => {
                    let v = palette[rng.below(palette.len() as u64) as usize];
                    let (jx, jy) = (rng.pt(-1.0, 1.0), rng.pt(-1.0, 1.0));
                    (window, from, shifted(from, v.x + jx, v.y + jy), false)
                }
            }
        })
        .collect()
}

/// A window on a surface, as a test stages it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OnSurface {
    pub window: WindowId,
    pub frame: SurfaceRect,
    pub pinned: bool,
    pub floating: bool,
    pub from: Option<SurfaceRect>,
}

impl SurfaceGeometry for OnSurface {
    fn window(&self) -> WindowId { self.window }

    fn frame(&self) -> SurfaceRect { self.frame }

    fn pinned(&self) -> bool { self.pinned }

    fn floating(&self) -> bool { self.floating }

    fn from(&self) -> Option<SurfaceRect> { self.from }
}
