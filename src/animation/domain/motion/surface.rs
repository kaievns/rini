//! One surface of windows moved as a whole by a travelling viewport: the model behind a strip pan,
//! a workspace switch and a move along the strip, whose swapped columns also cross the surface
//! unless the move holds one on it.
//! Nothing here knows what the surface holds.

use rini_core::ids::WindowId;
use rini_geometry::space::{
    Overlay, OverlayRect, OverlayVector, Respace, Surface, SurfacePoint, SurfaceRect, Translation,
};

/// One window's place on a surface, as a strip movement stages it. `frame` is where it lands; the
/// viewport moves, and only a window with a `from` also moves across the surface. What the reactor
/// builds is `crate::transitions::domain::request::SurfaceWindow`.
pub trait SurfaceGeometry {
    fn window(&self) -> WindowId;
    fn frame(&self) -> SurfaceRect;
    /// Held still while the surface moves under it.
    fn pinned(&self) -> bool;
    /// Drawn in the other z-band; see `z_group`.
    fn floating(&self) -> bool;
    /// Where on the surface the window starts, when not at `frame`: a column a move swapped starts
    /// in the slot it held before the move, and a window that was away from where the travel draws
    /// it, where it was.
    fn from(&self) -> Option<SurfaceRect>;

    /// Where the window's tile starts and ends on screen: `surface_travel` of its frame, starting
    /// from `from` when it has one.
    fn travel(
        &self,
        from_offset: SurfacePoint,
        to_offset: SurfacePoint,
    ) -> (OverlayRect, OverlayRect) {
        let (start, end) = surface_travel(self.frame(), from_offset, to_offset, self.pinned());
        match self.from() {
            Some(from) if !self.pinned() => {
                (surface_travel(from, from_offset, to_offset, false).0, end)
            }
            _ => (start, end),
        }
    }
}

/// Every window not carried by the surface is drawn where it stands; the caller decides what
/// "the window" is, so this is a description and not a window.
pub trait TileGeometry {
    fn window(&self) -> WindowId;
    fn from(&self) -> OverlayRect;
    fn to(&self) -> OverlayRect;
    fn floating(&self) -> bool;
    /// For a border window, the window it traces.
    fn companion(&self) -> Option<WindowId>;
}

/// What the viewport at `offset` shows of the surface: the surface less `offset`.
pub fn viewport(offset: SurfacePoint) -> Translation<Surface, Overlay> {
    Translation::new(-offset.x, -offset.y)
}

/// Where each tile of a strip movement starts and ends on screen.
///
/// The surface is fixed; the viewport travels from `from_offset` to `to_offset`, so every
/// unpinned window translates by the opposite of that travel. A pinned window stands still where
/// the final viewport shows it — and a standing tile still has to exist, because the overlay is
/// opaque and anything it omits vanishes.
pub fn surface_travel(
    frame: SurfaceRect,
    from_offset: SurfacePoint,
    to_offset: SurfacePoint,
    pinned: bool,
) -> (OverlayRect, OverlayRect) {
    let end = viewport(to_offset).transform_rect(&frame);
    if pinned {
        return (end, end);
    }
    (viewport(from_offset).transform_rect(&frame), end)
}

/// How far a strip movement carries every unpinned tile: `surface_travel`'s `to - from`.
pub fn pan_travel(from_offset: SurfacePoint, to_offset: SurfacePoint) -> OverlayVector {
    (from_offset - to_offset).respace()
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{OverlayPoint, Size};

    use super::*;
    use crate::animation::domain::motion::testing::{Gen, OnSurface, RUNS, VIEWPORT, rect};

    fn window(frame: SurfaceRect, from: Option<SurfaceRect>, pinned: bool) -> OnSurface {
        OnSurface {
            window: WindowId::new(1, 1),
            frame,
            pinned,
            floating: pinned,
            from,
        }
    }

    /// A swapped column starts from its old slot and lands at its new one, both seen through the
    /// travelling viewport; without a start of its own it is a plain pan.
    #[test]
    fn a_window_with_a_start_of_its_own_travels_from_it() {
        let frame: SurfaceRect = rect(864.0, 32.0, 861.0, 1081.0);
        let old = SurfaceRect::new(SurfacePoint::new(1728.0, 32.0), frame.size);
        let (from_offset, to_offset) =
            (SurfacePoint::new(-864.0, 0.0), SurfacePoint::new(0.0, 0.0));
        let (start, end) = window(frame, Some(old), false).travel(from_offset, to_offset);
        let at_rest = viewport(to_offset).transform_rect(&frame);
        assert_eq!(start.origin, OverlayPoint::new(2592.0, 32.0));
        assert_eq!(end, at_rest);
        assert_eq!(
            window(frame, None, false).travel(from_offset, to_offset),
            surface_travel(frame, from_offset, to_offset, false)
        );
        assert_eq!(
            window(frame, Some(old), true).travel(from_offset, to_offset),
            (at_rest, at_rest),
            "a pinned window stands still"
        );
    }

    /// The rigidity property the old canvas layer guaranteed structurally: every unpinned window
    /// of a strip movement translates by exactly the same vector, so tiles animated per-tile from
    /// these rects cannot drift apart. If this ever fails, the strip tears.
    #[test]
    fn a_strip_movement_translates_every_window_by_the_same_vector() {
        let from_offset = SurfacePoint::new(0.0, 0.0);
        let to_offset = SurfacePoint::new(-861.0, 1117.0);
        let frames: [SurfaceRect; 3] = [
            rect(4.0, 32.0, 859.0, 1081.0),
            rect(867.0, 32.0, 859.0, 1081.0),
            rect(4.0, 1149.0, 1720.0, 1081.0), // the row below, mid-jump
        ];
        for frame in frames {
            let (from, to) = surface_travel(frame, from_offset, to_offset, false);
            assert_eq!(
                (from.origin.x, from.origin.y),
                (frame.origin.x, frame.origin.y),
                "at rest the viewport offset is zero"
            );
            assert_eq!(to.origin.x - from.origin.x, 861.0);
            assert_eq!(to.origin.y - from.origin.y, -1117.0);
            assert_eq!(to.size, frame.size.respace(), "a strip movement never resizes");
        }
    }

    /// A floating window does not belong to the strip: a pan carries it with the viewport and it
    /// stands where the viewport ends, and a standing tile still exists, because the overlay is
    /// opaque and omissions vanish.
    #[test]
    fn a_pinned_window_stands_where_the_viewport_ends() {
        let frame: SurfaceRect = rect(224.0, 95.0, 1280.0, 960.0);
        let (from, to) = surface_travel(
            frame,
            SurfacePoint::new(100.0, 0.0),
            SurfacePoint::new(-4000.0, 0.0),
            true,
        );
        assert_eq!(from, rect(4224.0, 95.0, 1280.0, 960.0));
        assert_eq!(to, from);
        let home =
            surface_travel(frame, SurfacePoint::new(100.0, 0.0), SurfacePoint::origin(), true);
        assert_eq!(home, (rect(224.0, 95.0, 1280.0, 960.0), rect(224.0, 95.0, 1280.0, 960.0)));
    }

    #[test]
    fn a_pan_translates_a_parked_window_without_remapping_it() {
        let mut rng = Gen(33);
        for _ in 0..RUNS {
            let size = Size::new(rng.pt(400.0, 1720.0), 1081.0);
            let frame: SurfaceRect = rng.park(size);
            let from_offset = SurfacePoint::new(rng.pt(-4000.0, 4000.0), 0.0);
            let to_offset = SurfacePoint::new(rng.pt(-4000.0, 4000.0), 0.0);
            let (from, to) = surface_travel(frame, from_offset, to_offset, false);
            assert_eq!(from.origin.x, frame.origin.x - from_offset.x);
            assert_eq!(to.origin.x, frame.origin.x - to_offset.x);
            assert_eq!(from.size, frame.size.respace());
            assert_eq!(to.origin.x - from.origin.x, from_offset.x - to_offset.x);
            assert_eq!(from.origin.y, frame.origin.y, "a pan keeps the park's row");
            let park = viewport(SurfacePoint::origin()).transform_rect(&frame);
            let entry = rini_geometry::park_entry_frame(park, to, VIEWPORT);
            if from_offset.x.abs() != 1.0 {
                assert_ne!(from, entry, "the pan path does not consult the park remap");
            }
        }
    }
}
