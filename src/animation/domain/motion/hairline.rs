//! The hairline macOS composites around every window, which no capture carries: a light ring on the
//! window's outermost point and a dark outline just outside it, drawn over every tile as constants.
//! Measured in "The hairline is composited outside every capture" in
//! `src/pictures/docs/capture-research.md`; drawn as "The hairline" in
//! `src/animation/docs/animation-smoothness.md`.

use rini_geometry::space::{Point, Size, TileRect, TileSize};

use crate::pictures::domain::knowledge::CORNER_RADIUS;

/// The ring's width in points: two device pixels at 2x.
pub const RING_PT: f64 = 1.0;

/// The outline's stroke width in points, and its black's alpha.
pub const OUTLINE_PT: f64 = 0.5;
pub const OUTLINE_ALPHA: f64 = 0.9;

/// The alpha of the ring's white: a focused window's ring is the brighter.
pub fn ring_alpha(focused: bool) -> f64 {
    if focused { 0.20 } else { 0.18 }
}

/// A tile's corner radius: its window's, measured, else `CORNER_RADIUS`, clamped to half the
/// shorter side because Core Graphics clamps silently.
pub fn corner_radius(measured: Option<f64>, size: TileSize) -> f64 {
    let shorter = size.width.min(size.height);
    measured.unwrap_or(CORNER_RADIUS).min(shorter / 2.0).max(0.0)
}

/// The outline's rounded rect around a window of `size` with corners of `corner`, and that rect's
/// corner radius: centred half a stroke outside the edge, so the stroke covers exactly the
/// `OUTLINE_PT` beyond it.
pub fn outline(size: TileSize, corner: f64) -> (TileRect, f64) {
    let half = OUTLINE_PT / 2.0;
    let rect = TileRect::new(
        Point::new(-half, -half),
        Size::new(
            size.width.max(0.0) + OUTLINE_PT,
            size.height.max(0.0) + OUTLINE_PT,
        ),
    );
    (rect, corner + half)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> TileRect {
        TileRect::new(Point::new(x, y), Size::new(w, h))
    }

    #[test]
    fn the_outline_is_a_quarter_point_outside_the_window_and_a_quarter_point_rounder() {
        assert_eq!(
            outline(TileSize::new(859.0, 1081.0), 10.0),
            (rect(-0.25, -0.25, 859.5, 1081.5), 10.25)
        );
    }

    #[test]
    fn the_outline_stroke_covers_the_half_point_just_outside_the_edge() {
        let (path, _) = outline(TileSize::new(100.0, 60.0), 10.0);
        let half = OUTLINE_PT / 2.0;
        assert_eq!(path.origin.x - half, -0.5, "its outer side");
        assert_eq!(path.origin.x + half, 0.0, "its inner side, on the window's edge");
        assert_eq!(path.origin.x + path.size.width + half, 100.5);
    }

    /// Core Graphics takes a rounded rect only when no corner is wider than half its side, and an
    /// entrance grows from no width at all.
    #[test]
    fn the_outlines_corners_always_fit_its_rect() {
        for (w, h) in [
            (0.0, 1081.0),
            (0.5, 1081.0),
            (12.0, 400.0),
            (859.0, 1081.0),
            (-3.0, 10.0),
        ] {
            let size = TileSize::new(w, h);
            for measured in [None, Some(16.0)] {
                let (rect, radius) = outline(size, corner_radius(measured, size));
                assert!(2.0 * radius <= rect.size.width, "{w}x{h} {measured:?}");
                assert!(2.0 * radius <= rect.size.height, "{w}x{h} {measured:?}");
            }
        }
    }

    #[test]
    fn a_measured_radius_is_the_windows_own() {
        assert_eq!(corner_radius(Some(16.0), TileSize::new(859.0, 1081.0)), 16.0);
        assert_eq!(corner_radius(Some(4.5), TileSize::new(859.0, 1081.0)), 4.5);
    }

    #[test]
    fn a_window_not_yet_measured_gets_the_fallback_radius() {
        assert_eq!(corner_radius(None, TileSize::new(859.0, 1081.0)), CORNER_RADIUS);
        assert_eq!(corner_radius(None, TileSize::new(1720.0, 1081.0)), CORNER_RADIUS);
    }

    #[test]
    fn a_tile_thinner_than_its_radius_is_clamped_to_half_its_shorter_side() {
        assert_eq!(corner_radius(None, TileSize::new(12.0, 400.0)), 6.0);
        assert_eq!(corner_radius(None, TileSize::new(400.0, 8.0)), 4.0);
        assert_eq!(corner_radius(Some(16.0), TileSize::new(20.0, 400.0)), 10.0);
    }

    #[test]
    fn a_zero_sized_tile_has_no_corners_rather_than_negative_ones() {
        assert_eq!(corner_radius(None, TileSize::new(0.0, 0.0)), 0.0);
        assert_eq!(corner_radius(Some(16.0), TileSize::new(-10.0, 100.0)), 0.0);
    }

    #[test]
    fn the_focused_windows_ring_is_the_brighter() {
        assert_eq!(ring_alpha(true), 0.20);
        assert_eq!(ring_alpha(false), 0.18);
    }
}
