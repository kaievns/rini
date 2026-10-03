//! What capture work a running flight permits: the port transitions will drive.
//!
//! The flight engine reports where its flight is; these rules say which captures may start there, and
//! park the warms that may not. The budgets and cadences of the captures a flight runs live here too.
//! See "Capture work in flight" in `src/animation/docs/animation-smoothness.md`.

use std::time::Duration;

use crate::pictures::domain::target::PictureTarget;

/// Where a flight is between composition and lift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlightPhase {
    /// No flight.
    Idle,
    /// Composed, overlay up, not yet moving, nothing awaited.
    FrameZero,
    /// Overlay up, waiting at frame zero for reveal or entrance pictures.
    Holding,
    /// Tiles in motion.
    Moving,
}

/// A request to the window server that a flight might make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    /// Background captures for windows the reactor named (`warm_windows`).
    Warm,
    /// The full-display desktop render (`warm_desktop`).
    Desktop,
    /// The destination recapture (`refresh_destination_among`).
    Refresh,
    /// A reveal or entrance chase (`chase_reveal_pictures`).
    Chase,
    /// A hairline harvest for a landed picture.
    Harvest,
    /// A capture for a window SkyLight could not serve at composition.
    NeedsCapture,
}

/// Whether a flight in `phase` may start `kind` of capture work now.
pub fn capture_work_allowed(phase: FlightPhase, kind: CaptureKind) -> bool {
    match phase {
        FlightPhase::Idle => true,
        FlightPhase::FrameZero => matches!(kind, CaptureKind::Chase | CaptureKind::NeedsCapture),
        FlightPhase::Holding => matches!(kind, CaptureKind::Chase),
        FlightPhase::Moving => matches!(kind, CaptureKind::Chase | CaptureKind::Refresh),
    }
}

/// Parks warm targets asked for mid-flight, one per window; the latest request wins.
pub fn defer_warm(deferred: &mut Vec<PictureTarget>, targets: Vec<PictureTarget>) {
    for target in targets {
        match deferred.iter_mut().find(|held| held.window == target.window) {
            Some(held) => *held = target,
            None => deferred.push(target),
        }
    }
}

/// How long after an animation to recapture the bar: a bar composite is too slow (31ms median)
/// to pay per switch, so a burst of switches pays it once, at the end.
pub const BAR_REFRESH_DELAY: Duration = Duration::from_millis(250);

/// Chase poll interval and attempt budget for a growing window's real frame (about a second).
/// See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
pub const REVEAL_CHASE_INTERVAL: Duration = Duration::from_millis(8);

pub const REVEAL_CHASE_ATTEMPTS: usize = 125;

/// How many new windows one pass captures synchronously at spawn; the rest take a reservation.
/// See "A window that opens travels from its spawn frame" in `src/animation/docs/animation-smoothness.md`.
pub const MAX_SYNC_ENTRANCE_CAPTURES: usize = 4;
