//! One fixed surface of windows moved as a whole by a travelling viewport: the model behind a
//! strip pan or a workspace switch. Nothing here knows what the surface holds.

use objc2_core_foundation::{CGPoint, CGRect};
use rini_windows::ids::{WindowId, WindowServerId};

/// One window's fixed place on a surface. `frame` is never interpolated; the viewport moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceWindow {
    pub window: WindowId,
    pub server_id: WindowServerId,
    pub frame: CGRect,
    /// Held still while the surface moves under it.
    pub pinned: bool,
    /// Drawn in the other z-band; see `z_group`.
    pub floating: bool,
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
