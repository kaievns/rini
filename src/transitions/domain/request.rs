//! What the flight engine is asked for.
//!
//! The port into `crate::transitions::platform`: the reactor builds these, `domain::pass` and
//! `domain::strip_move` stage a transition in these terms, and the engine carries them out.

use std::time::Instant;

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{ScreenRect, SurfaceRect};

use crate::animation::domain::motion::surface::SurfaceGeometry;
use crate::decorations::domain::style::{Band, Decoration, Worn};
use crate::overview::domain::scene::Scene;

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
    /// The stack index of the other workspace the window belongs to, for a window of a strip a
    /// look draws beside the one the flight moves; `None` for the strip the flight moves.
    pub row: Option<usize>,
}

impl SurfaceGeometry for SurfaceWindow {
    fn window(&self) -> WindowId { self.window }

    fn frame(&self) -> SurfaceRect { self.frame }

    fn pinned(&self) -> bool { self.pinned }

    fn floating(&self) -> bool { self.floating }

    fn from(&self) -> Option<SurfaceRect> { self.from }

    fn row(&self) -> Option<usize> { self.row }
}

/// The overview as the reactor laid it out: the scene, what each of its thumbs is drawn from, and
/// where the display has the windows of the workspace it shows (`specs/overview.md`).
#[derive(Debug, Clone, PartialEq)]
pub struct OverviewView {
    pub scene: Scene,
    pub windows: Vec<OverviewWindow>,
    /// The shown workspace's strip windows where its layout has them on the display, one it parks
    /// off the display at its place on the strip: what it zooms out from as it opens, and back into
    /// as it closes.
    pub shown: Vec<(WindowId, ScreenRect)>,
    /// The shown workspace's floating windows, which no row draws: on screen with `shown` as it
    /// zooms out and back in, and gone while it is held.
    pub floating: Vec<OverviewFloating>,
    /// What an empty workspace's outline is painted with: the focus ring's band, if one is drawn.
    pub outline: Option<Band>,
}

/// A window of the overview: what its thumb is drawn from beside its picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverviewWindow {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// What it wears; the ring only the selected window's thumb wears.
    pub decoration: Decoration,
}

/// A floating window of the shown workspace: where the display has it and what it wears there, so
/// the overview draws it as the screen has it at its show and at its lift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverviewFloating {
    pub window: WindowId,
    pub server_id: WindowServerId,
    pub frame: ScreenRect,
    pub worn: Worn,
}
