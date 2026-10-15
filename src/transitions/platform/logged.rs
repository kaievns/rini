//! What the flight engine logs: geometry to the point, and what a merge changed.

use std::fmt;

use rini_geometry::space::{OverlayPoint, Point, Rect, Size, Vector};
use tracing::debug;

use crate::animation::domain::motion::plan::{FlightPlan, PlanDelta};
use crate::animation::domain::motion::score::{Node, Score, Value};

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

/// What a merge at `at` on the media clock changed of the flight, now `plan` and playing `score`:
/// the containers it retargeted, from where each is drawn, and the members it moved between them.
pub(super) fn merged(plan: &FlightPlan, score: &Score, delta: &PlanDelta, at: f64) {
    debug!(
        groups = plan.groups.iter().filter(|g| !g.members.is_empty()).count(),
        changing = plan.changing.len(),
        entrances = plan.entrances.len(),
        reparented = delta.reparented.len(),
        retargeted_groups = delta.retargeted_groups.len(),
        joined = delta.joined_tiles.len(),
        "flight merged"
    );
    for (key, to) in &delta.retargeted_groups {
        let drawn = match score.sample(Node::Container(*key), at) {
            Some(Value::Point(drawn)) => drawn,
            _ => OverlayPoint::origin(),
        };
        debug!(
            key = format!("{key:?}"),
            presented = %Whole(drawn),
            to = %Whole(*to),
            "group retargeted"
        );
    }
    for (window, from_key, to_key) in &delta.reparented {
        let dest = plan.destination(*window).map(Whole);
        let from_to = plan.position_of(*from_key);
        debug!(
            pid = window.pid,
            idx = window.idx.get(),
            from = format!("{from_key:?}"),
            to = format!("{to_key:?}"),
            group_destination = %Whole(from_to),
            member_destination = dest.map(|dest| dest.to_string()).unwrap_or_default(),
            "member reparented"
        );
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
