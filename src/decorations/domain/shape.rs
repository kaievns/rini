//! Where a window's decorations lie and how round they are, in its tile's own points: the corner
//! radius, the hairline's outline, the shadow's silhouette and the mask it is cut to, and the bands
//! drawn outside the window, the border and the ring; and where the bands at rest are drawn on their
//! display.

use rini_geometry::space::{Point, ScreenRect, Size, Tile, TileRect, TileSize};

use crate::decorations::domain::style::{Decoration, OUTLINE_PT};

/// The corner radius of a window the window server reports none for, in points: the commonest a
/// standard window reports. See "The corner, measured" in `src/decorations/docs/README.md`.
pub const FALLBACK_RADIUS: f64 = 16.0;

/// Room the mask leaves for the shadow: past where the focused blur (three radii plus the offset,
/// about 54pt) is spent, so the mask never clips it.
pub const SHADOW_REACH: f64 = 70.0;

/// A tile's corner radius: its window's, as the window server reports it, else `FALLBACK_RADIUS`,
/// clamped to half the shorter side, since Core Animation draws a corner past it as given.
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
/// radius, never below zero.
pub fn silhouette(size: TileSize, corner: f64) -> (TileRect, f64) {
    let size = TileSize::new(size.width.max(0.0), size.height.max(0.0));
    (TileRect::new(Point::origin(), size), corner.max(0.0))
}

/// The shadow mask's shape: its whole rect, and the silhouette with corners of `corner` as the hole
/// wound even-odd inside it, with the hole's corner radius.
pub fn shadow_mask(size: TileSize, corner: f64) -> (TileRect, TileRect, f64) {
    let (hole, corner) = silhouette(size, corner);
    (shadow_mask_frame(hole.size), hole, corner)
}

/// A band's width in points as it is drawn on a display of `scale`, as niri rounds one
/// (`niri:src/utils/mod.rs:185-191`): whole device pixels, never below one, and no band at all left
/// at none.
pub fn device_width(width: f64, scale: f64) -> f64 {
    if width <= 0.0 {
        return 0.0;
    }
    (width * scale).max(1.0).round() / scale
}

/// How wide each band a window's decoration has is laid, in points of a display of `scale`, 0 for
/// one it has none of: the border, and the ring, which reaches in under the border to the window's
/// edge, so the two share no seam and the ring shows past the border by its own width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Widths {
    pub border: f64,
    pub ring: f64,
}

pub fn widths(decoration: &Decoration, scale: f64) -> Widths {
    let border = decoration.border.map_or(0.0, |band| device_width(band.width, scale));
    let ring = decoration.ring.map_or(0.0, |band| border + device_width(band.width, scale));
    Widths { border, ring }
}

/// How far a window's decoration reaches outside it on a display of `scale`: past its ring while it
/// wears the ring (`ringed`), else past its border.
pub fn outset(decoration: &Decoration, ringed: bool, scale: f64) -> f64 {
    let Widths { border, ring } = widths(decoration, scale);
    if ringed { ring.max(border) } else { border }
}

/// The band `width` wide outside a window of `size` with corners of `corner`: its outer rect, and
/// that rect's corner. The inner contour is the window's own corner, a border's inner edge being its
/// outer one `width` in, so the outer corner is `corner + width`; a square window keeps a square
/// band, as niri's `expanded_by` keeps 0 at 0; and the corner is clamped to half the band's shorter
/// side, since Core Animation draws one past it as given.
pub fn band(size: TileSize, corner: f64, width: f64) -> (TileRect, f64) {
    let size = TileSize::new(
        size.width.max(0.0) + 2.0 * width,
        size.height.max(0.0) + 2.0 * width,
    );
    let rect = TileRect::new(Point::new(-width, -width), size);
    let outer = if corner > 0.0 { corner + width } else { 0.0 };
    (rect, outer.min(size.width.min(size.height) / 2.0))
}

/// The window a band at rest is drawn in around a window at `frame` on `display`: the band's outer
/// rect cut to the display, so a window at the display's edge draws nothing onto the next one.
/// `None` when nothing of it is on the display.
pub fn rest_frame(frame: ScreenRect, width: f64, display: ScreenRect) -> Option<ScreenRect> {
    frame.inflate(width, width).intersection(&display).filter(|cut| !cut.is_empty())
}

/// How far along each side a continuous corner reaches, in radii: Core Animation's
/// `cornerCurveExpansionFactor` for `kCACornerCurveContinuous`, 1.528665 on macOS 26.7.1.
pub const CONTINUOUS_REACH: f64 = 1.528665;

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
/// rounded rect, the curve Core Animation draws for `kCACornerCurveContinuous`.
const CONTINUOUS: [(f64, f64); 10] = [
    (CONTINUOUS_REACH, 0.0),
    (1.08849323, 0.0),
    (0.86840689, 0.0),
    (0.63149399, 0.07491100),
    (0.37282392, 0.16905899),
    (0.16905899, 0.37282392),
    (0.07491100, 0.63149399),
    (0.0, 0.86840689),
    (0.0, 1.08849323),
    (0.0, CONTINUOUS_REACH),
];

/// `rect` with continuous corners of `corner`, clockwise from its top side's start: per corner a
/// line onto it and three cubics. A corner whose reach passes half a side keeps it, the sides running
/// back over each other, as Core Animation draws it; so every rect has the same segments.
pub fn rounded_rect(rect: TileRect, corner: f64) -> Vec<Segment> {
    let r = corner.max(0.0);
    let (x0, y0) = (rect.origin.x, rect.origin.y);
    let (x1, y1) = (x0 + rect.size.width, y0 + rect.size.height);
    // Each corner: its point, the way back along the side coming in, the way along the side going out.
    let corners = [
        ((x1, y0), (-1.0, 0.0), (0.0, 1.0)),
        ((x1, y1), (0.0, -1.0), (-1.0, 0.0)),
        ((x0, y1), (1.0, 0.0), (0.0, -1.0)),
        ((x0, y0), (0.0, 1.0), (1.0, 0.0)),
    ];
    let mut path = vec![Segment::Move(Point::new(x0 + CONTINUOUS_REACH * r, y0))];
    for ((cx, cy), (ux, uy), (vx, vy)) in corners {
        let at =
            |(a, b): (f64, f64)| Point::new(cx + (a * ux + b * vx) * r, cy + (a * uy + b * vy) * r);
        path.push(Segment::Line(at(CONTINUOUS[0])));
        for i in [1, 4, 7] {
            path.push(Segment::Cubic(
                at(CONTINUOUS[i]),
                at(CONTINUOUS[i + 1]),
                at(CONTINUOUS[i + 2]),
            ));
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

    fn on_screen(x: f64, y: f64, w: f64, h: f64) -> ScreenRect {
        ScreenRect::new(Point::new(x, y), Size::new(w, h))
    }

    #[test]
    fn a_rest_frame_is_cut_to_its_display() {
        let display = on_screen(0.0, 0.0, 1728.0, 1117.0);
        assert_eq!(
            rest_frame(on_screen(3.0, 35.0, 859.0, 1079.0), 4.0, display),
            Some(on_screen(0.0, 31.0, 866.0, 1086.0))
        );
        assert_eq!(
            rest_frame(on_screen(300.0, 300.0, 400.0, 300.0), 4.0, display),
            Some(on_screen(296.0, 296.0, 408.0, 308.0))
        );
        assert_eq!(
            rest_frame(on_screen(1800.0, 35.0, 859.0, 1079.0), 4.0, display),
            None,
            "on the next display"
        );
    }

    #[test]
    fn a_width_rounds_to_whole_device_pixels_and_never_below_one_unless_zero() {
        assert_eq!(device_width(4.0, 2.0), 4.0);
        assert_eq!(device_width(1.3, 2.0), 1.5);
        assert_eq!(device_width(0.2, 2.0), 0.5, "one device pixel at least");
        assert_eq!(device_width(0.2, 1.0), 1.0);
        assert_eq!(device_width(0.0, 2.0), 0.0, "none stays none");
    }

    #[test]
    fn the_ring_band_rounds_r_plus_its_width_and_square_stays_square() {
        assert_eq!(
            band(TileSize::new(859.0, 1081.0), 16.0, 4.0),
            (rect(-4.0, -4.0, 867.0, 1089.0), 20.0)
        );
        assert_eq!(band(TileSize::new(859.0, 1081.0), 0.0, 4.0).1, 0.0);
    }

    #[test]
    fn the_border_band_rounds_r_plus_its_width() {
        assert_eq!(
            band(TileSize::new(859.0, 1081.0), 16.0, 4.0),
            (rect(-4.0, -4.0, 867.0, 1089.0), 20.0)
        );
        assert_eq!(
            band(TileSize::new(859.0, 1081.0), 26.0, 1.5),
            (rect(-1.5, -1.5, 862.0, 1084.0), 27.5)
        );
    }

    #[test]
    fn with_a_border_on_the_ring_reaches_under_it_to_the_window() {
        use crate::decorations::domain::paint::{Colour, Paint};
        use crate::decorations::domain::style::Band;
        let band = |width| {
            Some(Band {
                width,
                active: Paint::Solid(Colour::rgb(0x7fc8ff)),
                inactive: Paint::Solid(Colour::rgb(0x505050)),
                urgent: Paint::Solid(Colour::rgb(0x9b0000)),
            })
        };
        let both = Decoration {
            ring: band(2.0),
            border: band(4.0),
            ..Decoration::default()
        };
        assert_eq!(widths(&both, 2.0), Widths { border: 4.0, ring: 6.0 });
        let (ring, corner) = band_of(widths(&both, 2.0).ring);
        assert_eq!(
            (ring, corner),
            (rect(-6.0, -6.0, 871.0, 1093.0), 22.0),
            "outside the border, its inner edge the window's"
        );
        assert_eq!(outset(&both, true, 2.0), 6.0);
        assert_eq!(outset(&both, false, 2.0), 4.0, "the ring not worn");
        let ring_only = Decoration {
            ring: band(2.0),
            ..Decoration::default()
        };
        assert_eq!(widths(&ring_only, 2.0), Widths { border: 0.0, ring: 2.0 });
        assert_eq!(outset(&ring_only, false, 2.0), 0.0);
        assert_eq!(
            outset(
                &Decoration {
                    border: band(1.3),
                    ..Decoration::default()
                },
                true,
                2.0
            ),
            1.5,
            "whole device pixels"
        );
    }

    fn band_of(width: f64) -> (TileRect, f64) { band(TileSize::new(859.0, 1081.0), 16.0, width) }

    #[test]
    fn the_ring_bands_radius_is_clamped_to_half_its_shorter_side() {
        assert_eq!(
            band(TileSize::new(12.0, 400.0), 6.0, 4.0),
            (rect(-4.0, -4.0, 20.0, 408.0), 10.0)
        );
        assert_eq!(band(TileSize::new(2.0, 400.0), 1.0, 4.0).1, 5.0);
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

    /// Core Animation does not clamp a corner past half a side, so neither path nor hairline may take
    /// one, and an entrance grows from no width at all.
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

    /// An entrance grows from no width; a tile drawn past it casts nothing rather than a shadow on
    /// the other side.
    #[test]
    fn a_silhouette_never_turns_inside_out() {
        for width in [0.0, -3.0] {
            let size = TileSize::new(width, 1081.0);
            assert_eq!(
                silhouette(size, radius(Some(16.0), size)),
                (rect(0.0, 0.0, 0.0, 1081.0), 0.0),
                "{width} wide"
            );
        }
        assert_eq!(
            silhouette(TileSize::new(859.0, 1081.0), -1.0),
            (rect(0.0, 0.0, 859.0, 1081.0), 0.0)
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

    fn close(a: Point<Tile>, b: Point<Tile>) -> bool {
        (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
    }

    /// The midpoint of the cubic through a corner's diagonal, from the corner's own point.
    fn diagonal(path: &[Segment], from: Point<Tile>) -> Point<Tile> {
        let (Segment::Cubic(_, _, p0), Segment::Cubic(c1, c2, p3)) = (path[2], path[3]) else {
            panic!("{path:?}")
        };
        let mid =
            (p0.to_vector() + c1.to_vector() * 3.0 + c2.to_vector() * 3.0 + p3.to_vector()) / 8.0;
        Point::new(from.x - mid.x, mid.y - from.y)
    }

    /// Core Animation's continuous corner leaves the side 1.528665 radii from the corner, where a
    /// circular one leaves it at one, and crosses the diagonal 0.2915r in, against 0.2929r.
    #[test]
    fn a_continuous_corner_leaves_the_side_at_its_reach_and_crosses_the_diagonal_where_core_animation_does()
     {
        let rect = TileRect::new(Point::origin(), Size::new(860.0, 1079.0));
        let continuous = rounded_rect(rect, 26.0);
        assert!(close(
            ends(&continuous)[1],
            Point::new(860.0 - 26.0 * CONTINUOUS_REACH, 0.0)
        ));
        let d = diagonal(&continuous, Point::new(860.0, 0.0));
        assert!(
            (d.x - 0.291_51 * 26.0).abs() < 1e-3 && (d.x - d.y).abs() < 1e-9,
            "{d:?}"
        );
    }

    #[test]
    fn every_rounded_rect_has_the_same_segments_so_any_two_interpolate() {
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
        let none = TileRect::new(Point::origin(), Size::new(0.0, 0.0));
        let reference = kinds(rounded_rect(wide, 26.0));
        assert_eq!(reference.len(), 18);
        for (rect, corner) in [(narrow, 0.25), (wide, 0.0), (none, 0.0)] {
            assert_eq!(kinds(rounded_rect(rect, corner)), reference, "{rect:?} {corner}");
        }
    }

    /// Every corner is the first one turned: the path is the same shape at all four, and never
    /// leaves its rect.
    #[test]
    fn the_four_corners_are_one_corner_turned_and_stay_inside_the_rect() {
        let rect = TileRect::new(Point::new(-0.25, -0.25), Size::new(860.5, 1079.5));
        let path = ends(&rounded_rect(rect, 26.25));
        let corner = |i: usize| &path[1 + i * 4..5 + i * 4];
        let (x1, y1) = (rect.origin.x + rect.size.width, rect.origin.y + rect.size.height);
        let (x0, y0) = (rect.origin.x, rect.origin.y);
        for (k, p) in corner(0).iter().enumerate() {
            let (dx, dy) = (x1 - p.x, p.y - y0);
            assert!(close(corner(1)[k], Point::new(x1 - dy, y1 - dx)), "{k}");
            assert!(close(corner(2)[k], Point::new(x0 + dx, y1 - dy)), "{k}");
            assert!(close(corner(3)[k], Point::new(x0 + dy, y0 + dx)), "{k}");
        }
        assert!(
            path.iter().all(|p| p.x >= x0 - 1e-9
                && p.x <= x1 + 1e-9
                && p.y >= y0 - 1e-9
                && p.y <= y1 + 1e-9)
        );
    }

    /// Core Animation draws a continuous corner whose reach passes half a side at its full reach,
    /// so the path does not shrink it: the top side runs back from 15.29 to 4.71.
    #[test]
    fn a_corner_whose_reach_passes_half_the_side_keeps_it() {
        let rect = TileRect::new(Point::origin(), Size::new(20.0, 400.0));
        let path = ends(&rounded_rect(rect, 10.0));
        let reach = 10.0 * CONTINUOUS_REACH;
        assert!(
            close(path[0], Point::new(reach, 0.0)) && close(path[1], Point::new(20.0 - reach, 0.0)),
            "{path:?}"
        );
    }
}
