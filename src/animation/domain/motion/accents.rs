//! The accents a flight carries on top of its movement, the edge bounce and a move's nudge: which may
//! start, and when one waits for its flight. Their shapes are `bounce` and `nudge`.

use std::time::{Duration, Instant};

/// The two out-and-backs a flight carries on top of its movement: the edge bounce and a move's
/// nudge, which steps the whole strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutAndBack {
    Bounce,
    Nudge,
}

/// When each out-and-back playing now comes home. One still playing holds off another of its kind,
/// or a burst of presses replays it once per press; the two kinds never hold each other off. See
/// "Edge bounce" and "The move flight" in `src/animation/docs/animation-smoothness.md`.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct OutAndBacks {
    bounce: Option<Instant>,
    nudge: Option<Instant>,
}

impl OutAndBacks {
    /// Whether one of `kind` may start at `now`.
    pub fn admits(&self, kind: OutAndBack, now: Instant) -> bool {
        let until = match kind {
            OutAndBack::Bounce => self.bounce,
            OutAndBack::Nudge => self.nudge,
        };
        until.is_none_or(|until| now >= until)
    }

    /// One of `kind` starts at `now` and comes home after `duration`.
    pub fn start(
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

/// The nudge's admission from what is playing and whether the flight moves yet. An edge bounce
/// playing never holds it off.
pub fn nudge_admission(
    playing: &OutAndBacks,
    now: Instant,
    moving: bool,
) -> NudgeAdmission {
    if !playing.admits(OutAndBack::Nudge, now) {
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

    fn nudging_since(now: Instant) -> OutAndBacks {
        let mut playing = OutAndBacks::default();
        playing.start(OutAndBack::Nudge, now, std::time::Duration::from_millis(455));
        playing
    }

    /// Every combination the engine meets: one playing holds a nudge off whatever else holds, and a
    /// flight still collecting makes it wait.
    #[test]
    fn a_nudge_is_admitted_once_per_burst_and_waits_for_its_flight_to_move() {
        use std::time::Duration;
        let now = Instant::now();
        let soon = now + Duration::from_millis(100);
        let idle = OutAndBacks::default();
        assert_eq!(nudge_admission(&idle, now, true), NudgeAdmission::Start);
        assert_eq!(nudge_admission(&idle, now, false), NudgeAdmission::Wait);
        for moving in [true, false] {
            assert_eq!(
                nudge_admission(&nudging_since(now), soon, moving),
                NudgeAdmission::Skip,
                "a press while the step plays starts no other"
            );
        }
        let mut bouncing = OutAndBacks::default();
        bouncing.start(OutAndBack::Bounce, now, Duration::from_millis(350));
        assert_eq!(
            nudge_admission(&bouncing, soon, true),
            NudgeAdmission::Start,
            "a bounce at the end never holds the step off"
        );
        assert_eq!(
            nudge_admission(&nudging_since(now), now + Duration::from_millis(455), true),
            NudgeAdmission::Start,
            "once it is home the next press steps again"
        );
    }
}
