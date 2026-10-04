//! What the flight engine is asked for.
//!
//! The port into `crate::transitions::platform`: the reactor builds these, `domain::pass` and
//! `domain::strip_move` stage a transition in these terms, and the engine carries them out.

use objc2_core_foundation::CGRect;
use rini_core::ids::{WindowId, WindowServerId};

use crate::animation::domain::motion::surface::SurfaceGeometry;

/// One window of a layout pass.
#[derive(Debug, Clone)]
pub struct AnimationRequest {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// Frame the window is leaving, in display coordinates.
    pub from: CGRect,
    /// Frame the window is arriving at, in display coordinates.
    pub to: CGRect,
    /// Off the strip, and so in the other z-order group.
    pub floating: bool,
}

/// One window of a strip movement. `frame` is where it lands; the viewport moves, and only a window
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

impl SurfaceGeometry for SurfaceWindow {
    fn window(&self) -> WindowId { self.window }

    fn frame(&self) -> CGRect { self.frame }

    fn pinned(&self) -> bool { self.pinned }

    fn floating(&self) -> bool { self.floating }

    fn from(&self) -> Option<CGRect> { self.from }
}
