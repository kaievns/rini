//! The accents a flight carries on top of its movement, the edge bounce and a move's nudge: when a
//! nudge may start, when one waits for its flight, and where a push against an end starts each
//! container's bounce. The bounce is never held off: every push re-kicks it on every container that
//! carries it, from where that container's bounce is drawn (`push_tracks`). Their shapes are
//! `bounce` and `nudge`.

use std::time::{Duration, Instant};

use objc2_core_foundation::CGPoint;

use super::bounce::Bounce;
use super::plan::carries_out_and_back;
use super::score::{Channel, Node, Score, Track, Value};

/// The bounce a push against `wall` at `at` plays on each container of `playing` that carries it,
/// each from where its own bounce is drawn then: a container whose bounce is home, or gone with its
/// tracks, starts from rest. See "Edge bounce" in `src/animation/docs/animation-smoothness.md`.
pub fn push_tracks(playing: &Score, wall: CGPoint, seconds: f64, at: f64) -> Vec<Track> {
    let duration = Duration::from_secs_f64(seconds);
    playing
        .containers()
        .filter(|key| carries_out_and_back(*key, wall))
        .map(|key| {
            let drawn = match playing
                .track(Node::Container(key), Channel::Bounce)
                .and_then(|bounce| bounce.at(at))
            {
                Some(Value::Point(drawn)) => drawn,
                _ => CGPoint::new(0.0, 0.0),
            };
            Bounce::from_drawn(drawn, wall, duration).track(key, at)
        })
        .collect()
}

/// When a move's nudge playing now comes home. One still playing holds off the next, or a burst of
/// moves steps the strip once per press. The edge bounce is not held off: every push against an end
/// re-kicks it (`push_tracks`). See "Edge bounce" and "The move flight" in
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
    /// The flight is still collecting passes, each installing its containers again, which would drop
    /// it: it starts when the flight does.
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

    mod pushes {
        use super::*;
        use crate::animation::domain::motion::plan::GroupKey;

        const PLAYS: f64 = 0.35;
        const UP: CGPoint = CGPoint { x: 0.0, y: -72.0 };
        const LEFT: CGPoint = CGPoint { x: -72.0, y: 0.0 };
        const STRIP: GroupKey = GroupKey::STILL;

        fn plays() -> Duration {
            Duration::from_secs_f64(PLAYS)
        }

        /// The strip and the floating container, placed, nothing playing.
        fn surface() -> Score {
            let mut score = Score::default();
            for key in [STRIP, GroupKey::Floating] {
                score.place(Node::Container(key), Value::Point(CGPoint::new(-861.0, 0.0)));
            }
            score
        }

        /// Pushes against `wall` at `at`, as the overlay does: each carrier's track replaces the
        /// bounce on it. Returns what the push made, by container.
        fn push(score: &mut Score, wall: CGPoint, at: f64) -> Vec<(GroupKey, Track)> {
            let tracks = push_tracks(score, wall, PLAYS, at);
            let made = tracks
                .iter()
                .map(|track| match track.node() {
                    Node::Container(key) => (key, track.clone()),
                    Node::Tile(_) => unreachable!("a push rides containers"),
                })
                .collect();
            for track in tracks {
                score.add(track);
            }
            made
        }

        fn on(made: &[(GroupKey, Track)], key: GroupKey) -> Option<&Track> {
            made.iter().find(|(k, _)| *k == key).map(|(_, track)| track)
        }

        fn drawn(score: &Score, key: GroupKey, at: f64) -> CGPoint {
            match score.track(Node::Container(key), Channel::Bounce).and_then(|t| t.at(at)) {
                Some(Value::Point(p)) => p,
                _ => CGPoint::new(0.0, 0.0),
            }
        }

        /// Every push at an end starts each carrier's bounce from where that carrier's is drawn: a
        /// carrier at rest exactly from rest, one mid-bounce from where it is, never over from rest,
        /// and one whose bounce is home or gone, as installing frame zero again and the lift take
        /// it, from rest. A container that does not carry this push, the floating windows under a
        /// sideways one, plays none and keeps what it draws; under the next vertical push it goes on
        /// from that older bounce rather than jumping to rest.
        #[test]
        fn a_push_starts_each_carrier_from_where_its_bounce_is_drawn() {
            let mut score = surface();
            let first = push(&mut score, UP, 10.0);
            for key in [STRIP, GroupKey::Floating] {
                assert_eq!(
                    on(&first, key),
                    Some(&Bounce::from_rest(UP, plays()).track(key, 10.0)),
                    "{key:?} at rest starts from rest"
                );
            }

            let at = 10.06;
            let up_drawn = drawn(&score, STRIP, at);
            assert_ne!(up_drawn, CGPoint::new(0.0, 0.0));
            let sideways = push(&mut score, LEFT, at);
            assert_eq!(
                on(&sideways, STRIP),
                Some(&Bounce::from_drawn(up_drawn, LEFT, plays()).track(STRIP, at)),
                "not over from rest"
            );
            assert_eq!(on(&sideways, GroupKey::Floating), None, "only a vertical push");

            let later = 10.15;
            let (strip_drawn, floating_drawn) = (
                drawn(&score, STRIP, later),
                drawn(&score, GroupKey::Floating, later),
            );
            assert!(
                floating_drawn.y < -1.0,
                "still drawing the first: {floating_drawn:?}"
            );
            assert!(strip_drawn.x < -1.0, "drawing the sideways one: {strip_drawn:?}");
            let again = push(&mut score, UP, later);
            assert_eq!(
                on(&again, STRIP),
                Some(&Bounce::from_drawn(strip_drawn, UP, plays()).track(STRIP, later))
            );
            assert_eq!(
                on(&again, GroupKey::Floating),
                Some(
                    &Bounce::from_drawn(floating_drawn, UP, plays())
                        .track(GroupKey::Floating, later)
                ),
                "from the older bounce it still draws"
            );

            let home = later + PLAYS;
            let after = push(&mut score, UP, home);
            assert_eq!(
                on(&after, STRIP),
                Some(&Bounce::from_rest(UP, plays()).track(STRIP, home)),
                "home: from rest"
            );

            let mut gone = surface();
            push(&mut gone, LEFT, 20.0);
            gone.cut(Node::Container(STRIP), Channel::Bounce);
            let next = push(&mut gone, LEFT, 20.06);
            assert_eq!(
                on(&next, STRIP),
                Some(&Bounce::from_rest(LEFT, plays()).track(STRIP, 20.06)),
                "gone with its tracks: from rest, or the surface would jump"
            );
        }
    }

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
