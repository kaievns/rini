//! One surface of windows moved as a whole by a travelling viewport: the model behind a strip pan,
//! a workspace switch and a move along the strip, whose two swapped columns also cross the surface.
//! Nothing here knows what the surface holds.

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{WindowId, WindowServerId};

/// One window's place on a surface. `frame` is where it lands; the viewport moves, and only a window
/// with a `from` also moves across the surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceWindow {
    pub window: WindowId,
    pub server_id: WindowServerId,
    pub frame: CGRect,
    /// Held still while the surface moves under it.
    pub pinned: bool,
    /// Drawn in the other z-band; see `z_group`.
    pub floating: bool,
    /// Where on the surface the window starts, when not at `frame`: a column a move swapped starts
    /// in the slot it held before the move.
    pub from: Option<CGRect>,
}

impl SurfaceWindow {
    /// Where the window's tile starts and ends on screen: `surface_travel` of its frame, starting
    /// from `from` when it has one.
    pub fn travel(&self, from_offset: CGPoint, to_offset: CGPoint) -> (CGRect, CGRect) {
        let (start, end) = surface_travel(self.frame, from_offset, to_offset, self.pinned);
        match self.from {
            Some(from) if !self.pinned => {
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
    fn companion(&self) -> bool;
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

/// A snapshot's pixels as packed RGB, downsampled by four in each axis, for the debug dump.
/// Converts a display-space rect into the overlay's own coordinate space.
///
/// The overlay's layer tree has its origin at the overlay's top-left, not the display's, so a window
/// frame has to have the overlay's origin subtracted. Skipping this puts every tile off by the menu
/// bar inset, which reads as the whole animation being shifted down.
pub fn to_overlay_space(frame: CGRect, overlay_frame: CGRect) -> CGRect {
    CGRect::new(
        CGPoint::new(
            frame.origin.x - overlay_frame.origin.x,
            frame.origin.y - overlay_frame.origin.y,
        ),
        frame.size,
    )
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    fn window(frame: CGRect, from: Option<CGRect>, pinned: bool) -> SurfaceWindow {
        SurfaceWindow {
            window: WindowId::new(1, 1),
            server_id: WindowServerId::new(1),
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
}
