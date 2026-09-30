//! When the flight engine does what, and for how long.
//!
//! Every constant here was measured rather than chosen, and the comment on each says against what. The
//! functions are arithmetic over those constants and a progress fraction, so the feel of an animation
//! is a number in this file. Measurements in `src/animation/docs/animation-smoothness.md`.

use std::time::{Duration, Instant};

use super::flight::FlightKind;

/// Tick interval. Nothing is drawn on ticks; this only paces the mid-flight orchestration.
pub(in crate::animation) const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// How long to collect the reactor's layout passes before the movement starts.
/// See "Layout changes" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const COALESCE_WINDOW: Duration = Duration::from_millis(25);

/// Progress at which the focus change's two ends are recaptured: early enough to land and be cut in
/// while the flight still moves, then again for an app that repaints its focus late.
/// See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const REFRESH_PASSES_AT: [f64; 2] = [0.25, 0.55];

/// A reveal landing at or after this progress is cached only.
pub(in crate::animation) const REVEAL_APPLY_BEFORE: f64 = 0.6;

/// A focus refresh is swapped in up to here. Later than a reveal: the alternative to a cut while the
/// flight still moves is the same change after it lands, at rest, which is what reads as a flicker.
pub(in crate::animation) const REFRESH_APPLY_BEFORE: f64 = 0.9;

/// How long after an animation to recapture the bar: a bar composite is too slow (31ms median)
/// to pay per switch, so a burst of switches pays it once, at the end.
pub(in crate::animation) const BAR_REFRESH_DELAY: Duration = Duration::from_millis(250);

/// Progress at which a move-only layout flight places the real windows. Resizes and strips place
/// earlier (`apply_frames_at`). See "The apply point" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const APPLY_FRAMES_AT: f64 = 0.75;

/// How much larger than the window it traces a border window may be, per axis.
/// See "Window borders during animations" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const COMPANION_EXPANSION: f64 = 8.0;

/// How far the centers may disagree. The border window is centered on what it traces.
pub(in crate::animation) const COMPANION_CENTER_SLACK: f64 = 4.0;

/// The earlier apply point when a window resizes: the resize costs three synchronous round trips
/// into the owning app. See "The apply point" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const APPLY_FRAMES_AT_RESIZE: f64 = 0.5;

/// The apply point for a strip movement: frame zero, so a switch's serialized AX writes land
/// before lift. See "The apply point" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const APPLY_FRAMES_AT_PAN: f64 = 0.0;

/// Which apply point an animation needs.
pub(in crate::animation) fn apply_frames_at(kind: FlightKind, any_resize: bool) -> f64 {
    match (kind, any_resize) {
        (_, true) => APPLY_FRAMES_AT_RESIZE,
        (FlightKind::Layout, false) => APPLY_FRAMES_AT,
        (FlightKind::Pan, false) => APPLY_FRAMES_AT_PAN,
    }
}

/// A real window further than this from its intended frame at lift is a handover miss.
pub(in crate::animation) const HANDOVER_THRESHOLD_PT: f64 = 2.0;

/// The longest a flight stands still at frame zero for a reveal.
/// See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const HOLD_CAP: Duration = Duration::from_millis(300);

/// How long a grow may hold at frame zero for its reveal pixels, capped at `HOLD_CAP`.
pub(in crate::animation) fn reveal_hold_limit(duration: Duration) -> Duration {
    duration.mul_f64(0.4).max(Duration::from_millis(300)).min(HOLD_CAP)
}

/// Time a holding flight still waits before flying the placeholder; `None` once past the deadline.
pub(in crate::animation) fn hold_wait(
    hold_deadline: Option<Instant>,
    now: Instant,
) -> Option<Duration> {
    let deadline = hold_deadline?;
    (now < deadline).then(|| (deadline - now).max(Duration::from_millis(10)))
}

/// Chase poll interval and attempt budget for a growing window's real frame (about a second).
/// See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const REVEAL_CHASE_INTERVAL: Duration = Duration::from_millis(8);

pub(in crate::animation) const REVEAL_CHASE_ATTEMPTS: usize = 125;

/// How long a tile joining a flight already in motion travels: what is left of the flight.
pub(in crate::animation) fn late_join_duration(duration: Duration, progress: f64) -> Duration {
    duration.mul_f64((1.0 - progress).max(0.0))
}

/// How long after a lift the flight's owed captures wait for the user to stop pressing.
/// See "Capture work in flight" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const SETTLE_BEFORE_CAPTURES: Duration = Duration::from_millis(400);

/// How long past its clock a flight waits for the render server and the real windows before
/// lifting anyway. See "Real windows land before lift" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const LIFT_GRACE: Duration = Duration::from_millis(350);

/// The flight's clock once an out-and-back joins it, an edge bounce or a move's nudge: long enough
/// for it to come home, never shorter.
pub(in crate::animation) fn clock_for_bounce(
    started: Option<Instant>,
    duration: Duration,
    bounce: Duration,
) -> Duration {
    let needed = started.map_or(bounce, |s| s.elapsed() + bounce);
    duration.max(needed)
}

/// The two out-and-backs a flight carries on top of its movement: the edge bounce and a move's
/// nudge, which steps the whole strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::animation) enum OutAndBack {
    Bounce,
    Nudge,
}

/// When each out-and-back playing now comes home. One still playing holds off another of its kind,
/// or a burst of presses replays it once per press; the two kinds never hold each other off. See
/// "Edge bounce" and "The move flight" in `src/animation/docs/animation-smoothness.md`.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(in crate::animation) struct OutAndBacks {
    bounce: Option<Instant>,
    nudge: Option<Instant>,
}

impl OutAndBacks {
    /// Whether one of `kind` may start at `now`.
    pub(in crate::animation) fn admits(&self, kind: OutAndBack, now: Instant) -> bool {
        let until = match kind {
            OutAndBack::Bounce => self.bounce,
            OutAndBack::Nudge => self.nudge,
        };
        until.is_none_or(|until| now >= until)
    }

    /// One of `kind` starts at `now` and comes home after `duration`.
    pub(in crate::animation) fn start(
        &mut self,
        kind: OutAndBack,
        now: Instant,
        duration: Duration,
    ) {
        let until = Some(now + duration);
        match kind {
            OutAndBack::Bounce => self.bounce = until,
            OutAndBack::Nudge => self.nudge = until,
        }
    }
}

/// Whether the overlay lifts now: clock done AND (presented and landed, or `LIFT_GRACE` overdue).
pub(in crate::animation) fn lift_now(
    clock_done: bool,
    settled: bool,
    landed: bool,
    overdue: bool,
) -> bool {
    clock_done && ((settled && landed) || overdue)
}

/// How many new windows one pass captures synchronously at spawn; the rest take a reservation.
/// See "A window that opens travels from its spawn frame" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const MAX_SYNC_ENTRANCE_CAPTURES: usize = 4;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    const PLAYS: Duration = Duration::from_millis(350);

    /// The reported case: presses at the end of the strip kept the view bouncing off the wall, once
    /// per press, after the pressing had stopped. A move's nudge is held off the same way.
    #[test]
    fn a_press_while_one_plays_starts_no_other_of_its_kind() {
        let now = Instant::now();
        for kind in [OutAndBack::Bounce, OutAndBack::Nudge] {
            let mut playing = OutAndBacks::default();
            playing.start(kind, now, PLAYS);
            assert!(
                !playing.admits(kind, now + Duration::from_millis(100)),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_press_after_it_came_home_starts_one() {
        let now = Instant::now();
        for kind in [OutAndBack::Bounce, OutAndBack::Nudge] {
            let mut playing = OutAndBacks::default();
            assert!(playing.admits(kind, now));
            playing.start(kind, now, PLAYS);
            assert!(playing.admits(kind, now + PLAYS), "{kind:?}");
        }
    }

    /// A move at the end of the strip nudges and bounces at once: neither holds the other off.
    #[test]
    fn a_bounce_and_a_nudge_never_hold_each_other_off() {
        let now = Instant::now();
        let soon = now + Duration::from_millis(100);
        let mut playing = OutAndBacks::default();
        playing.start(OutAndBack::Bounce, now, PLAYS);
        assert!(playing.admits(OutAndBack::Nudge, soon), "a bounce playing");
        let mut playing = OutAndBacks::default();
        playing.start(OutAndBack::Nudge, now, PLAYS);
        assert!(playing.admits(OutAndBack::Bounce, soon), "a nudge playing");
        playing.start(OutAndBack::Bounce, now, PLAYS);
        assert!(!playing.admits(OutAndBack::Bounce, soon));
        assert!(!playing.admits(OutAndBack::Nudge, soon));
    }
}
