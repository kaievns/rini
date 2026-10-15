//! The holes the overlay leaves over windows that end on screen. A window wholly covered by the
//! overlay is not painted by the window server, so an app that repaints for focus under it shows
//! the change only after the lift; a 1pt hole over the window keeps it painting, and one over the
//! band rini draws around it at rest keeps that window painting too. See "Holes" in
//! `src/animation/docs/animation-smoothness.md`.
use rini_geometry::space::{OverlayRect, Point, Size};

/// The hole over a window ending at `end`: a 1pt square a point in from its left edge and halfway
/// down, inside the window's own pixels and clear of its corners.
pub fn hole_in(end: OverlayRect) -> OverlayRect {
    OverlayRect::new(
        Point::new(end.origin.x + 1.0, end.origin.y + end.size.height / 2.0),
        Size::new(1.0, 1.0),
    )
}

/// The hole over the band `outset` wide a window ending at `end` wears at rest: a point square, or
/// the band's width where it is narrower, halfway across its left edge and halfway down, outside
/// the window and inside the band, where its tile and the window at rest draw the same pixels.
pub fn band_hole_in(end: OverlayRect, outset: f64) -> OverlayRect {
    let side = outset.min(1.0);
    OverlayRect::new(
        Point::new(
            end.origin.x - (outset + side) / 2.0,
            end.origin.y + end.size.height / 2.0,
        ),
        Size::new(side, 1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> OverlayRect {
        OverlayRect::new(Point::new(x, y), Size::new(w, h))
    }

    fn inside(inner: OverlayRect, outer: OverlayRect) -> bool {
        inner.origin.x >= outer.origin.x
            && inner.origin.y >= outer.origin.y
            && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width
            && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height
    }

    #[test]
    fn the_hole_is_one_point_a_point_in_from_the_left_edge_halfway_down() {
        assert_eq!(
            hole_in(rect(868.0, 32.0, 856.0, 1081.0)),
            rect(869.0, 572.5, 1.0, 1.0)
        );
    }

    #[test]
    fn the_band_hole_is_outside_the_window_and_inside_its_band() {
        let end = rect(868.0, 32.0, 856.0, 1081.0);
        for outset in [4.0, 1.5, 1.0, 0.5] {
            let hole = band_hole_in(end, outset);
            let band = rect(end.origin.x - outset, end.origin.y, outset, end.size.height);
            assert!(inside(hole, band), "{outset}: {hole:?}");
            assert!(
                hole.origin.x + hole.size.width <= end.origin.x,
                "{outset}: outside the window"
            );
        }
        assert_eq!(band_hole_in(end, 4.0), rect(865.5, 572.5, 1.0, 1.0));
    }

    /// Every hole is within its window and clear of the rounded corners, whatever the window's
    /// size, down to one a few points across.
    #[test]
    fn the_hole_is_inside_the_window_and_off_its_corners() {
        for end in [
            rect(868.0, 32.0, 856.0, 1081.0),
            rect(-861.0, 32.0, 861.0, 1081.0),
            rect(400.0, 300.0, 3.0, 40.0),
        ] {
            let hole = hole_in(end);
            assert!(inside(hole, end), "{end:?}: {hole:?}");
            let from_top = hole.origin.y - end.origin.y;
            let from_bottom = end.origin.y + end.size.height - (hole.origin.y + hole.size.height);
            assert!(
                from_top.min(from_bottom) >= end.size.height / 2.0 - 1.0,
                "{end:?}"
            );
        }
    }
}
