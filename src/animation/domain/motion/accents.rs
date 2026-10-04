//! The accents a flight carries on top of its movement, the edge bounce and a move's step, and the
//! tracks each plays on the containers that carry it. A push against an end re-kicks every
//! carrier's bounce from where that carrier's is drawn (`push_tracks`), never held off; a step is
//! taken only while none plays (`steps`), or a burst of moves steps the strip once per press, and
//! plays on every carrier (`step_tracks`). Their shapes are `bounce` and `nudge`.

use std::time::Duration;

use objc2_core_foundation::CGPoint;

use super::bounce::Bounce;
use super::nudge::nudge_track;
use super::plan::carries_out_and_back;
use super::score::{Channel, Node, SAMPLE_STEP, Score, Track, Value};

/// What a press asks to play on top of its movement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Accent {
    /// A push against an end: out toward `wall` and home over `seconds`.
    Push { wall: CGPoint, seconds: f64 },
    /// A move's step: the whole strip out by `offset` and back, over its press's plain `seconds`
    /// stretched, the flight it rides lasting at least as long.
    Step { offset: CGPoint, seconds: f64 },
}

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

/// Whether a press's step is taken at `at`: only while none plays anywhere in `playing`, or a burst
/// of moves steps the strip once per press.
pub fn steps(playing: &Score, at: f64) -> bool { !playing.plays(Channel::Nudge, at) }

/// The step out by `offset` and back over `seconds` from `at`, one `nudge_track` on each container of
/// `playing` that carries it. See "The move flight" in `src/animation/docs/animation-smoothness.md`.
pub fn step_tracks(playing: &Score, offset: CGPoint, seconds: f64, at: f64) -> Vec<Track> {
    playing
        .containers()
        .filter(|key| carries_out_and_back(*key, offset))
        .map(|key| nudge_track(key, offset, at, seconds, SAMPLE_STEP))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::domain::motion::plan::GroupKey;
    use crate::animation::domain::motion::score::Edit;

    const STRIP: GroupKey = GroupKey::STILL;

    /// The strip and the floating container, placed, nothing playing.
    fn surface() -> Score {
        let mut score = Score::default();
        for key in [STRIP, GroupKey::Floating] {
            score.apply(&Edit::Place(
                Node::Container(key),
                Value::Point(CGPoint::new(-861.0, 0.0)),
            ));
        }
        score
    }

    fn carriers(tracks: &[Track]) -> Vec<GroupKey> {
        let mut keys: Vec<GroupKey> = tracks
            .iter()
            .map(|track| match track.node() {
                Node::Container(key) => key,
                Node::Tile(_) => unreachable!("an accent rides containers"),
            })
            .collect();
        keys.sort_by_key(|key| format!("{key:?}"));
        keys
    }

    mod pushes {
        use super::*;

        const PLAYS: f64 = 0.35;
        const UP: CGPoint = CGPoint { x: 0.0, y: -72.0 };
        const LEFT: CGPoint = CGPoint { x: -72.0, y: 0.0 };

        fn plays() -> Duration {
            Duration::from_secs_f64(PLAYS)
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
                score.apply(&Edit::Add(track));
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
        /// from that older bounce rather than jumping to rest. One a merge opened while a bounce
        /// plays draws none of it and starts the next from rest, as the strip goes on from its own.
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

            let mut opened = surface();
            push(&mut opened, UP, 30.0);
            let column = GroupKey::Rigid(7);
            opened.apply(&Edit::Place(
                Node::Container(column),
                Value::Point(CGPoint::new(-861.0, 0.0)),
            ));
            let mid = 30.06;
            let strip_drawn = drawn(&opened, STRIP, mid);
            let next = push(&mut opened, UP, mid);
            assert_eq!(
                on(&next, column),
                Some(&Bounce::from_rest(UP, plays()).track(column, mid)),
                "opened mid-bounce: from rest, where it is drawn"
            );
            assert_eq!(
                on(&next, STRIP),
                Some(&Bounce::from_drawn(strip_drawn, UP, plays()).track(STRIP, mid))
            );

            let mut gone = surface();
            push(&mut gone, LEFT, 20.0);
            gone.apply(&Edit::Cut(Node::Container(STRIP), Channel::Bounce));
            let next = push(&mut gone, LEFT, 20.06);
            assert_eq!(
                on(&next, STRIP),
                Some(&Bounce::from_rest(LEFT, plays()).track(STRIP, 20.06)),
                "gone with its tracks: from rest, or the surface would jump"
            );
        }
    }

    /// A step is taken only while none plays: one from rest, on every carrier, none for a press
    /// while it plays however many come, and one again once it is home. A burst of moves steps the
    /// strip once, not once per press.
    #[test]
    fn a_step_is_taken_only_while_none_plays() {
        let mut score = surface();
        let (offset, seconds) = (CGPoint::new(-576.0, 0.0), 0.455);
        assert!(steps(&score, 10.0), "from rest");
        let first = step_tracks(&score, offset, seconds, 10.0);
        assert_eq!(first, [nudge_track(STRIP, offset, 10.0, seconds, SAMPLE_STEP)]);
        for track in first {
            score.apply(&Edit::Add(track));
        }
        for at in [10.0, 10.1, 10.3, 10.455 - 1e-9] {
            assert!(!steps(&score, at), "{at}: one plays");
        }
        assert!(steps(&score, 10.0 + seconds), "home: the next press steps again");
    }

    /// The floating container carries only a vertical accent, as a pan pins it and a switch carries
    /// it; every strip container carries either.
    #[test]
    fn the_floating_container_carries_only_a_vertical_accent() {
        let score = surface();
        let (left, up) = (CGPoint::new(-72.0, 0.0), CGPoint::new(0.0, -72.0));
        assert_eq!(carriers(&push_tracks(&score, left, 0.35, 1.0)), [STRIP]);
        assert_eq!(carriers(&push_tracks(&score, up, 0.35, 1.0)), [
            GroupKey::Floating,
            STRIP
        ]);
        assert_eq!(carriers(&step_tracks(&score, left, 0.455, 1.0)), [STRIP]);
        assert_eq!(carriers(&step_tracks(&score, up, 0.455, 1.0)), [
            GroupKey::Floating,
            STRIP
        ]);
    }
}
