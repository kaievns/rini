//! One surface of windows moved as a whole by a travelling viewport: the model behind a strip pan,
//! a workspace switch and a move along the strip, whose swapped columns also cross the surface
//! unless the move holds one on it.
//! Nothing here knows what the surface holds.

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::WindowId;

/// One window's place on a surface, as a strip movement stages it. `frame` is where it lands; the
/// viewport moves, and only a window with a `from` also moves across the surface. What the reactor
/// builds is `crate::transitions::domain::request::SurfaceWindow`.
pub trait SurfaceGeometry {
    fn window(&self) -> WindowId;
    fn frame(&self) -> CGRect;
    /// Held still while the surface moves under it.
    fn pinned(&self) -> bool;
    /// Drawn in the other z-band; see `z_group`.
    fn floating(&self) -> bool;
    /// Where on the surface the window starts, when not at `frame`: a column a move swapped starts
    /// in the slot it held before the move.
    fn from(&self) -> Option<CGRect>;

    /// Where the window's tile starts and ends on screen: `surface_travel` of its frame, starting
    /// from `from` when it has one.
    fn travel(&self, from_offset: CGPoint, to_offset: CGPoint) -> (CGRect, CGRect) {
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
    fn from(&self) -> CGRect;
    fn to(&self) -> CGRect;
    fn floating(&self) -> bool;
    /// For a border window, the window it traces.
    fn companion(&self) -> Option<WindowId>;
}

/// Where each tile of a strip movement starts and ends on screen, in overlay coordinates.
///
/// The surface is fixed; the viewport travels from `from_offset` to `to_offset`, so every
/// unpinned window translates by the opposite of that travel. A pinned window stands still — and a
/// standing tile still has to exist, because the overlay is opaque and anything it omits vanishes.
pub fn surface_travel(
    frame: CGRect,
    from_offset: CGPoint,
    to_offset: CGPoint,
    pinned: bool,
) -> (CGRect, CGRect) {
    if pinned {
        return (frame, frame);
    }
    let at = |offset: CGPoint| {
        CGRect::new(
            CGPoint::new(frame.origin.x - offset.x, frame.origin.y - offset.y),
            frame.size,
        )
    };
    (at(from_offset), at(to_offset))
}

/// How far a strip movement carries every unpinned tile: `surface_travel`'s `to - from`.
pub fn pan_travel(from_offset: CGPoint, to_offset: CGPoint) -> CGPoint {
    CGPoint::new(from_offset.x - to_offset.x, from_offset.y - to_offset.y)
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;
    use crate::animation::domain::motion::testing::{DISPLAY, Gen, OnSurface, RUNS};

    fn window(frame: CGRect, from: Option<CGRect>, pinned: bool) -> OnSurface {
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
        let frame = CGRect::new(CGPoint::new(864.0, 32.0), CGSize::new(861.0, 1081.0));
        let old = CGRect::new(CGPoint::new(1728.0, 32.0), frame.size);
        let (from_offset, to_offset) = (CGPoint::new(-864.0, 0.0), CGPoint::new(0.0, 0.0));
        let (start, end) = window(frame, Some(old), false).travel(from_offset, to_offset);
        assert_eq!(start.origin, CGPoint::new(2592.0, 32.0));
        assert_eq!(end, frame);
        assert_eq!(
            window(frame, None, false).travel(from_offset, to_offset),
            surface_travel(frame, from_offset, to_offset, false)
        );
        assert_eq!(
            window(frame, Some(old), true).travel(from_offset, to_offset),
            (frame, frame),
            "a pinned window stands still"
        );
    }

    #[test]
    fn a_pan_translates_a_parked_window_without_remapping_it() {
        let mut rng = Gen(33);
        for _ in 0..RUNS {
            let size = CGSize::new(rng.pt(400.0, 1720.0), 1081.0);
            let frame = rng.park(size);
            let from_offset = CGPoint::new(rng.pt(-4000.0, 4000.0), 0.0);
            let to_offset = CGPoint::new(rng.pt(-4000.0, 4000.0), 0.0);
            let (from, to) = surface_travel(frame, from_offset, to_offset, false);
            assert_eq!(from.origin.x, frame.origin.x - from_offset.x);
            assert_eq!(to.origin.x, frame.origin.x - to_offset.x);
            assert_eq!(from.size, frame.size);
            assert_eq!(to.origin.x - from.origin.x, from_offset.x - to_offset.x);
            assert_eq!(from.origin.y, frame.origin.y, "a pan keeps the park's row");
            let entry = rini_geometry::park_entry_frame(frame, to, DISPLAY);
            if from_offset.x.abs() != 1.0 {
                assert_ne!(from, entry, "the pan path does not consult the park remap");
            }
        }
    }
}
