//! Rects, points, sizes and displacements that name the space they are in, so one in the wrong space
//! fails to compile. The spaces, where each conversion lives and which of euclid's operations agree
//! with CG's: `docs/README.md`.

use objc2_core_foundation as ic;

use crate::{IsWithin, Round, SameAs};

/// The window server's global space: points, origin at the main display's top-left, y down.
pub enum Screen {}
/// The overlay's root layer: `Screen` less the covered display's origin.
pub enum Overlay {}
/// A plane a viewport travels across: `Overlay` is `Surface` less the viewport.
pub enum Surface {}
/// A container layer's own points: `Overlay` less the container's position.
pub enum Group {}
/// A tile layer's own points, origin at its top-left.
pub enum Tile {}
/// An image's pixels, origin top-left, y down.
pub enum Pixel {}
/// A layer's contents in unit space, y down under rini's flipped views.
pub enum Contents {}

pub type Rect<U> = euclid::Rect<f64, U>;
pub type Point<U> = euclid::Point2D<f64, U>;
pub type Size<U> = euclid::Size2D<f64, U>;
pub type Vector<U> = euclid::Vector2D<f64, U>;

pub type Translation<A, B> = euclid::Translation2D<f64, A, B>;

pub type ScreenRect = Rect<Screen>;
pub type ScreenPoint = Point<Screen>;
pub type ScreenSize = Size<Screen>;
pub type ScreenVector = Vector<Screen>;
pub type OverlayRect = Rect<Overlay>;
pub type OverlayPoint = Point<Overlay>;
pub type OverlayVector = Vector<Overlay>;
pub type SurfaceRect = Rect<Surface>;
pub type SurfacePoint = Point<Surface>;
pub type GroupRect = Rect<Group>;
pub type TileRect = Rect<Tile>;
pub type TileSize = Size<Tile>;
pub type PixelRect = Rect<Pixel>;
pub type PixelSize = Size<Pixel>;
pub type ContentsRect = Rect<Contents>;

/// Points apart by a translation. A size or a displacement means the same in each.
pub trait PointSpace {}
impl PointSpace for Screen {}
impl PointSpace for Overlay {}
impl PointSpace for Surface {}
impl PointSpace for Group {}
impl PointSpace for Tile {}

/// A size or a displacement carried into another point space, where it means the same.
pub trait Respace {
    type In<B>;
    fn respace<B: PointSpace>(self) -> Self::In<B>;
}

impl<A: PointSpace> Respace for Size<A> {
    type In<B> = Size<B>;
    fn respace<B: PointSpace>(self) -> Size<B> { self.cast_unit() }
}

impl<A: PointSpace> Respace for Vector<A> {
    type In<B> = Vector<B>;
    fn respace<B: PointSpace>(self) -> Vector<B> { self.cast_unit() }
}

/// A CG value entering a space. Not `From`: euclid's types and CG's are both foreign.
pub trait FromCg<C> {
    fn from_cg(cg: C) -> Self;
}

/// A typed value leaving its space for CG.
pub trait ToCg {
    type Cg;
    fn to_cg(self) -> Self::Cg;
}

impl<U> FromCg<ic::CGRect> for Rect<U> {
    fn from_cg(cg: ic::CGRect) -> Self { Rect::new(Point::from_cg(cg.origin), Size::from_cg(cg.size)) }
}

impl<U> FromCg<ic::CGPoint> for Point<U> {
    fn from_cg(cg: ic::CGPoint) -> Self { Point::new(cg.x, cg.y) }
}

impl<U> FromCg<ic::CGSize> for Size<U> {
    fn from_cg(cg: ic::CGSize) -> Self { Size::new(cg.width, cg.height) }
}

impl<U> FromCg<ic::CGPoint> for Vector<U> {
    fn from_cg(cg: ic::CGPoint) -> Self { Vector::new(cg.x, cg.y) }
}

impl<U> ToCg for Rect<U> {
    type Cg = ic::CGRect;
    fn to_cg(self) -> ic::CGRect { ic::CGRect::new(self.origin.to_cg(), self.size.to_cg()) }
}

impl<U> ToCg for Point<U> {
    type Cg = ic::CGPoint;
    fn to_cg(self) -> ic::CGPoint { ic::CGPoint::new(self.x, self.y) }
}

impl<U> ToCg for Size<U> {
    type Cg = ic::CGSize;
    fn to_cg(self) -> ic::CGSize { ic::CGSize::new(self.width, self.height) }
}

impl<U> ToCg for Vector<U> {
    type Cg = ic::CGPoint;
    fn to_cg(self) -> ic::CGPoint { ic::CGPoint::new(self.x, self.y) }
}

impl<U> Round for Rect<U> {
    fn rounded(&self) -> Self {
        let min = self.min().rounded();
        let max = self.max().rounded();
        Rect::new(min, Size::new(max.x - min.x, max.y - min.y))
    }
}

impl<U> Round for Point<U> {
    fn rounded(&self) -> Self { Point::new(self.x.round(), self.y.round()) }
}

impl<U> Round for Size<U> {
    fn rounded(&self) -> Self { Size::new(self.width.round(), self.height.round()) }
}

impl<U> IsWithin for Rect<U> {
    fn is_within(&self, how_much: f64, other: Self) -> bool {
        self.origin.is_within(how_much, other.origin) && self.size.is_within(how_much, other.size)
    }
}

impl<U> IsWithin for Point<U> {
    fn is_within(&self, how_much: f64, other: Self) -> bool {
        self.x.is_within(how_much, other.x) && self.y.is_within(how_much, other.y)
    }
}

impl<U> IsWithin for Size<U> {
    fn is_within(&self, how_much: f64, other: Self) -> bool {
        self.width.is_within(how_much, other.width) && self.height.is_within(how_much, other.height)
    }
}

impl<U> IsWithin for Vector<U> {
    fn is_within(&self, how_much: f64, other: Self) -> bool {
        self.x.is_within(how_much, other.x) && self.y.is_within(how_much, other.y)
    }
}

impl<U> SameAs for Rect<U> {}
impl<U> SameAs for Point<U> {}
impl<U> SameAs for Size<U> {}
impl<U> SameAs for Vector<U> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CGRectExt;

    fn cg(x: f64, y: f64, w: f64, h: f64) -> ic::CGRect {
        ic::CGRect::new(ic::CGPoint::new(x, y), ic::CGSize::new(w, h))
    }

    fn typed(x: f64, y: f64, w: f64, h: f64) -> ScreenRect {
        Rect::new(Point::new(x, y), Size::new(w, h))
    }

    fn bits(r: ic::CGRect) -> [u64; 4] {
        [r.origin.x, r.origin.y, r.size.width, r.size.height].map(f64::to_bits)
    }

    /// A small seeded generator, so the property runs the same rects every time.
    struct Seeded(u64);

    impl Seeded {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }

        /// Whole, half and quarter points across two displays, the values frames carry, and a few
        /// that are none of those.
        fn coordinate(&mut self, span: f64) -> f64 {
            let raw = (self.next() * 2.0 - 1.0) * span;
            let value = if self.next() < 0.8 { (raw * 4.0).round() / 4.0 } else { raw };
            // No -0.0: `f64::min` and euclid's comparison pick different zeros of the two.
            value + 0.0
        }

        fn rect(&mut self) -> ic::CGRect {
            cg(
                self.coordinate(3500.0),
                self.coordinate(1700.0),
                self.coordinate(1800.0).abs() + 0.25,
                self.coordinate(1200.0).abs() + 0.25,
            )
        }
    }

    #[test]
    fn cg_values_cross_into_a_space_and_back_bit_for_bit() {
        let frame = cg(-860.5, 32.25, 859.0, 1081.125);
        let typed = ScreenRect::from_cg(frame);
        assert_eq!((typed.origin.x, typed.origin.y), (-860.5, 32.25));
        assert_eq!((typed.size.width, typed.size.height), (859.0, 1081.125));
        assert_eq!(bits(typed.to_cg()), bits(frame));

        let negative_zero = cg(-0.0, -1e-300, 0.1, 3.0e8);
        assert_eq!(bits(Rect::<Overlay>::from_cg(negative_zero).to_cg()), bits(negative_zero));

        let point = ic::CGPoint::new(-1680.75, 0.5);
        assert_eq!(ScreenPoint::from_cg(point).to_cg(), point);
        let size = ic::CGSize::new(1728.0, 1116.5);
        assert_eq!(ScreenSize::from_cg(size).to_cg(), size);
        let displacement = ic::CGPoint::new(-4000.0, 0.25);
        let vector = Vector::<Overlay>::from_cg(displacement);
        assert_eq!((vector.x, vector.y), (-4000.0, 0.25));
        assert_eq!(vector.to_cg(), displacement);
    }

    #[test]
    fn a_size_or_a_displacement_is_the_same_in_every_point_space() {
        let size = ScreenSize::new(859.0, 1081.5);
        let in_tile: Size<Tile> = size.respace();
        assert_eq!((in_tile.width, in_tile.height), (859.0, 1081.5));
        let back: ScreenSize = in_tile.respace::<Group>().respace::<Surface>().respace();
        assert_eq!(back, size);

        let shift = Vector::<Surface>::new(-1728.0, 0.5);
        let in_overlay: Vector<Overlay> = shift.respace();
        assert_eq!((in_overlay.x, in_overlay.y), (-1728.0, 0.5));
        let tile = Rect::<Overlay>::new(Point::new(4.0, 32.0), Size::new(859.0, 1081.0));
        assert_eq!(tile.translate(in_overlay).origin, Point::new(-1724.0, 32.5));
    }

    /// Why `rounded` exists: euclid's `round` is `(x + 0.5).floor()`, which takes -860.5 to -860.
    #[test]
    fn rounded_takes_halves_away_from_zero() {
        let rounded = typed(-860.5, 31.5, 859.0, 1081.0).rounded();
        assert_eq!((rounded.origin.x, rounded.origin.y), (-861.0, 32.0));
        assert_eq!((rounded.size.width, rounded.size.height), (859.0, 1081.0), "both edges snap");
        assert_eq!(cg(-860.5, 31.5, 859.0, 1081.0).rounded(), rounded.to_cg());
        assert_eq!(ScreenPoint::new(-0.5, 0.5).rounded(), Point::new(-1.0, 1.0));
        assert_eq!(ScreenSize::new(858.5, 1080.5).rounded(), Size::new(859.0, 1081.0));
    }

    #[test]
    fn euclid_agrees_with_the_cg_ops_on_what_the_features_ask() {
        let mut seeded = Seeded(0x5eed);
        for _ in 0..20_000 {
            let (a, b) = (seeded.rect(), seeded.rect());
            let (ta, tb) = (ScreenRect::from_cg(a), ScreenRect::from_cg(b));

            let cut = ta.intersection(&tb);
            assert_eq!(cut.is_some(), a.intersection(&b).area() > 0.0, "{a:?} and {b:?}");
            if let Some(cut) = cut {
                assert_eq!(bits(cut.to_cg()), bits(a.intersection(&b)), "{a:?} and {b:?}");
            }
            assert_eq!(bits(ta.union(&tb).to_cg()), bits(a.union(&b)), "{a:?} and {b:?}");
            assert_eq!(ta.center().to_cg(), a.mid(), "{a:?}");

            let by = Vector::new(seeded.coordinate(4000.0), seeded.coordinate(1200.0));
            let moved = cg(a.origin.x + by.x, a.origin.y + by.y, a.size.width, a.size.height);
            assert_eq!(bits(ta.translate(by).to_cg()), bits(moved), "{a:?} by {by:?}");
        }
    }

    #[test]
    fn euclid_differs_from_the_cg_ops_at_the_edges() {
        let (window, typed_window) = (cg(0.0, 0.0, 10.0, 10.0), typed(0.0, 0.0, 10.0, 10.0));
        assert!(window.contains(ic::CGPoint::new(10.0, 10.0)), "CG: inclusive at the max edge");
        assert!(!typed_window.contains(Point::new(10.0, 10.0)), "euclid: exclusive");

        let empty = cg(100.0, 100.0, 0.0, 0.0);
        assert_eq!(window.union(&empty), cg(0.0, 0.0, 100.0, 100.0), "CG spans an empty rect");
        assert_eq!(typed_window.union(&ScreenRect::from_cg(empty)), typed_window, "euclid skips it");

        let sliver = cg(5.0, 2.0, 0.0, 4.0);
        assert_eq!(window.intersection(&sliver).area(), 0.0);
        assert!(
            typed_window.intersects(&ScreenRect::from_cg(sliver)),
            "a zero-width rect inside another intersects it"
        );
        assert!(typed_window.intersection(&ScreenRect::from_cg(sliver)).is_none());

        let (from, to, t) = (864.0, -861.0, 0.3);
        assert_eq!(from + (to - from) * t, 346.5, "rini's lerp");
        let euclid = ScreenPoint::new(from, 0.0).lerp(Point::new(to, 0.0), t);
        assert_eq!(euclid.x, 346.49999999999994, "euclid's: (1 - t)a + tb");
    }
}
