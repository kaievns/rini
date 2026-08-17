//! Make vertically-stacked displays feel physically side by side.
//!
//! # Why this exists
//!
//! A scrolling layout parks off-strip columns just past the screen edge. Measured live on
//! this machine, with the display spanning x=0..1728, the strip's column origins were:
//!
//! ```text
//! [-1680, -857, -819, 4, 865, 1688, 1726]
//!  └──── scrolled away / parked ────┘
//! ```
//!
//! Those are not transient animation frames, they are the steady state. With displays
//! arranged SIDE BY SIDE those coordinates land inside the neighbouring display, so
//! scrolled-away windows appear on the wrong screen and focus follows them there. Upstream
//! closed this as not-planned (rift issue #266):
//!
//! > "displays simply have to be arranged vertically. consequence of native multi display
//! > behavior in macOS" — acsandmann
//!
//! So the displays have to be stacked LOGICALLY even when they sit side by side on the
//! desk. The cost of stacking is exactly what this actor pays back: with no display beside
//! either one, moving the pointer left or right hits a wall and stops.
//!
//! # What it does
//!
//! Polls the cursor. When it presses against a vertical edge that has no display beyond it,
//! but a display exists above or below, it warps the cursor to the corresponding edge of
//! that display, preserving the fractional position along the edge so travel feels
//! continuous.
//!
//! ```text
//! physical:  [   4K   ] [ built-in ]      logical:  [   4K   ]
//!                                                   [built-in]
//!
//! push LEFT  off the built-in  ->  appear at the RIGHT edge of the 4K
//! push RIGHT off the 4K        ->  appear at the LEFT  edge of the built-in
//! ```
//!
//! Travel has to continue in the direction the pointer was already moving. Pushing left means
//! carrying on leftwards, so the pointer must arrive at the FAR edge of the display that sits to
//! the left, not at its near edge.
//!
//! Which side the upper display occupies cannot be derived from the coordinate space: macOS knows
//! the displays are stacked and nothing about the desk. It comes from
//! `settings.stacked_display_upper_is`, defaulting to `left`.
//!
//! This was originally implemented the other way round, pairing right-goes-up with left-goes-down,
//! which sent the pointer to the opposite screen AND to the wrong edge of it.
//!
//! # History
//!
//! Ported from `okibi-warp`, a standalone Swift daemon that lived in the config directory
//! with its own LaunchAgent. Bringing it in-tree removes the separate build step and the
//! agent, and more importantly lets it CHECK its own precondition: the daemon assumed the
//! displays were stacked and would have fought a side-by-side arrangement silently. This
//! version verifies the geometry every tick and does nothing when it does not hold.
//!
//! An earlier approach (paneru's `horizontal_mouse_warp`) needed a hand-tuned pixel offset
//! that broke whenever the arrangement changed. Everything here is derived from
//! `CGDisplayBounds` at runtime, so docking and rearranging need no intervention.
//!
//! # Permissions
//!
//! None. `CGWarpMouseCursorPosition` needs no TCC grant, and reading the cursor through a
//! null-source `CGEvent` needs no event tap. Verified: the warp returns success with no
//! prompt.

use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::{CGError, CGEvent};
use tracing::{debug, info};

use crate::common::config::StackedUpperSide;
use crate::sys::geometry::CGRectExt;
use crate::sys::screen::ScreenInfo;

/// How close to an edge counts as pressing against it.
///
/// 2pt, not 1: macOS clamps the cursor to `maxX - 1`, so a 1pt threshold is never
/// reliably observed.
const EDGE_SLOP: f64 = 2.0;

/// How far inside the destination display the cursor lands.
///
/// Must exceed `EDGE_SLOP` by enough that the next poll does not immediately see the
/// cursor against the destination's own edge and warp it straight back.
const ENTRY_INSET: f64 = 12.0;

/// Poll interval. ~120Hz, matching the display refresh. Each tick is one cursor read.
const POLL: Duration = Duration::from_millis(8);

/// Ignore edges for this long after a warp, so one fast swipe cannot bounce between
/// displays repeatedly.
const COOLDOWN: Duration = Duration::from_millis(250);

/// Probe distance used to ask "is there a display beyond this edge".
const BEYOND_PROBE: f64 = 10.0;

#[derive(Debug)]
pub enum Request {
    /// Replace the display geometry. Sent whenever the reactor's screen set changes, so
    /// docking or rearranging does not need a restart.
    ScreensChanged(Vec<WarpScreen>),
    /// Enable or disable warping without tearing the actor down, so a config reload can
    /// toggle it.
    SetEnabled(bool),
    /// Which side the logically-upper display physically sits on.
    SetUpperSide(StackedUpperSide),
    /// Where the lower display's top edge sits on the upper display's height, measured from the
    /// upper display's bottom.
    SetLowerTopAt(f64),
    Stop,
}

pub type Sender = crate::actor::Sender<Request>;
pub type Receiver = crate::actor::Receiver<Request>;

pub struct CursorWarp {
    rx: Receiver,
    enabled: bool,
    upper_side: StackedUpperSide,
    lower_top_at: f64,
    screens: Vec<WarpScreen>,
    last_warp: Option<Instant>,
}

impl CursorWarp {
    pub fn new(
        enabled: bool,
        upper_side: StackedUpperSide,
        lower_top_at: f64,
        rx: Receiver,
    ) -> Self {
        Self {
            rx,
            enabled,
            upper_side,
            lower_top_at,
            screens: Vec::new(),
            last_warp: None,
        }
    }

    pub async fn run(mut self) {
        info!(enabled = self.enabled, "cursor warp actor started");
        loop {
            // Only poll while there is something to do. With warping off, or fewer than
            // two displays, this actor costs nothing but a parked await.
            if self.enabled && self.screens.len() > 1 {
                tokio::select! {
                    request = self.rx.recv() => {
                        match request {
                            Some((_span, request)) => {
                                if !self.handle(request) { return }
                            }
                            None => return,
                        }
                    }
                    _ = crate::sys::executor::sleep(POLL) => self.tick(),
                }
            } else {
                match self.rx.recv().await {
                    Some((_span, request)) => {
                        if !self.handle(request) {
                            return;
                        }
                    }
                    None => return,
                }
            }
        }
    }

    /// Returns false when the actor should stop.
    fn handle(&mut self, request: Request) -> bool {
        match request {
            Request::ScreensChanged(screens) => {
                debug!(count = screens.len(), "cursor warp screens updated");
                self.screens = screens;
            }
            Request::SetEnabled(enabled) => {
                if enabled != self.enabled {
                    info!(enabled, "cursor warp toggled");
                }
                self.enabled = enabled;
            }
            Request::SetLowerTopAt(fraction) => {
                if (fraction - self.lower_top_at).abs() > f64::EPSILON {
                    info!(fraction, "cursor warp vertical alignment changed");
                }
                self.lower_top_at = fraction;
            }
            Request::SetUpperSide(side) => {
                if side != self.upper_side {
                    info!(?side, "cursor warp upper display side changed");
                }
                self.upper_side = side;
            }
            Request::Stop => return false,
        }
        true
    }

    fn tick(&mut self) {
        if let Some(last) = self.last_warp
            && last.elapsed() < COOLDOWN
        {
            return;
        }
        let Some(cursor) = cursor_position() else { return };
        let Some(target) =
            warp_target(&self.screens, cursor, self.upper_side, self.lower_top_at)
        else {
            return;
        };

        // Both calls are FFI into CoreGraphics with plain-value arguments; there are no
        // pointers or lifetimes involved, so the unsafety is purely that they are extern.
        let result = unsafe { crate::sys::skylight::CGWarpMouseCursorPosition(target) };
        if result == CGError::Success {
            // Re-associate the cursor with the mouse. Without this the pointer keeps the
            // velocity it had at the edge and can slide straight back out of the display
            // it just entered.
            unsafe { crate::sys::skylight::CGAssociateMouseAndMouseCursorPosition(1) };
            self.last_warp = Some(Instant::now());
            debug!(?cursor, ?target, "warped cursor across stacked displays");
        } else {
            debug!(?target, ?result, "cursor warp failed");
        }
    }
}

/// Current cursor position in CoreGraphics coordinates, or None if unavailable.
fn cursor_position() -> Option<CGPoint> {
    let event = CGEvent::new(None)?;
    Some(CGEvent::location(Some(&event)))
}

/// Where the cursor should be warped to, or None if this position is not a warp trigger.
///
/// Split out as a free function over plain geometry so the decision can be unit tested
/// without a display or a running actor. Every branch here is a reason NOT to warp, which
/// matters: warping when macOS would have handled the crossing natively means fighting it.
fn warp_target(
    screens: &[WarpScreen],
    cursor: CGPoint,
    upper_side: StackedUpperSide,
    lower_top_at: f64,
) -> Option<CGPoint> {
    if screens.len() < 2 {
        return None;
    }
    let here = display_containing(screens, cursor)?;
    let here_frame = here.frame;

    let at_left = cursor.x <= here_frame.origin.x + EDGE_SLOP;
    let at_right = cursor.x >= here_frame.max().x - EDGE_SLOP;
    if !(at_left || at_right) {
        return None;
    }

    // If a display lies beyond that edge on this row, macOS crosses natively. Interfering
    // would fight it, and the result is a cursor that jitters at the boundary.
    let beyond = CGPoint::new(
        if at_left {
            here_frame.origin.x - BEYOND_PROBE
        } else {
            here_frame.max().x + BEYOND_PROBE
        },
        cursor.y,
    );
    if display_containing(screens, beyond).is_some() {
        return None;
    }

    // Which logical direction continues the pointer's physical travel.
    //
    // With the upper display on the LEFT, moving left carries on into it, so left goes up. With the
    // upper display on the right, moving right carries on into it, so right goes up.
    let going_up = match upper_side {
        StackedUpperSide::Left => at_left,
        StackedUpperSide::Right => at_right,
    };
    let target = vertical_neighbour(screens, here, going_up)?;
    let target_frame = target.frame;

    // The upper display is the one the pointer is travelling toward when going up, and the one it is
    // leaving otherwise.
    let (upper, lower) = if going_up { (target, here) } else { (here, target) };
    let y = mapped_y(here, target, upper, lower, cursor.y, lower_top_at)
        .max(target_frame.origin.y + 1.0)
        .min(target_frame.max().y - 1.0);
    // Arrive at the FAR edge of the destination, so travel continues rather than reversing. Moving
    // left must land near the destination's right edge, and moving right near its left edge.
    let x = if at_left {
        target_frame.max().x - ENTRY_INSET
    } else {
        target_frame.origin.x + ENTRY_INSET
    };
    Some(CGPoint::new(x, y))
}

/// Where on the destination's edge the pointer should arrive, in destination logical coordinates.
///
/// Maps through PHYSICAL position rather than fraction along the edge. With a 391mm external beside a
/// 223mm laptop, the fractional mapping put the external's midpoint at the laptop's midpoint, while
/// physically it lines up near the laptop's top. That mismatch is what made a sweep between them feel
/// like the pointer jumped vertically.
///
/// The two displays are placed on a shared vertical axis in millimetres, with zero at the UPPER
/// display's bottom edge and positive upward. `lower_top_at` says where the lower display's top edge
/// sits on the upper display's height, so the lower display's physical span follows from its own
/// height. A crossing point with no counterpart on the destination clamps to the nearest edge, which
/// is the honest outcome: there is no physically corresponding position to travel to.
///
/// Falls back to proportional mapping when either display does not report a physical size.
fn mapped_y(
    from: WarpScreen,
    to: WarpScreen,
    upper: WarpScreen,
    lower: WarpScreen,
    cursor_y: f64,
    lower_top_at: f64,
) -> f64 {
    let proportional = || {
        let fraction = if from.frame.size.height > 0.0 {
            (cursor_y - from.frame.origin.y) / from.frame.size.height
        } else {
            0.5
        };
        to.frame.origin.y + fraction * to.frame.size.height
    };

    if upper.physical_height_mm <= 0.0
        || lower.physical_height_mm <= 0.0
        || upper.frame.size.height <= 0.0
        || lower.frame.size.height <= 0.0
    {
        return proportional();
    }

    // Physical span of each display on the shared axis: 0 is the upper display's bottom edge.
    let upper_span = (0.0, upper.physical_height_mm);
    let lower_top = lower_top_at * upper.physical_height_mm;
    let lower_span = (lower_top - lower.physical_height_mm, lower_top);

    let (from_span, to_span) = if from == upper {
        (upper_span, lower_span)
    } else {
        (lower_span, upper_span)
    };

    // Logical y grows downward, so the top of a display is its origin and the bottom its max.
    let from_fraction_from_top =
        ((cursor_y - from.frame.origin.y) / from.frame.size.height).clamp(0.0, 1.0);
    let physical = from_span.1 - from_fraction_from_top * (from_span.1 - from_span.0);

    let to_height = to_span.1 - to_span.0;
    if to_height <= 0.0 {
        return proportional();
    }
    let to_fraction_from_top = ((to_span.1 - physical) / to_height).clamp(0.0, 1.0);
    to.frame.origin.y + to_fraction_from_top * to.frame.size.height
}

/// The display containing `point`.
///
/// Outset by half a point: a cursor clamped at `maxX - 1` still belongs to its display,
/// and without the slack a point exactly on a shared edge belongs to neither.
fn display_containing(screens: &[WarpScreen], point: CGPoint) -> Option<WarpScreen> {
    screens.iter().copied().find(|screen| {
        let frame = screen.frame;
        let padded = CGRect::new(
            CGPoint::new(frame.origin.x - 0.5, frame.origin.y - 0.5),
            objc2_core_foundation::CGSize::new(frame.size.width + 1.0, frame.size.height + 1.0),
        );
        padded.contains(point)
    })
}

/// The display directly above or below `frame`, preferring the one with the greatest
/// horizontal overlap — i.e. the visually adjacent one when three displays are stacked.
fn vertical_neighbour(
    screens: &[WarpScreen],
    here: WarpScreen,
    going_up: bool,
) -> Option<WarpScreen> {
    let frame = here.frame;
    screens
        .iter()
        .copied()
        .filter(|other| {
            if other.frame.origin == frame.origin && other.frame.size == frame.size {
                return false;
            }
            // 1pt tolerance: stacked displays usually share an edge exactly, but a menu bar
            // inset or a rounding difference should not disqualify a neighbour.
            if going_up {
                other.frame.max().y <= frame.origin.y + 1.0
            } else {
                other.frame.origin.y >= frame.max().y - 1.0
            }
        })
        .max_by(|a, b| {
            overlap_x(a.frame, frame)
                .partial_cmp(&overlap_x(b.frame, frame))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn overlap_x(a: CGRect, b: CGRect) -> f64 {
    (a.max().x.min(b.max().x) - a.origin.x.max(b.origin.x)).max(0.0)
}

/// A display, with the physical height needed to map a crossing by real-world position.
///
/// Fractional mapping along the edge is wrong whenever the displays differ in size, which is the
/// normal case: on this machine a 391mm external sits beside a 223mm laptop, so the external's
/// midpoint is physically near the laptop's TOP rather than its middle. Preserving the fraction sent
/// the pointer to a visibly different height than it left from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WarpScreen {
    pub frame: CGRect,
    /// Physical height in millimetres, from `CGDisplayScreenSize`. Zero when unknown, in which case
    /// the mapping falls back to proportional.
    pub physical_height_mm: f64,
}

/// Every attached screen with its physical height, for `Request::ScreensChanged`.
pub fn screens_of(screens: &[ScreenInfo]) -> Vec<WarpScreen> {
    screens
        .iter()
        .map(|screen| WarpScreen {
            frame: screen.frame,
            physical_height_mm: physical_height_mm(screen),
        })
        .collect()
}

/// Physical height of a display in millimetres, or 0.0 when the display does not report it.
fn physical_height_mm(screen: &ScreenInfo) -> f64 {
    // SAFETY: plain-value FFI into CoreGraphics with a display id.
    let size = unsafe { crate::sys::skylight::CGDisplayScreenSize(screen.id.as_u32()) };
    if size.height.is_finite() && size.height > 0.0 { size.height } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
    }

    /// The real arrangement, read from `rini-cli query displays` and `CGDisplayScreenSize`:
    /// the 31.6" external logically ABOVE the 16.1" built-in, while physically sitting to its LEFT.
    fn built_in() -> WarpScreen {
        WarpScreen { frame: rect(0.0, 32.0, 1728.0, 1085.0), physical_height_mm: 223.0 }
    }
    fn external() -> WarpScreen {
        WarpScreen { frame: rect(-670.0, -1692.0, 3008.0, 1692.0), physical_height_mm: 391.0 }
    }

    /// The measured arrangement: laptop's top edge 40% up the external's height.
    const LOWER_TOP_AT: f64 = 0.4;

    fn warp(screens: &[WarpScreen], cursor: CGPoint) -> Option<CGPoint> {
        warp_target(screens, cursor, StackedUpperSide::Left, LOWER_TOP_AT)
    }

    /// The bug this was reported for. With the upper display physically on the LEFT, pushing left off
    /// the built-in must continue leftwards onto the external's RIGHT edge. The original code paired
    /// right-with-up and entered at the near edge, so it sent the pointer to the opposite screen and
    /// the opposite side of it.
    #[test]
    fn pushing_left_continues_onto_the_upper_displays_right_edge() {
        let screens = vec![built_in(), external()];
        let cursor = CGPoint::new(built_in().frame.origin.x + 1.0, 500.0);

        let target = warp(&screens, cursor).expect("should warp");
        assert_eq!(
            target.x,
            external().frame.max().x - ENTRY_INSET,
            "must arrive near the far (right) edge so leftward travel continues"
        );
    }

    #[test]
    fn pushing_right_continues_onto_the_lower_displays_left_edge() {
        let screens = vec![built_in(), external()];
        let cursor = CGPoint::new(external().frame.max().x - 1.0, -846.0);

        let target = warp(&screens, cursor).expect("should warp");
        assert_eq!(target.x, built_in().frame.origin.x + ENTRY_INSET);
    }

    #[test]
    fn pushing_away_from_the_upper_display_does_not_warp() {
        let screens = vec![built_in(), external()];
        let cursor = CGPoint::new(built_in().frame.max().x - 1.0, 500.0);
        assert!(warp(&screens, cursor).is_none(), "nothing lies right of the built-in");
    }

    #[test]
    fn with_the_upper_display_on_the_right_the_pairing_mirrors() {
        let screens = vec![built_in(), external()];
        let pushing_right = CGPoint::new(built_in().frame.max().x - 1.0, 500.0);
        let target =
            warp_target(&screens, pushing_right, StackedUpperSide::Right, LOWER_TOP_AT)
                .expect("should warp toward the upper display");
        assert_eq!(target.x, external().frame.origin.x + ENTRY_INSET);

        let pushing_left = CGPoint::new(built_in().frame.origin.x + 1.0, 500.0);
        assert!(
            warp_target(&screens, pushing_left, StackedUpperSide::Right, LOWER_TOP_AT).is_none()
        );
    }

    /// The vertical bug. Physically the laptop covers only the lower part of the external, so leaving
    /// the external at its MIDPOINT must land near the laptop's TOP, not its middle.
    ///
    /// Worked from the measured numbers: the external's midpoint is 195.5mm above its bottom, the
    /// laptop spans -67mm to 156.4mm, so the crossing lands above the laptop's top and clamps there.
    #[test]
    fn leaving_the_middle_of_the_big_screen_arrives_near_the_top_of_the_laptop() {
        let screens = vec![built_in(), external()];
        let ext = external().frame;
        let cursor = CGPoint::new(ext.max().x - 1.0, ext.origin.y + ext.size.height / 2.0);

        let target = warp(&screens, cursor).expect("should warp");
        let lap = built_in().frame;
        let fraction_down = (target.y - lap.origin.y) / lap.size.height;
        assert!(
            fraction_down < 0.1,
            "expected near the laptop's top, got {fraction_down:.3} of the way down"
        );
    }

    /// The inverse: leaving the laptop's midpoint lands well below the external's midpoint, because
    /// the laptop sits low on the external.
    #[test]
    fn leaving_the_middle_of_the_laptop_arrives_low_on_the_big_screen() {
        let screens = vec![built_in(), external()];
        let lap = built_in().frame;
        let cursor = CGPoint::new(lap.origin.x + 1.0, lap.origin.y + lap.size.height / 2.0);

        let target = warp(&screens, cursor).expect("should warp");
        let ext = external().frame;
        let fraction_down = (target.y - ext.origin.y) / ext.size.height;
        assert!(
            fraction_down > 0.6,
            "expected low on the external, got {fraction_down:.3} of the way down"
        );
    }

    /// The one position that must map exactly: the laptop's top edge sits at `lower_top_at` up the
    /// external, so leaving the laptop's top must arrive at 1 - 0.4 = 0.6 of the way DOWN the external.
    #[test]
    fn the_laptops_top_edge_maps_to_the_configured_fraction() {
        let screens = vec![built_in(), external()];
        let lap = built_in().frame;
        let cursor = CGPoint::new(lap.origin.x + 1.0, lap.origin.y);

        let target = warp(&screens, cursor).expect("should warp");
        let ext = external().frame;
        let fraction_down = (target.y - ext.origin.y) / ext.size.height;
        assert!(
            (fraction_down - (1.0 - LOWER_TOP_AT)).abs() < 0.01,
            "expected {:.2} down the external, got {fraction_down:.3}",
            1.0 - LOWER_TOP_AT
        );
    }

    /// Aligning the tops is expressible, and is the sanity check that the fraction is honoured rather
    /// than the measured arrangement being baked in.
    #[test]
    fn a_fraction_of_one_aligns_the_top_edges() {
        let screens = vec![built_in(), external()];
        let lap = built_in().frame;
        let cursor = CGPoint::new(lap.origin.x + 1.0, lap.origin.y);

        let target = warp_target(&screens, cursor, StackedUpperSide::Left, 1.0).expect("warp");
        let ext = external().frame;
        let fraction_down = (target.y - ext.origin.y) / ext.size.height;
        assert!(
            fraction_down < 0.01,
            "aligning tops should map top to top, got {fraction_down:.3}"
        );
    }

    /// Without physical sizes there is nothing to map through, so it must degrade to the old
    /// proportional behaviour rather than dividing by zero or flinging the pointer somewhere.
    #[test]
    fn unknown_physical_size_falls_back_to_proportional() {
        let lap = WarpScreen { frame: rect(0.0, 32.0, 1728.0, 1085.0), physical_height_mm: 0.0 };
        let ext =
            WarpScreen { frame: rect(-670.0, -1692.0, 3008.0, 1692.0), physical_height_mm: 0.0 };
        let screens = vec![lap, ext];
        let cursor = CGPoint::new(lap.frame.origin.x + 1.0, lap.frame.origin.y + lap.frame.size.height / 2.0);

        let target = warp(&screens, cursor).expect("should warp");
        let fraction_down = (target.y - ext.frame.origin.y) / ext.frame.size.height;
        assert!(
            (fraction_down - 0.5).abs() < 0.01,
            "expected the proportional midpoint, got {fraction_down:.3}"
        );
    }

    /// The entry point must be far enough in that the next poll does not re-trigger, or the cursor
    /// ping-pongs for as long as it is held against the edge.
    #[test]
    fn the_entry_point_does_not_immediately_warp_back() {
        let screens = vec![built_in(), external()];
        let cursor = CGPoint::new(built_in().frame.origin.x + 1.0, 500.0);

        let first = warp(&screens, cursor).expect("should warp");
        assert!(warp(&screens, first).is_none(), "landing point {first:?} re-triggered a warp");
    }

    /// The landing point must be inside the destination, never on its boundary, or the pointer can
    /// be considered to belong to neither display.
    #[test]
    fn the_landing_point_is_strictly_inside_the_destination() {
        let screens = vec![built_in(), external()];
        let ext = external().frame;
        // Cross at the very top of the external, which has no counterpart on the laptop.
        let cursor = CGPoint::new(ext.max().x - 1.0, ext.origin.y);

        let target = warp(&screens, cursor).expect("should warp");
        let lap = built_in().frame;
        assert!(target.y > lap.origin.y, "landed on or above the top edge: {target:?}");
        assert!(target.y < lap.max().y, "landed on or below the bottom edge: {target:?}");
    }

    /// Side-by-side displays must be left entirely alone: macOS crosses them natively.
    #[test]
    fn adjacent_displays_are_left_to_macos() {
        let left = WarpScreen { frame: rect(0.0, 0.0, 1000.0, 1000.0), physical_height_mm: 200.0 };
        let right =
            WarpScreen { frame: rect(1000.0, 0.0, 1000.0, 1000.0), physical_height_mm: 200.0 };
        let screens = vec![left, right];

        assert!(warp(&screens, CGPoint::new(999.0, 500.0)).is_none());
        assert!(warp(&screens, CGPoint::new(1001.0, 500.0)).is_none());
    }

    /// This is the precondition the standalone Swift daemon could not check.
    #[test]
    fn a_lone_display_never_warps() {
        let screens = vec![built_in()];
        assert!(warp(&screens, CGPoint::new(1.0, 500.0)).is_none());
    }

    #[test]
    fn a_cursor_away_from_any_edge_never_warps() {
        let screens = vec![built_in(), external()];
        assert!(warp(&screens, CGPoint::new(864.0, 500.0)).is_none());
    }

    /// With three displays stacked, the neighbour chosen is the one that actually lines up
    /// horizontally, not merely the first one found above.
    #[test]
    fn the_most_overlapping_neighbour_wins() {
        let middle =
            WarpScreen { frame: rect(0.0, 0.0, 1000.0, 500.0), physical_height_mm: 200.0 };
        let barely =
            WarpScreen { frame: rect(980.0, -500.0, 1000.0, 500.0), physical_height_mm: 200.0 };
        let mostly =
            WarpScreen { frame: rect(-100.0, -500.0, 1000.0, 500.0), physical_height_mm: 200.0 };
        let screens = vec![middle, barely, mostly];

        let target = warp(&screens, CGPoint::new(middle.frame.origin.x + 1.0, 250.0))
            .expect("should warp");
        assert_eq!(
            target.x,
            mostly.frame.max().x - ENTRY_INSET,
            "expected the display with the greater horizontal overlap"
        );
    }
}
