//! Where a window's decorations lie and how round they are, in its tile's own points: the corner
//! radius, the hairline's outline, the shadow's silhouette and the mask it is cut to.

use rini_geometry::space::{Point, Size, TileRect, TileSize};

use crate::decorations::domain::style::OUTLINE_PT;

/// The corner radius a window is drawn with until its own is measured, in points: a macOS window's,
/// measured. See "The hairline is composited outside every capture" in
/// `src/pictures/docs/capture-research.md`.
pub const CORNER_RADIUS: f64 = 10.0;

/// Room the mask leaves for the shadow: past where the focused blur (three radii plus the offset,
/// about 54pt) is spent, so the mask never clips it.
pub const SHADOW_REACH: f64 = 70.0;

/// A tile's corner radius: its window's, measured, else `CORNER_RADIUS`, clamped to half the
/// shorter side because Core Graphics clamps silently.
pub fn radius(measured: Option<f64>, size: TileSize) -> f64 {
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

/// The shadow mask's rect: the window grown by `SHADOW_REACH` on every side.
pub fn shadow_mask_frame(size: TileSize) -> TileRect {
    TileRect::new(
        Point::new(-SHADOW_REACH, -SHADOW_REACH),
        Size::new(size.width + 2.0 * SHADOW_REACH, size.height + 2.0 * SHADOW_REACH),
    )
}

/// The window's rounded-rect silhouette, the shadow's shape, and its corner radius. Size and radius
/// are floored above zero, or Core Graphics emits a plain rect and path animations cut.
pub fn silhouette(size: TileSize) -> (TileRect, f64) {
    let size = TileSize::new(size.width.max(0.5), size.height.max(0.5));
    (
        TileRect::new(Point::origin(), size),
        radius(None, size).max(0.01),
    )
}

/// The shadow mask's shape: its whole rect, and the silhouette as the hole wound even-odd inside it,
/// with the hole's corner radius. Floored as the silhouette is, so it is always one rect plus one
/// rounded rect and two masks interpolate.
pub fn shadow_mask(size: TileSize) -> (TileRect, TileRect, f64) {
    let (hole, corner) = silhouette(size);
    (shadow_mask_frame(hole.size), hole, corner)
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
                let (rect, corner) = outline(size, radius(measured, size));
                assert!(2.0 * corner <= rect.size.width, "{w}x{h} {measured:?}");
                assert!(2.0 * corner <= rect.size.height, "{w}x{h} {measured:?}");
            }
        }
    }

    #[test]
    fn a_measured_radius_is_the_windows_own() {
        assert_eq!(radius(Some(16.0), TileSize::new(859.0, 1081.0)), 16.0);
        assert_eq!(radius(Some(4.5), TileSize::new(859.0, 1081.0)), 4.5);
    }

    #[test]
    fn a_window_not_yet_measured_gets_the_fallback_radius() {
        assert_eq!(radius(None, TileSize::new(859.0, 1081.0)), CORNER_RADIUS);
        assert_eq!(radius(None, TileSize::new(1720.0, 1081.0)), CORNER_RADIUS);
    }

    #[test]
    fn a_tile_thinner_than_its_radius_is_clamped_to_half_its_shorter_side() {
        assert_eq!(radius(None, TileSize::new(12.0, 400.0)), 6.0);
        assert_eq!(radius(None, TileSize::new(400.0, 8.0)), 4.0);
        assert_eq!(radius(Some(16.0), TileSize::new(20.0, 400.0)), 10.0);
    }

    #[test]
    fn a_zero_sized_tile_has_no_corners_rather_than_negative_ones() {
        assert_eq!(radius(None, TileSize::new(0.0, 0.0)), 0.0);
        assert_eq!(radius(Some(16.0), TileSize::new(-10.0, 100.0)), 0.0);
    }

    #[test]
    fn the_shadow_mask_surrounds_the_window_by_one_reach() {
        assert_eq!(
            shadow_mask_frame(TileSize::new(859.0, 1081.0)),
            rect(
                -SHADOW_REACH,
                -SHADOW_REACH,
                859.0 + 2.0 * SHADOW_REACH,
                1081.0 + 2.0 * SHADOW_REACH
            )
        );
    }

    /// An entrance grows from no width, and Core Graphics draws a zero-sized or square-cornered
    /// rounded rect as a plain one, which a path animation cannot interpolate into.
    #[test]
    fn the_silhouette_is_floored_so_two_of_them_interpolate() {
        for width in [0.0, -3.0] {
            let (floored, corner) = silhouette(TileSize::new(width, 1081.0));
            assert_eq!(floored, rect(0.0, 0.0, 0.5, 1081.0), "{width} wide");
            assert!(corner >= 0.01, "{width} wide");
        }
        assert_eq!(
            silhouette(TileSize::new(859.0, 1081.0)),
            (rect(0.0, 0.0, 859.0, 1081.0), CORNER_RADIUS)
        );
    }

    #[test]
    fn the_shadow_masks_hole_is_the_silhouette_one_reach_inside_its_whole() {
        let size = TileSize::new(859.0, 1081.0);
        let (whole, hole, corner) = shadow_mask(size);
        assert_eq!(whole, rect(-70.0, -70.0, 999.0, 1221.0));
        assert_eq!((hole, corner), silhouette(size));
        assert_eq!(hole.origin, Point::origin());
    }
}
