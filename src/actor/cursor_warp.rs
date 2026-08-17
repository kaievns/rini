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
    ScreensChanged(Vec<CGRect>),
    /// Enable or disable warping without tearing the actor down, so a config reload can
    /// toggle it.
    SetEnabled(bool),
    /// Which side the logically-upper display physically sits on.
    SetUpperSide(StackedUpperSide),
    Stop,
}

pub type Sender = crate::actor::Sender<Request>;
pub type Receiver = crate::actor::Receiver<Request>;

pub struct CursorWarp {
    rx: Receiver,
    enabled: bool,
    upper_side: StackedUpperSide,
    screens: Vec<CGRect>,
    last_warp: Option<Instant>,
}

impl CursorWarp {
    pub fn new(enabled: bool, upper_side: StackedUpperSide, rx: Receiver) -> Self {
        Self {
            rx,
            enabled,
            upper_side,
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
        let Some(target) = warp_target(&self.screens, cursor, self.upper_side) else {
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
    screens: &[CGRect],
    cursor: CGPoint,
    upper_side: StackedUpperSide,
) -> Option<CGPoint> {
    if screens.len() < 2 {
        return None;
    }
    let here = display_containing(screens, cursor)?;

    let at_left = cursor.x <= here.origin.x + EDGE_SLOP;
    let at_right = cursor.x >= here.max().x - EDGE_SLOP;
    if !(at_left || at_right) {
        return None;
    }

    // If a display lies beyond that edge on this row, macOS crosses natively. Interfering
    // would fight it, and the result is a cursor that jitters at the boundary.
    let beyond = CGPoint::new(
        if at_left {
            here.origin.x - BEYOND_PROBE
        } else {
            here.max().x + BEYOND_PROBE
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

    // Preserve position along the edge proportionally, so entering high on a short display
    // leaves you high on a tall one.
    let fraction = if here.size.height > 0.0 {
        (cursor.y - here.origin.y) / here.size.height
    } else {
        0.5
    };
    let y = (target.origin.y + fraction * target.size.height)
        .max(target.origin.y + 1.0)
        .min(target.max().y - 1.0);
    // Arrive at the FAR edge of the destination, so travel continues rather than reversing. Moving
    // left must land near the destination's right edge, and moving right near its left edge.
    let x = if at_left {
        target.max().x - ENTRY_INSET
    } else {
        target.origin.x + ENTRY_INSET
    };
    Some(CGPoint::new(x, y))
}

/// The display containing `point`.
///
/// Outset by half a point: a cursor clamped at `maxX - 1` still belongs to its display,
/// and without the slack a point exactly on a shared edge belongs to neither.
fn display_containing(screens: &[CGRect], point: CGPoint) -> Option<CGRect> {
    screens.iter().copied().find(|screen| {
        let padded = CGRect::new(
            CGPoint::new(screen.origin.x - 0.5, screen.origin.y - 0.5),
            objc2_core_foundation::CGSize::new(screen.size.width + 1.0, screen.size.height + 1.0),
        );
        padded.contains(point)
    })
}

/// The display directly above or below `frame`, preferring the one with the greatest
/// horizontal overlap — i.e. the visually adjacent one when three displays are stacked.
fn vertical_neighbour(screens: &[CGRect], frame: CGRect, going_up: bool) -> Option<CGRect> {
    screens
        .iter()
        .copied()
        .filter(|other| {
            if other.origin == frame.origin && other.size == frame.size {
                return false;
            }
            // 1pt tolerance: stacked displays usually share an edge exactly, but a menu bar
            // inset or a rounding difference should not disqualify a neighbour.
            if going_up {
                other.max().y <= frame.origin.y + 1.0
            } else {
                other.origin.y >= frame.max().y - 1.0
            }
        })
        .max_by(|a, b| {
            overlap_x(*a, frame)
                .partial_cmp(&overlap_x(*b, frame))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn overlap_x(a: CGRect, b: CGRect) -> f64 {
    (a.max().x.min(b.max().x) - a.origin.x.max(b.origin.x)).max(0.0)
}

/// Frames of every attached screen, for `Request::ScreensChanged`.
pub fn frames_of(screens: &[ScreenInfo]) -> Vec<CGRect> {
    screens.iter().map(|screen| screen.frame).collect()
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
    }

    /// The real arrangement this was built for, read from `rini-cli query displays`: the external
    /// logically ABOVE the built-in, with overlapping x ranges, while physically sitting to its LEFT.
    const BUILT_IN: fn() -> CGRect = || rect(0.0, 32.0, 1728.0, 1085.0);
    const EXTERNAL: fn() -> CGRect = || rect(-670.0, -1692.0, 3008.0, 1692.0);

    fn upper_left(screens: &[CGRect], cursor: CGPoint) -> Option<CGPoint> {
        warp_target(screens, cursor, StackedUpperSide::Left)
    }

    /// The bug this file was reported for. With the upper display physically on the LEFT, pushing
    /// left off the built-in must continue leftwards onto the external's RIGHT edge. The original
    /// implementation paired right-with-up and entered at the near edge, so it sent the pointer to
    /// the opposite screen and the opposite side of it.
    #[test]
    fn pushing_left_continues_onto_the_upper_displays_right_edge() {
        let screens = vec![BUILT_IN(), EXTERNAL()];
        let built_in = BUILT_IN();
        let cursor = CGPoint::new(built_in.origin.x + 1.0, built_in.origin.y + 542.5);

        let target = upper_left(&screens, cursor).expect("should warp");
        let external = EXTERNAL();
        assert_eq!(
            target.x,
            external.max().x - ENTRY_INSET,
            "must arrive near the far (right) edge so leftward travel continues"
        );
    }

    /// The return trip: pushing right off the upper display continues onto the lower one's LEFT edge.
    #[test]
    fn pushing_right_continues_onto_the_lower_displays_left_edge() {
        let screens = vec![BUILT_IN(), EXTERNAL()];
        let external = EXTERNAL();
        let cursor = CGPoint::new(external.max().x - 1.0, external.origin.y + 846.0);

        let target = upper_left(&screens, cursor).expect("should warp");
        assert_eq!(target.x, BUILT_IN().origin.x + ENTRY_INSET);
    }

    /// Pushing AWAY from the upper display must do nothing. With the external on the left there is
    /// no screen to the right of the built-in, so the pointer should simply stop.
    #[test]
    fn pushing_away_from_the_upper_display_does_not_warp() {
        let screens = vec![BUILT_IN(), EXTERNAL()];
        let built_in = BUILT_IN();
        let cursor = CGPoint::new(built_in.max().x - 1.0, built_in.origin.y + 542.5);
        assert!(
            upper_left(&screens, cursor).is_none(),
            "nothing lies right of the built-in, so the pointer must stop"
        );
    }

    /// The mirror arrangement, so the setting is genuinely honoured rather than the default being
    /// hardcoded twice.
    #[test]
    fn with_the_upper_display_on_the_right_the_pairing_mirrors() {
        let screens = vec![BUILT_IN(), EXTERNAL()];
        let built_in = BUILT_IN();
        let external = EXTERNAL();

        let pushing_right = CGPoint::new(built_in.max().x - 1.0, built_in.origin.y + 542.5);
        let target = warp_target(&screens, pushing_right, StackedUpperSide::Right)
            .expect("should warp toward the upper display");
        assert_eq!(target.x, external.origin.x + ENTRY_INSET);

        let pushing_left = CGPoint::new(built_in.origin.x + 1.0, built_in.origin.y + 542.5);
        assert!(
            warp_target(&screens, pushing_left, StackedUpperSide::Right).is_none(),
            "with the upper display on the right, nothing lies left of the built-in"
        );
    }

    #[test]
    fn position_along_the_edge_is_preserved_proportionally() {
        let screens = vec![BUILT_IN(), EXTERNAL()];
        let built_in = BUILT_IN();
        let external = EXTERNAL();
        let cursor = CGPoint::new(built_in.origin.x + 1.0, built_in.origin.y + built_in.size.height / 2.0);

        let target = upper_left(&screens, cursor).expect("should warp");
        assert!(
            (target.y - (external.origin.y + external.size.height / 2.0)).abs() < 1.0,
            "halfway down one edge should be halfway down the other, got {target:?}"
        );
    }

    /// The entry point must be far enough in that the next poll does not re-trigger. Otherwise the
    /// cursor ping-pongs between displays for as long as it is held there, which is what makes a
    /// too-small inset feel like the pointer is stuck.
    #[test]
    fn the_entry_point_does_not_immediately_warp_back() {
        let screens = vec![BUILT_IN(), EXTERNAL()];
        let built_in = BUILT_IN();
        let cursor = CGPoint::new(built_in.origin.x + 1.0, built_in.origin.y + 542.5);

        let first = upper_left(&screens, cursor).expect("should warp");
        assert!(
            upper_left(&screens, first).is_none(),
            "landing point {first:?} re-triggered a warp"
        );
    }

    /// Side-by-side displays must be left entirely alone: macOS crosses them natively.
    #[test]
    fn adjacent_displays_are_left_to_macos() {
        let left = rect(0.0, 0.0, 1000.0, 1000.0);
        let right = rect(1000.0, 0.0, 1000.0, 1000.0);
        let screens = vec![left, right];

        assert!(upper_left(&screens, CGPoint::new(999.0, 500.0)).is_none());
        assert!(upper_left(&screens, CGPoint::new(1001.0, 500.0)).is_none());
    }

    /// This is the precondition the standalone Swift daemon could not check. Displays that are
    /// neither stacked nor adjacent give no vertical neighbour, so nothing happens rather than the
    /// cursor being flung somewhere arbitrary.
    #[test]
    fn a_lone_display_never_warps() {
        let screens = vec![BUILT_IN()];
        let built_in = BUILT_IN();
        assert!(upper_left(&screens, CGPoint::new(built_in.origin.x + 1.0, 500.0)).is_none());
    }

    #[test]
    fn a_cursor_away_from_any_edge_never_warps() {
        let screens = vec![BUILT_IN(), EXTERNAL()];
        assert!(upper_left(&screens, CGPoint::new(864.0, 500.0)).is_none());
    }

    /// With three displays stacked, the neighbour chosen is the one that actually lines up
    /// horizontally, not merely the first one found above.
    #[test]
    fn the_most_overlapping_neighbour_wins() {
        let middle = rect(0.0, 0.0, 1000.0, 500.0);
        let barely = rect(980.0, -500.0, 1000.0, 500.0); // 20pt of overlap
        let mostly = rect(-100.0, -500.0, 1000.0, 500.0); // 900pt of overlap
        let screens = vec![middle, barely, mostly];

        let target = upper_left(&screens, CGPoint::new(middle.origin.x + 1.0, 250.0))
            .expect("should warp");
        assert_eq!(
            target.x,
            mostly.max().x - ENTRY_INSET,
            "expected the display with the greater horizontal overlap"
        );
    }
}
