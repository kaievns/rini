//! The handover check: how far the screen just after a lift differs from what the overlay drew just
//! before it, inside each window that ends on screen, either side of its outline, and over a band
//! drawn outside it. Pure, over RGBA
//! bytes; `crate::pictures::platform::service` takes the two pictures. The sampling is the one "The
//! handover, measured" in `src/animation/docs/capture-overlay-research.md` was measured with, so its
//! numbers and these compare.

use std::time::Duration;

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{ScreenPoint, ScreenRect};

use crate::pictures::domain::watch::Tag;

/// How long after the overlay's own picture the screen is taken: the 45ms the research waited after
/// the hide for the window server to show what was under the overlay.
pub const AFTER_LIFT: Duration = Duration::from_millis(45);

/// The interior's inset from the window's edge, and its sampling step.
const INSET: f64 = 4.0;
const INTERIOR_STEP: f64 = 1.5;
/// An interior sample this many levels off counts towards `interior_over`.
const OVER: f64 = 12.0;
/// Where the edge is sampled across the outline, negative outside the window, and along it.
const ACROSS: [f64; 4] = [-0.75, -0.25, 0.25, 0.75];
const ALONG_STEP: f64 = 2.0;
/// How far the edge samples keep from the corners.
const CORNER: f64 = 16.0;
/// How deep the band samples start past the edge, beyond the outline's half point, and how far apart
/// they lie across it.
const BAND_FROM: f64 = 0.75;
const BAND_STEP: f64 = 0.5;

/// A lift to measure: the overlay window, where it starts on screen, the display it covers and its
/// scale, and every window ending wholly on screen, where it ends and how wide a band is drawn
/// outside it at rest, 0 with none.
#[derive(Clone, Debug, PartialEq)]
pub struct HandoverCheck {
    pub tag: Tag,
    pub overlay: WindowServerId,
    /// Where the overlay window's top-left is on screen, as the overlay places it.
    pub overlay_origin: ScreenPoint,
    pub display: ScreenRect,
    pub scale: f64,
    pub windows: Vec<(WindowId, ScreenRect, f64)>,
}

/// A picture as RGBA8 bytes, top row first, and where its top-left pixel lies on screen at `scale`
/// pixels a point.
pub struct Rgba<'a> {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bytes: &'a [u8],
    pub origin: ScreenPoint,
    pub scale: f64,
}

impl Rgba<'_> {
    /// The pixel at the screen point `(x, y)`; `None` off the picture.
    fn at(&self, x: f64, y: f64) -> Option<[u8; 3]> {
        let column = ((x - self.origin.x) * self.scale).floor();
        let row = ((y - self.origin.y) * self.scale).floor();
        if column < 0.0 || row < 0.0 || column as usize >= self.width || row as usize >= self.height
        {
            return None;
        }
        let at = row as usize * self.stride + column as usize * 4;
        let pixel = self.bytes.get(at..at + 3)?;
        Some([pixel[0], pixel[1], pixel[2]])
    }
}

/// How far the screen after a lift is from the overlay before it over one window: the mean of the RGB
/// channels' absolute differences, in levels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandoverDiff {
    /// Inside the window, `INSET` in from its edge.
    pub interior: f32,
    /// The share of the interior's samples more than `OVER` levels off.
    pub interior_over: f32,
    /// Just inside the outline, half a point either side of a quarter in...
    pub edge_in: f32,
    /// ... and just outside it.
    pub edge_out: f32,
    /// Over the band drawn outside the window, past the outline; `None` with no band.
    pub band: Option<f32>,
}

#[derive(Default)]
struct Mean {
    sum: f64,
    n: usize,
}

impl Mean {
    fn add(&mut self, value: f64) {
        self.sum += value;
        self.n += 1;
    }

    fn get(&self) -> Option<f32> { (self.n > 0).then(|| (self.sum / self.n as f64) as f32) }
}

/// `overlay` against `screen` over `window`, in screen points, and over the band `band` wide outside
/// it. `None` when either picture leaves the window with no sample inside it, or none on either side
/// of its outline.
pub fn compare(
    overlay: &Rgba,
    screen: &Rgba,
    window: ScreenRect,
    band: f64,
) -> Option<HandoverDiff> {
    let diff = |x: f64, y: f64| -> Option<f64> {
        let (a, b) = (overlay.at(x, y)?, screen.at(x, y)?);
        Some((0..3).map(|k| (f64::from(a[k]) - f64::from(b[k])).abs()).sum::<f64>() / 3.0)
    };
    let (left, top) = (window.origin.x, window.origin.y);
    let (right, bottom) = (left + window.size.width, top + window.size.height);

    let (mut interior, mut over) = (Mean::default(), 0usize);
    for y in steps(top + INSET, bottom - INSET, INTERIOR_STEP) {
        for x in steps(left + INSET, right - INSET, INTERIOR_STEP) {
            if let Some(d) = diff(x, y) {
                interior.add(d);
                over += usize::from(d > OVER);
            }
        }
    }

    let (mut inside, mut outside) = (Mean::default(), Mean::default());
    for across in ACROSS {
        let side = if across > 0.0 {
            &mut inside
        } else {
            &mut outside
        };
        for x in steps(left + CORNER, right - CORNER, ALONG_STEP) {
            for y in [top + across, bottom - across] {
                if let Some(d) = diff(x, y) {
                    side.add(d);
                }
            }
        }
        for y in steps(top + CORNER, bottom - CORNER, ALONG_STEP) {
            for x in [left + across, right - across] {
                if let Some(d) = diff(x, y) {
                    side.add(d);
                }
            }
        }
    }

    let mut banded = Mean::default();
    for depth in steps(BAND_FROM, band, BAND_STEP) {
        for x in steps(left + CORNER, right - CORNER, ALONG_STEP) {
            for y in [top - depth, bottom + depth] {
                if let Some(d) = diff(x, y) {
                    banded.add(d);
                }
            }
        }
        for y in steps(top + CORNER, bottom - CORNER, ALONG_STEP) {
            for x in [left - depth, right + depth] {
                if let Some(d) = diff(x, y) {
                    banded.add(d);
                }
            }
        }
    }

    Some(HandoverDiff {
        interior: interior.get()?,
        interior_over: over as f32 / interior.n as f32,
        edge_in: inside.get()?,
        edge_out: outside.get()?,
        band: banded.get(),
    })
}

/// `from`, `from + step`, ... while below `to`.
fn steps(from: f64, to: f64, step: f64) -> impl Iterator<Item = f64> {
    (0..).map(move |i| from + f64::from(i) * step).take_while(move |at| *at < to)
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::ScreenSize;

    use super::*;

    const SCALE: f64 = 2.0;
    const SIDE: usize = 400;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> ScreenRect {
        ScreenRect::new(ScreenPoint::new(x, y), ScreenSize::new(w, h))
    }

    /// A 200pt square picture at 2x of a grey ground with `window` drawn white with a black outline
    /// one pixel outside it, as `paint` changes it.
    fn picture(window: ScreenRect, paint: impl Fn(&mut [u8], usize, usize)) -> Vec<u8> {
        let mut bytes = vec![0u8; SIDE * SIDE * 4];
        for row in 0..SIDE {
            for column in 0..SIDE {
                let (x, y) = (column as f64 / SCALE, row as f64 / SCALE);
                let inside = |grow: f64| {
                    x >= window.origin.x - grow
                        && x < window.origin.x + window.size.width + grow
                        && y >= window.origin.y - grow
                        && y < window.origin.y + window.size.height + grow
                };
                let level = if inside(0.0) {
                    255
                } else if inside(0.5) {
                    0
                } else {
                    128
                };
                let at = (row * SIDE + column) * 4;
                bytes[at..at + 4].copy_from_slice(&[level, level, level, 255]);
            }
        }
        for row in 0..SIDE {
            for column in 0..SIDE {
                paint(&mut bytes[(row * SIDE + column) * 4..][..4], column, row);
            }
        }
        bytes
    }

    fn rgba(bytes: &[u8], origin: ScreenPoint) -> Rgba<'_> {
        Rgba {
            width: SIDE,
            height: SIDE,
            stride: SIDE * 4,
            bytes,
            origin,
            scale: SCALE,
        }
    }

    fn window() -> ScreenRect { rect(40.0, 40.0, 120.0, 100.0) }

    fn untouched(_: &mut [u8], _: usize, _: usize) {}

    fn compared(overlay: &[u8], screen: &[u8]) -> Option<HandoverDiff> {
        compare(
            &rgba(overlay, ScreenPoint::origin()),
            &rgba(screen, ScreenPoint::origin()),
            window(),
            0.0,
        )
    }

    #[test]
    fn identical_pictures_differ_by_nothing() {
        let drawn = picture(window(), untouched);
        assert_eq!(
            compared(&drawn, &drawn),
            Some(HandoverDiff {
                interior: 0.0,
                interior_over: 0.0,
                edge_in: 0.0,
                edge_out: 0.0,
                band: None
            })
        );
    }

    /// A window that ends a pixel off its tile: its outline moves, its insides do not.
    #[test]
    fn an_outline_a_pixel_off_shows_at_the_edge_only() {
        let overlay = picture(window(), untouched);
        let screen = picture(
            window().translate(rini_geometry::space::ScreenVector::new(0.5, 0.5)),
            untouched,
        );
        let diff = compared(&overlay, &screen).unwrap();
        assert_eq!(diff.interior, 0.0);
        assert!(diff.edge_in > 10.0 && diff.edge_out > 10.0, "{diff:?}");
    }

    /// The tile's white line drawn in, missing from the screen: inside the outline only.
    #[test]
    fn a_white_line_missing_shows_just_inside_the_edge() {
        let w = window();
        let white_line = |pixel: &mut [u8], column: usize, row: usize| {
            let (x, y) = (column as f64 / SCALE, row as f64 / SCALE);
            let inside = x >= w.origin.x && x < w.max_x() && y >= w.origin.y && y < w.max_y();
            let near = x < w.origin.x + 1.0
                || x >= w.max_x() - 1.0
                || y < w.origin.y + 1.0
                || y >= w.max_y() - 1.0;
            if inside && near {
                pixel[..3].copy_from_slice(&[200, 200, 200]);
            }
        };
        let diff = compared(&picture(w, untouched), &picture(w, white_line)).unwrap();
        assert!(diff.edge_in > 40.0, "{diff:?}");
        assert_eq!(diff.edge_out, 0.0);
        assert_eq!(diff.interior, 0.0, "the white line is inside the inset");
    }

    /// The app repainted under the overlay: the screen shows a body the tile did not.
    #[test]
    fn an_interior_repainted_shows_inside_and_counts_over() {
        let w = window();
        let repainted = |pixel: &mut [u8], column: usize, row: usize| {
            let (x, y) = (column as f64 / SCALE, row as f64 / SCALE);
            if x >= w.origin.x + 20.0
                && x < w.max_x() - 20.0
                && y >= w.origin.y + 20.0
                && y < w.max_y() - 20.0
            {
                pixel[..3].copy_from_slice(&[40, 40, 40]);
            }
        };
        let diff = compared(&picture(w, untouched), &picture(w, repainted)).unwrap();
        assert!(diff.interior > 50.0, "{diff:?}");
        assert!(diff.interior_over > 0.3 && diff.interior_over < 0.6, "{diff:?}");
        assert_eq!((diff.edge_in, diff.edge_out), (0.0, 0.0));
    }

    /// Each picture is placed at its own origin: the overlay starts below the bar's band.
    #[test]
    fn each_picture_is_read_at_its_own_origin() {
        let below = ScreenPoint::new(0.0, 32.0);
        let shifted = window().translate(rini_geometry::space::ScreenVector::new(0.0, -32.0));
        let overlay = picture(shifted, untouched);
        let screen = picture(window(), untouched);
        let diff = compare(
            &rgba(&overlay, below),
            &rgba(&screen, ScreenPoint::origin()),
            window(),
            0.0,
        )
        .unwrap();
        assert_eq!(diff.interior, 0.0);
    }

    #[test]
    fn a_window_off_either_picture_is_not_compared() {
        let drawn = picture(window(), untouched);
        let off = ScreenPoint::new(500.0, 0.0);
        assert_eq!(
            compare(
                &rgba(&drawn, off),
                &rgba(&drawn, ScreenPoint::origin()),
                window(),
                0.0
            ),
            None
        );
        assert_eq!(
            compare(
                &rgba(&drawn, ScreenPoint::origin()),
                &rgba(&drawn, off),
                window(),
                0.0
            ),
            None
        );
    }

    /// The tile's ring drawn on the overlay and no ring at rest: over the band, and nowhere else.
    #[test]
    fn a_band_missing_shows_outside_the_edge() {
        let w = window();
        let ring = |pixel: &mut [u8], column: usize, row: usize| {
            let (x, y) = (column as f64 / SCALE, row as f64 / SCALE);
            let within = |grow: f64| {
                x >= w.origin.x - grow
                    && x < w.max_x() + grow
                    && y >= w.origin.y - grow
                    && y < w.max_y() + grow
            };
            if within(4.0) && !within(0.5) {
                pixel[..3].copy_from_slice(&[127, 200, 255]);
            }
        };
        let both = |overlay: &[u8], screen: &[u8]| {
            compare(
                &rgba(overlay, ScreenPoint::origin()),
                &rgba(screen, ScreenPoint::origin()),
                w,
                4.0,
            )
            .unwrap()
        };
        let (tile, rest, bare) = (picture(w, ring), picture(w, ring), picture(w, untouched));
        let missing = both(&tile, &bare);
        assert!(missing.band.is_some_and(|band| band > 50.0), "{missing:?}");
        assert_eq!((missing.interior, missing.edge_in), (0.0, 0.0));
        assert_eq!(
            both(&tile, &rest).band,
            Some(0.0),
            "drawn the same on both sides"
        );
    }

    #[test]
    fn a_window_with_no_band_reports_none() {
        let drawn = picture(window(), untouched);
        assert_eq!(compared(&drawn, &drawn).and_then(|diff| diff.band), None);
        let thin = compare(
            &rgba(&drawn, ScreenPoint::origin()),
            &rgba(&drawn, ScreenPoint::origin()),
            window(),
            0.5,
        );
        assert_eq!(
            thin.and_then(|diff| diff.band),
            None,
            "a band no wider than the outline"
        );
    }
}
