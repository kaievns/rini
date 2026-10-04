//! The accents a flight carries on top of its movement, the edge bounce and a move's nudge: when a
//! nudge may start, and when one waits for its flight. The bounce is never held off: every push
//! against an end re-kicks it (`bounce::Bounce::pushed`). Their shapes are `bounce` and `nudge`.

use std::time::{Duration, Instant};

/// When a move's nudge playing now comes home. One still playing holds off the next, or a burst of
/// moves steps the strip once per press. The edge bounce is not held off: every push against an end
/// re-kicks it (`motion::bounce::Bounce::pushed`). See "Edge bounce" and "The move flight" in
/// `src/animation/docs/animation-smoothness.md`.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct NudgePlaying {
    until: Option<Instant>,
}

impl NudgePlaying {
    /// Whether a nudge may start at `now`.
    pub fn admits(&self, now: Instant) -> bool {
        self.until.is_none_or(|until| now >= until)
    }

    /// A nudge starts at `now` and comes home after `duration`.
    pub fn start(&mut self, now: Instant, duration: Duration) {
        self.until = Some(now + duration);
    }
}

/// What becomes of a move's nudge. See "The move flight, staged" in
/// `src/transitions/docs/coordination.md`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NudgeAdmission {
    /// It steps the strip from now, and the flight is stretched to carry it.
    Start,
    /// The flight is still collecting passes, which recomposes its containers and would drop it:
    /// it starts when the flight does.
    Wait,
    /// One is still playing: not in this flight, which keeps its plain duration.
    Skip,
}

/// The nudge's admission from the nudge playing and whether the flight moves yet. An edge bounce
/// is not consulted: it never holds the nudge off.
pub fn nudge_admission(playing: &NudgePlaying, now: Instant, moving: bool) -> NudgeAdmission {
    if !playing.admits(now) {
        NudgeAdmission::Skip
    } else if !moving {
        NudgeAdmission::Wait
    } else {
        NudgeAdmission::Start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYS: Duration = Duration::from_millis(350);

    /// A burst of moves steps the strip once, not once per press.
    #[test]
    fn a_move_while_its_nudge_plays_starts_no_other() {
        let now = Instant::now();
        let mut playing = NudgePlaying::default();
        playing.start(now, PLAYS);
        assert!(!playing.admits(now + Duration::from_millis(100)));
    }

    #[test]
    fn a_move_after_the_nudge_came_home_starts_one() {
        let now = Instant::now();
        let mut playing = NudgePlaying::default();
        assert!(playing.admits(now));
        playing.start(now, PLAYS);
        assert!(playing.admits(now + PLAYS));
    }

    fn nudging_since(now: Instant) -> NudgePlaying {
        let mut playing = NudgePlaying::default();
        playing.start(now, std::time::Duration::from_millis(455));
        playing
    }

    /// Every combination the engine meets: one playing holds a nudge off whatever else holds, and a
    /// flight still collecting makes it wait.
    #[test]
    fn a_nudge_is_admitted_once_per_burst_and_waits_for_its_flight_to_move() {
        use std::time::Duration;
        let now = Instant::now();
        let soon = now + Duration::from_millis(100);
        let idle = NudgePlaying::default();
        assert_eq!(nudge_admission(&idle, now, true), NudgeAdmission::Start);
        assert_eq!(nudge_admission(&idle, now, false), NudgeAdmission::Wait);
        for moving in [true, false] {
            assert_eq!(
                nudge_admission(&nudging_since(now), soon, moving),
                NudgeAdmission::Skip,
                "a press while the step plays starts no other"
            );
        }
        assert_eq!(
            nudge_admission(&nudging_since(now), now + Duration::from_millis(455), true),
            NudgeAdmission::Start,
            "once it is home the next press steps again"
        );
    }
}
