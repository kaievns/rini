//! Where a window's decorations lie and how round they are, in its tile's own points: the corner
//! radius, the hairline's outline, the shadow's silhouette and the mask it is cut to.

use rini_geometry::space::{Point, Size, Tile, TileRect, TileSize};

use crate::decorations::domain::style::OUTLINE_PT;

/// The corner radius of a window the window server reports none for, in points: the commonest a
/// standard window reports. See "The corner, measured" in `src/decorations/docs/README.md`.
pub const FALLBACK_RADIUS: f64 = 16.0;

/// Room the mask leaves for the shadow: past where the focused blur (three radii plus the offset,
/// about 54pt) is spent, so the mask never clips it.
pub const SHADOW_REACH: f64 = 70.0;

/// A tile's corner radius: its window's, as the window server reports it, else `FALLBACK_RADIUS`,
/// clamped to half the shorter side because Core Graphics clamps silently.
pub fn radius(reported: Option<f64>, size: TileSize) -> f64 {
    let shorter = size.width.min(size.height);
    reported.unwrap_or(FALLBACK_RADIUS).min(shorter / 2.0).max(0.0)
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

/// The window's rounded-rect silhouette with corners of `corner`, the shadow's shape, and its corner
/// radius. Size and radius are floored above zero, or Core Graphics emits a plain rect and path
/// animations cut.
pub fn silhouette(size: TileSize, corner: f64) -> (TileRect, f64) {
    let size = TileSize::new(size.width.max(0.5), size.height.max(0.5));
    (TileRect::new(Point::origin(), size), corner.max(0.01))
}

/// The shadow mask's shape: its whole rect, and the silhouette with corners of `corner` as the hole
/// wound even-odd inside it, with the hole's corner radius. Floored as the silhouette is, so it is
/// always one rect plus one rounded rect and two masks interpolate.
pub fn shadow_mask(size: TileSize, corner: f64) -> (TileRect, TileRect, f64) {
    let (hole, corner) = silhouette(size, corner);
    (shadow_mask_frame(hole.size), hole, corner)
}


/// How far along each side a continuous corner reaches, in radii: Core Animation's
/// `cornerCurveExpansionFactor` for `kCACornerCurveContinuous`, 1.528665 on macOS 26.7.1.
pub const CONTINUOUS_REACH: f64 = 1.528665;

/// The two curves a corner can round on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    Circular,
    Continuous,
}

/// The curve every piece of a tile of `size` rounds `corner` on: continuous, the window's own,
/// while its reach fits in half the shorter side; circular past that, where Core Animation's
/// continuous corner is a shape no path here matches. See "The corner, measured" in
/// `src/decorations/docs/README.md`.
pub fn curve(size: TileSize, corner: f64) -> Curve {
    if corner * CONTINUOUS_REACH <= size.width.min(size.height) / 2.0 {
        Curve::Continuous
    } else {
        Curve::Circular
    }
}

/// One step of a path in a tile's points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Segment {
    Move(Point<Tile>),
    Line(Point<Tile>),
    Cubic(Point<Tile>, Point<Tile>, Point<Tile>),
    Close,
}

/// A continuous corner in radii from its corner point, along the side coming in and the side going
/// out: where it leaves the side, then three cubics as (control, control, end). UIKit's continuous
/// rounded rect; within half an alpha step of Core Animation's at 2x.
const CONTINUOUS: [(f64, f64); 10] = [
    (CONTINUOUS_REACH, 0.0),
    (1.08849323, 0.0),
    (0.86840689, 0.0),
    (0.63149399, 0.07491100),
    (0.37282392, 0.18603501),
    (0.18603501, 0.37282392),
    (0.07491100, 0.63149399),
    (0.0, 0.86840689),
    (0.0, 1.08849323),
    (0.0, CONTINUOUS_REACH),
];

/// A quarter circle in the same ten points: the cubics before and after it are empty, so a
/// circular path has the continuous one's segments and the two interpolate.
const KAPPA: f64 = 0.552_284_749_830_793_4;
const CIRCULAR: [(f64, f64); 10] = [
    (1.0, 0.0),
    (1.0, 0.0),
    (1.0, 0.0),
    (1.0, 0.0),
    (1.0 - KAPPA, 0.0),
    (0.0, 1.0 - KAPPA),
    (0.0, 1.0),
    (0.0, 1.0),
    (0.0, 1.0),
    (0.0, 1.0),
];

/// `rect` with corners of `corner` on `curve`, clockwise from its top side's start: per corner a
/// line onto it and three cubics.
pub fn rounded_rect(rect: TileRect, corner: f64, curve: Curve) -> Vec<Segment> {
    let points = match curve {
        Curve::Circular => &CIRCULAR,
        Curve::Continuous => &CONTINUOUS,
    };
    let reach = points[0].0;
    let half = rect.size.width.min(rect.size.height).max(0.0) / 2.0;
    let r = corner.max(0.0).min(half / reach);
    let (x0, y0) = (rect.origin.x, rect.origin.y);
    let (x1, y1) = (x0 + rect.size.width, y0 + rect.size.height);
    // Each corner: its point, the way back along the side coming in, the way along the side going out.
    let corners = [
        ((x1, y0), (-1.0, 0.0), (0.0, 1.0)),
        ((x1, y1), (0.0, -1.0), (-1.0, 0.0)),
        ((x0, y1), (1.0, 0.0), (0.0, -1.0)),
        ((x0, y0), (0.0, 1.0), (1.0, 0.0)),
    ];
    let mut path = vec![Segment::Move(Point::new(x0 + reach * r, y0))];
    for ((cx, cy), (ux, uy), (vx, vy)) in corners {
        let at = |(a, b): (f64, f64)| Point::new(cx + (a * ux + b * vx) * r, cy + (a * uy + b * vy) * r);
        path.push(Segment::Line(at(points[0])));
        for i in [1, 4, 7] {
            path.push(Segment::Cubic(at(points[i]), at(points[i + 1]), at(points[i + 2])));
        }
    }
    path.push(Segment::Close);
    path
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
            for reported in [None, Some(26.0)] {
                let (rect, corner) = outline(size, radius(reported, size));
                assert!(2.0 * corner <= rect.size.width, "{w}x{h} {reported:?}");
                assert!(2.0 * corner <= rect.size.height, "{w}x{h} {reported:?}");
            }
        }
    }

    #[test]
    fn a_reported_radius_is_the_windows_own() {
        assert_eq!(radius(Some(26.0), TileSize::new(859.0, 1081.0)), 26.0);
        assert_eq!(radius(Some(4.5), TileSize::new(859.0, 1081.0)), 4.5);
    }

    #[test]
    fn a_window_the_server_reports_no_radius_for_rounds_at_16pt() {
        assert_eq!(radius(None, TileSize::new(859.0, 1081.0)), 16.0);
        assert_eq!(radius(None, TileSize::new(1720.0, 1081.0)), 16.0);
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
            let size = TileSize::new(width, 1081.0);
            let (floored, corner) = silhouette(size, radius(Some(16.0), size));
            assert_eq!(floored, rect(0.0, 0.0, 0.5, 1081.0), "{width} wide");
            assert_eq!(corner, 0.01, "{width} wide");
        }
        assert_eq!(
            silhouette(TileSize::new(859.0, 1081.0), 0.0),
            (rect(0.0, 0.0, 859.0, 1081.0), 0.01)
        );
    }

    #[test]
    fn the_shadow_masks_hole_is_the_silhouette_one_reach_inside_its_whole() {
        let size = TileSize::new(859.0, 1081.0);
        let (whole, hole, corner) = shadow_mask(size, 16.0);
        assert_eq!(whole, rect(-70.0, -70.0, 999.0, 1221.0));
        assert_eq!((hole, corner), silhouette(size, 16.0));
        assert_eq!(hole.origin, Point::origin());
    }

    #[test]
    fn the_silhouette_and_the_masks_hole_round_to_the_corner_given() {
        let size = TileSize::new(859.0, 1081.0);
        for corner in [10.0, 16.0, 26.0] {
            assert_eq!(silhouette(size, corner).1, corner);
            assert_eq!(shadow_mask(size, corner).2, corner);
        }
    }

    fn ends(path: &[Segment]) -> Vec<Point<Tile>> {
        path.iter()
            .filter_map(|segment| match *segment {
                Segment::Move(to) | Segment::Line(to) | Segment::Cubic(_, _, to) => Some(to),
                Segment::Close => None,
            })
            .collect()
    }

    fn close(a: Point<Tile>, b: Point<Tile>) -> bool { (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6 }

    /// The midpoint of the cubic through a corner's diagonal, from the corner's own point.
    fn diagonal(path: &[Segment], from: Point<Tile>) -> Point<Tile> {
        let (Segment::Cubic(_, _, p0), Segment::Cubic(c1, c2, p3)) = (path[2], path[3]) else {
            panic!("{path:?}")
        };
        let mid = (p0.to_vector() + c1.to_vector() * 3.0 + c2.to_vector() * 3.0 + p3.to_vector()) / 8.0;
        Point::new(from.x - mid.x, mid.y - from.y)
    }

    #[test]
    fn a_window_sized_tile_rounds_on_the_continuous_curve_and_a_narrow_one_on_the_circular() {
        assert_eq!(curve(TileSize::new(860.0, 1079.0), 26.0), Curve::Continuous);
        assert_eq!(curve(TileSize::new(80.0, 400.0), 26.0), Curve::Continuous, "39.7pt reach, 40 half side");
        assert_eq!(curve(TileSize::new(60.0, 400.0), 26.0), Curve::Circular);
        assert_eq!(curve(TileSize::new(400.0, 40.0), 16.0), Curve::Circular);
        assert_eq!(curve(TileSize::new(0.5, 1081.0), 0.25), Curve::Circular);
    }

    /// Core Animation's continuous corner leaves the side 1.528665 radii from the corner, a circular
    /// one at one radius; at the diagonal the continuous one sits 0.2979r in, the circular 0.2929r.
    #[test]
    fn a_continuous_corner_leaves_the_side_at_its_reach_and_crosses_the_diagonal_where_core_animation_does() {
        let rect = TileRect::new(Point::origin(), Size::new(860.0, 1079.0));
        let continuous = rounded_rect(rect, 26.0, Curve::Continuous);
        let circular = rounded_rect(rect, 26.0, Curve::Circular);
        assert!(close(ends(&continuous)[1], Point::new(860.0 - 26.0 * CONTINUOUS_REACH, 0.0)));
        assert!(close(ends(&circular)[1], Point::new(860.0 - 26.0, 0.0)));
        let corner = Point::new(860.0, 0.0);
        let d = diagonal(&continuous, corner);
        assert!((d.x - 0.297_87 * 26.0).abs() < 1e-3 && (d.x - d.y).abs() < 1e-9, "{d:?}");
        let d = diagonal(&circular, corner);
        assert!((d.x - (1.0 - std::f64::consts::FRAC_1_SQRT_2) * 26.0).abs() < 0.01, "{d:?}");
    }

    #[test]
    fn both_curves_have_the_same_segments_so_a_resize_across_them_interpolates() {
        let kinds = |path: Vec<Segment>| -> Vec<u8> {
            path.iter()
                .map(|s| match s {
                    Segment::Move(_) => 0,
                    Segment::Line(_) => 1,
                    Segment::Cubic(..) => 2,
                    Segment::Close => 3,
                })
                .collect()
        };
        let wide = TileRect::new(Point::origin(), Size::new(860.0, 1079.0));
        let narrow = TileRect::new(Point::origin(), Size::new(0.5, 1079.0));
        let reference = kinds(rounded_rect(wide, 26.0, Curve::Continuous));
        assert_eq!(reference.len(), 18);
        for (rect, corner, curve) in [(wide, 26.0, Curve::Circular), (narrow, 0.25, Curve::Circular), (wide, 0.0, Curve::Continuous)] {
            assert_eq!(kinds(rounded_rect(rect, corner, curve)), reference, "{rect:?} {corner} {curve:?}");
        }
    }

    /// Every corner is the first one turned: the path is the same shape at all four, and never
    /// leaves its rect.
    #[test]
    fn the_four_corners_are_one_corner_turned_and_stay_inside_the_rect() {
        let rect = TileRect::new(Point::new(-0.25, -0.25), Size::new(860.5, 1079.5));
        for curve in [Curve::Circular, Curve::Continuous] {
            let path = ends(&rounded_rect(rect, 26.25, curve));
            let corner = |i: usize| &path[1 + i * 4..5 + i * 4];
            let (x1, y1) = (rect.origin.x + rect.size.width, rect.origin.y + rect.size.height);
            let (x0, y0) = (rect.origin.x, rect.origin.y);
            for (k, p) in corner(0).iter().enumerate() {
                let (dx, dy) = (x1 - p.x, p.y - y0);
                assert!(close(corner(1)[k], Point::new(x1 - dy, y1 - dx)), "{curve:?} {k}");
                assert!(close(corner(2)[k], Point::new(x0 + dx, y1 - dy)), "{curve:?} {k}");
                assert!(close(corner(3)[k], Point::new(x0 + dy, y0 + dx)), "{curve:?} {k}");
            }
            assert!(path.iter().all(|p| p.x >= x0 - 1e-9 && p.x <= x1 + 1e-9 && p.y >= y0 - 1e-9 && p.y <= y1 + 1e-9));
        }
    }

    /// A corner too round for its rect is shrunk until its reach fits, so the sides never run
    /// backwards.
    #[test]
    fn a_corner_too_round_for_its_rect_shrinks_until_its_reach_fits() {
        let rect = TileRect::new(Point::origin(), Size::new(20.0, 400.0));
        let path = ends(&rounded_rect(rect, 26.0, Curve::Continuous));
        assert!(close(path[0], Point::new(10.0, 0.0)) && close(path[1], Point::new(10.0, 0.0)), "{path:?}");
    }
}
