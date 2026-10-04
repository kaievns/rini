//! When the flight engine does what, and for how long.
//!
//! Every constant here was measured rather than chosen, and the comment on each says against what. The
//! functions are arithmetic over those constants and a progress fraction, so the feel of an animation
//! is a number in this file. Measurements in `src/animation/docs/animation-smoothness.md`.

use std::time::{Duration, Instant};

/// Tick interval. Nothing is drawn on ticks; this only paces the mid-flight orchestration.
pub(in crate::transitions) const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// How long to collect the reactor's layout passes before the movement starts.
/// See "Layout changes" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::transitions) const COALESCE_WINDOW: Duration = Duration::from_millis(25);

/// Progress at which the focus change's two ends are recaptured: early enough to land and be cut in
/// while the flight still moves, then again for an app that repaints its focus late.
/// See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::transitions) const REFRESH_PASSES_AT: [f64; 2] = [0.25, 0.55];

/// A reveal landing at or after this progress is cached only.
pub(in crate::transitions) const REVEAL_APPLY_BEFORE: f64 = 0.6;

/// A focus refresh is swapped in up to here. Later than a reveal: the alternative to a cut while the
/// flight still moves is the same change after it lands, at rest, which is what reads as a flicker.
pub(in crate::transitions) const REFRESH_APPLY_BEFORE: f64 = 0.9;

/// Progress at which a move-only layout flight places the real windows. Resizes and strips place
/// earlier (`apply_frames_at`). See "The apply point" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::transitions) const APPLY_FRAMES_AT: f64 = 0.75;

/// How much larger than the window it traces a border window may be, per axis.
/// See "Window borders during animations" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::transitions) const COMPANION_EXPANSION: f64 = 8.0;

/// How far the centers may disagree. The border window is centered on what it traces.
pub(in crate::transitions) const COMPANION_CENTER_SLACK: f64 = 4.0;

/// The earlier apply point when a window resizes: the resize costs three synchronous round trips
/// into the owning app. See "The apply point" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::transitions) const APPLY_FRAMES_AT_RESIZE: f64 = 0.5;

/// The apply point for a strip movement: frame zero, so a switch's serialized AX writes land
/// before lift. See "The apply point" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::transitions) const APPLY_FRAMES_AT_PAN: f64 = 0.0;

/// Which path composed a flight. See "The apply point" in `src/animation/docs/animation-smoothness.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::transitions) enum FlightKind {
    /// A per-window layout pass: moves and resizes.
    Layout,
    /// A strip movement: pure translations.
    Pan,
}

/// Which apply point an animation needs.
pub(in crate::transitions) fn apply_frames_at(kind: FlightKind, any_resize: bool) -> f64 {
    match (kind, any_resize) {
        (_, true) => APPLY_FRAMES_AT_RESIZE,
        (FlightKind::Layout, false) => APPLY_FRAMES_AT,
        (FlightKind::Pan, false) => APPLY_FRAMES_AT_PAN,
    }
}

/// A real window further than this from its intended frame at lift is a handover miss.
pub(in crate::transitions) const HANDOVER_THRESHOLD_PT: f64 = 2.0;

/// How long a flight stands still at frame zero for a reveal: a grow's pixels at its new size, or an
/// entrance's first picture, whatever the flight's duration.
/// See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::transitions) const HOLD_CAP: Duration = Duration::from_millis(300);

/// Time a holding flight still waits before flying the stand-in; `None` once past the deadline.
pub(in crate::transitions) fn hold_wait(
    hold_deadline: Option<Instant>,
    now: Instant,
) -> Option<Duration> {
    let deadline = hold_deadline?;
    (now < deadline).then(|| (deadline - now).max(Duration::from_millis(10)))
}

/// How long a tile joining a flight already in motion travels: what is left of the flight.
pub(in crate::transitions) fn late_join_duration(duration: Duration, progress: f64) -> Duration {
    duration.mul_f64((1.0 - progress).max(0.0))
}

/// How long after a lift the flight's owed captures wait for the user to stop pressing.
/// See "Capture work in flight" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::transitions) const SETTLE_BEFORE_CAPTURES: Duration = Duration::from_millis(400);

/// How long past its clock a flight waits for the render server and the real windows before
/// lifting anyway. See "Real windows land before lift" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::transitions) const LIFT_GRACE: Duration = Duration::from_millis(350);

/// The flight's clock once an out-and-back joins it, an edge bounce or a move's nudge: long enough
/// for it to come home, never shorter.
pub(in crate::transitions) fn clock_for_bounce(
    started: Option<Instant>,
    duration: Duration,
    bounce: Duration,
) -> Duration {
    let needed = started.map_or(bounce, |s| s.elapsed() + bounce);
    duration.max(needed)
}

/// Whether the overlay lifts now: clock done AND (presented and landed, or `LIFT_GRACE` overdue).
pub(in crate::transitions) fn lift_now(
    clock_done: bool,
    settled: bool,
    landed: bool,
    overdue: bool,
) -> bool {
    clock_done && ((settled && landed) || overdue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pictures::domain::schedule::{REVEAL_CHASE_ATTEMPTS, REVEAL_CHASE_INTERVAL};
    use crate::pictures::domain::settle::chase_settled;

    /// The layout pass's apply points as they were before strip movements moved theirs to frame zero.
    fn apply_frames_at_old(any_resize: bool) -> f64 {
        if any_resize {
            APPLY_FRAMES_AT_RESIZE
        } else {
            APPLY_FRAMES_AT
        }
    }

    #[test]
    fn a_resize_places_the_real_windows_earlier() {
        assert!(
            apply_frames_at(FlightKind::Layout, true) < apply_frames_at(FlightKind::Layout, false)
        );
        assert_eq!(apply_frames_at(FlightKind::Layout, false), APPLY_FRAMES_AT);
        assert_eq!(apply_frames_at(FlightKind::Layout, true), APPLY_FRAMES_AT_RESIZE);
    }

    #[test]
    fn apply_points_by_flight_kind() {
        assert_eq!(apply_frames_at(FlightKind::Layout, false), 0.75);
        assert_eq!(apply_frames_at(FlightKind::Layout, true), 0.5);
        assert_eq!(apply_frames_at(FlightKind::Pan, false), APPLY_FRAMES_AT_PAN);
        assert_eq!(
            APPLY_FRAMES_AT_PAN, 0.0,
            "a strip movement places its windows at frame zero"
        );
        assert_eq!(apply_frames_at(FlightKind::Pan, true), 0.5);
    }

    #[test]
    fn a_strip_movement_applies_frames_by_the_midpoint() {
        let at = apply_frames_at(FlightKind::Pan, false);
        assert!(at <= 0.5, "strip apply point is {at}, leaving too little runway");
    }

    #[test]
    fn layout_apply_points_are_unchanged() {
        for any_resize in [false, true] {
            assert_eq!(
                apply_frames_at(FlightKind::Layout, any_resize),
                apply_frames_at_old(any_resize),
                "any_resize={any_resize}"
            );
        }
        assert_eq!(apply_frames_at_old(false), 0.75);
        assert_eq!(apply_frames_at_old(true), 0.5);
    }

    #[test]
    fn a_bounce_extends_the_clock_to_cover_its_return() {
        let bounce = Duration::from_millis(350);
        assert_eq!(
            clock_for_bounce(None, Duration::from_millis(100), bounce),
            bounce
        );
        assert_eq!(
            clock_for_bounce(None, Duration::from_millis(900), bounce),
            Duration::from_millis(900)
        );
        let started = Instant::now() - Duration::from_millis(300);
        let extended = clock_for_bounce(Some(started), Duration::from_millis(350), bounce);
        assert!(extended >= Duration::from_millis(650), "{extended:?}");
        let long = clock_for_bounce(Some(started), Duration::from_secs(5), bounce);
        assert_eq!(long, Duration::from_secs(5));
    }

    #[test]
    fn the_overlay_lifts_when_the_clock_is_done_and_the_tiles_are_presented_there() {
        assert!(!lift_now(false, true, true, false), "the clock has not run out");
        assert!(
            !lift_now(false, true, true, true),
            "overdue is meaningless before the clock is done"
        );
        assert!(
            !lift_now(true, false, true, false),
            "clock done, render server a frame behind: wait"
        );
        assert!(
            !lift_now(true, true, false, false),
            "clock done, a real window still travelling: wait"
        );
        assert!(lift_now(true, true, true, false));
        assert!(
            lift_now(true, false, false, true),
            "the grace ran out: lift anyway"
        );
        assert!(
            LIFT_GRACE < Duration::from_millis(500),
            "a stall is a hold, not a hang"
        );
    }

    /// See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
    #[test]
    fn the_hold_is_capped_at_a_blink() {
        assert_eq!(HOLD_CAP, Duration::from_millis(300));
        assert_eq!(REVEAL_CHASE_INTERVAL, Duration::from_millis(8));
        assert_eq!(
            REVEAL_CHASE_INTERVAL * REVEAL_CHASE_ATTEMPTS as u32,
            Duration::from_secs(1)
        );
    }

    /// See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
    #[test]
    fn a_hold_is_short_and_settles_on_the_first_repaint() {
        let p = vec![10u8; 64];
        let q = vec![200u8; 64];
        let settled = chase_settled(None, &p, Some(&q));
        let wrong: Vec<String> = [
            (
                HOLD_CAP <= Duration::from_millis(300),
                format!("HOLD_CAP = {HOLD_CAP:?}"),
            ),
            (
                REVEAL_CHASE_INTERVAL == Duration::from_millis(8),
                format!("REVEAL_CHASE_INTERVAL = {REVEAL_CHASE_INTERVAL:?}"),
            ),
            (
                settled,
                format!("chase_settled(None, p, Some(q != p)) = {settled}"),
            ),
        ]
        .into_iter()
        .filter(|(ok, _)| !ok)
        .map(|(_, why)| why)
        .collect();
        assert!(
            wrong.is_empty(),
            "hold is not bounded and cheap:\n{}",
            wrong.join("\n")
        );
    }
}
