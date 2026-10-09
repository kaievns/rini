//! What the flight engine is asked for.
//!
//! The port into `crate::transitions::platform`: the reactor builds these, `domain::pass` and
//! `domain::strip_move` stage a transition in these terms, and the engine carries them out.

use std::time::Instant;

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{ScreenRect, SurfaceRect};

use crate::animation::domain::motion::surface::SurfaceGeometry;
use crate::decorations::domain::style::Worn;

/// When the reactor took the event a request answers, and what it was: where the request's journal
/// starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cause {
    pub at: Instant,
    pub what: &'static str,
}

/// One window of a layout pass.
#[derive(Debug, Clone)]
pub struct AnimationRequest {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// Frame the window is leaving.
    pub from: ScreenRect,
    /// Frame the window is arriving at.
    pub to: ScreenRect,
    /// Off the strip, and so in the other z-order group.
    pub floating: bool,
    /// What the window wears, carried to its tile unread.
    pub worn: Worn,
    /// Where the strip had the window at the pass before, unparked. `None` for a floating window,
    /// one the strip did not hold, or the first pass a workspace gets.
    pub strip_before: Option<ScreenRect>,
    /// Where the strip has it after this pass, unparked. `None` for a floating window or one the
    /// strip does not hold.
    pub strip_after: Option<ScreenRect>,
}

/// One window of a strip movement. `frame` is where it lands; the viewport moves, and only a window
/// with a `from` also moves across the surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceWindow {
    pub window: WindowId,
    pub server_id: WindowServerId,
    pub frame: SurfaceRect,
    /// Held still while the surface moves under it.
    pub pinned: bool,
    /// Drawn in the other z-band; see `z_group`.
    pub floating: bool,
    /// Where on the surface the window starts, when not at `frame`: a column a move swapped starts
    /// in the slot it held before the move, and a window that was away from where the travel draws
    /// it, where it was (`strip_move::start_where_standing`).
    pub from: Option<SurfaceRect>,
    /// What the window wears, carried to its tile unread.
    pub worn: Worn,
}

impl SurfaceGeometry for SurfaceWindow {
    fn window(&self) -> WindowId { self.window }

    fn frame(&self) -> SurfaceRect { self.frame }

    fn pinned(&self) -> bool { self.pinned }

    fn floating(&self) -> bool { self.floating }

    fn from(&self) -> Option<SurfaceRect> { self.from }
}
