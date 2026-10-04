//! When the flight engine does what, and for how long.
//!
//! Every constant here was measured rather than chosen, and the comment on each says against what. The
//! lifecycle (`crate::transitions::domain::lifecycle`) decides with them. Measurements in
//! `src/transitions/docs/coordination.md`.

use std::time::Duration;

/// Tick interval. Nothing is drawn on ticks; the clock only paces the lifecycle.
pub(in crate::transitions) const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// How long to collect the reactor's layout passes before the movement starts.
/// See "Layout changes" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) const COALESCE_WINDOW: Duration = Duration::from_millis(25);

/// How much larger than the window it traces a border window may be, per axis.
/// See "Border companions" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) const COMPANION_EXPANSION: f64 = 8.0;

/// How far the centers may disagree. The border window is centered on what it traces.
pub(in crate::transitions) const COMPANION_CENTER_SLACK: f64 = 4.0;

/// A real window further than this from its intended frame at lift is a handover miss.
pub(in crate::transitions) const HANDOVER_THRESHOLD_PT: f64 = 2.0;

/// How long a flight stands still with the overlay up for a reveal: a grow's pixels at its new size,
/// or an entrance's first picture, whatever the flight's duration.
/// See "A grow holds, then reveals" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) const HOLD_CAP: Duration = Duration::from_millis(300);

/// How long after a lift the flight's owed captures wait for the user to stop pressing.
/// See "Capture work in flight" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) const SETTLE_BEFORE_CAPTURES: Duration = Duration::from_millis(400);

/// How long past its clock a flight waits for the render server and the real windows before
/// lifting anyway. See "Real windows land before lift" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) const LIFT_GRACE: Duration = Duration::from_millis(350);

#[cfg(test)]
mod tests {
    use super::*;

    /// See "A grow holds, then reveals" in `src/transitions/docs/coordination.md`.
    #[test]
    fn the_hold_is_capped_at_a_blink() {
        assert_eq!(HOLD_CAP, Duration::from_millis(300));
    }

    #[test]
    fn a_stall_past_the_clock_is_a_hold_not_a_hang() {
        assert!(LIFT_GRACE < Duration::from_millis(500));
    }
}
