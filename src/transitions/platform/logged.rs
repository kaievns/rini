//! How the flight engine logs geometry.

use std::fmt;

use rini_geometry::space::{Point, Rect, Size, Vector};

/// A frame, point, vector or size as the log gives it, to the point: a frame as its origin, then
/// its size.
pub(super) struct Whole<T>(pub(super) T);

impl<U> fmt::Display for Whole<Rect<U>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", Whole(self.0.origin), Whole(self.0.size))
    }
}

impl<U> fmt::Display for Whole<Point<U>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.0},{:.0}", self.0.x, self.0.y)
    }
}

impl<U> fmt::Display for Whole<Vector<U>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { Whole(self.0.to_point()).fmt(f) }
}

impl<U> fmt::Display for Whole<Size<U>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.0}x{:.0}", self.0.width, self.0.height)
    }
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{OverlayPoint, OverlayVector, ScreenRect, ScreenSize};

    use super::*;
    use crate::animation::domain::motion::testing::rect;

    #[test]
    fn a_frame_is_logged_as_its_origin_then_its_size_and_a_point_vector_or_size_alone_to_the_point()
    {
        let frame: ScreenRect = rect(10.4, -3.6, 859.0, 1080.6);
        assert_eq!(Whole(frame).to_string(), "10,-4 859x1081");
        assert_eq!(Whole(OverlayPoint::new(-0.4, 862.6)).to_string(), "-0,863");
        assert_eq!(Whole(OverlayVector::new(24.6, 0.0)).to_string(), "25,0");
        assert_eq!(Whole(ScreenSize::new(1727.7, 40.2)).to_string(), "1728x40");
    }
}
