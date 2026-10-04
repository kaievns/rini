//! The edge bounce: the surface pushed out toward the wall it hit and eased home. `pushed` is a
//! push arriving mid-bounce, a new one from wherever the last is drawn, so a burst at the wall
//! shows every press and none starts over from rest. See "Edge bounce" in
//! `src/animation/docs/animation-smoothness.md`.
use std::time::Duration;

use objc2_core_foundation::CGPoint;

use super::easing::{BOUNCE_TURN, EASE_IN_OUT, MOTION_CURVE};
use super::plan::GroupKey;
use super::score::{Channel, Curve, Node, Track, Value};

/// The shortest way out a push is given, as a fraction of a full one. A push that finds the
/// surface already at the wall holds it there this long instead of turning straight back.
pub const SHORTEST_PUSH: f64 = 0.4;

/// One bounce as Core Animation draws it: out from `from` to `overshoot` on the motion curve until
/// `turn` of `duration`, then home to rest on ease-in-out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounce {
    pub from: CGPoint,
    pub overshoot: CGPoint,
    /// Where the way home begins, as a fraction of `duration`.
    pub turn: f64,
    pub duration: Duration,
}

impl Bounce {
    /// A push with nothing playing: out from rest, the full way.
    pub fn from_rest(overshoot: CGPoint, duration: Duration) -> Self {
        Bounce {
            from: CGPoint::new(0.0, 0.0),
            overshoot,
            turn: BOUNCE_TURN,
            duration,
        }
    }

    /// The displacement `elapsed` into the bounce.
    pub fn at(&self, elapsed: Duration) -> CGPoint {
        let total = self.duration.as_secs_f64();
        let t = if total > 0.0 {
            (elapsed.as_secs_f64() / total).clamp(0.0, 1.0)
        } else {
            1.0
        };
        if t <= self.turn && self.turn > 0.0 {
            let k = MOTION_CURVE.ease(t / self.turn);
            CGPoint::new(
                self.from.x + (self.overshoot.x - self.from.x) * k,
                self.from.y + (self.overshoot.y - self.from.y) * k,
            )
        } else {
            let k = 1.0 - EASE_IN_OUT.ease((t - self.turn) / (1.0 - self.turn));
            CGPoint::new(self.overshoot.x * k, self.overshoot.y * k)
        }
    }

    pub fn is_home(&self, elapsed: Duration) -> bool {
        elapsed >= self.duration
    }

    /// A push arriving `elapsed` into this bounce: out again from where it is drawn, the way out
    /// shortened by how much of it is already covered, the way home a full one.
    pub fn pushed(&self, elapsed: Duration, overshoot: CGPoint, duration: Duration) -> Self {
        if self.is_home(elapsed) {
            return Bounce::from_rest(overshoot, duration);
        }
        let from = self.at(elapsed);
        let reach = (overshoot.x * overshoot.x + overshoot.y * overshoot.y).sqrt();
        let (dx, dy) = (overshoot.x - from.x, overshoot.y - from.y);
        let left = if reach > 0.0 {
            ((dx * dx + dy * dy).sqrt() / reach).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let out = duration.mul_f64(BOUNCE_TURN * left.max(SHORTEST_PUSH));
        let home = duration.mul_f64(1.0 - BOUNCE_TURN);
        let total = out + home;
        Bounce {
            from,
            overshoot,
            turn: out.as_secs_f64() / total.as_secs_f64(),
            duration: total,
        }
    }

    /// The bounce on `container` from `begin` on the media clock, as the overlay hands it to Core
    /// Animation: one additive keyframe animation, a curve per way.
    pub fn track(&self, container: GroupKey, begin: f64) -> Track {
        let rest = CGPoint::new(0.0, 0.0);
        Track::keyframes(
            Node::Container(container),
            Channel::Bounce,
            vec![
                (0.0, Value::Point(self.from)),
                (self.turn, Value::Point(self.overshoot)),
                (1.0, Value::Point(rest)),
            ],
            vec![Curve::Bezier(MOTION_CURVE), Curve::Bezier(EASE_IN_OUT)],
            begin,
            self.duration.as_secs_f64(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::domain::motion::score::Score;

    const PLAYS: Duration = Duration::from_millis(350);
    const WALL: CGPoint = CGPoint { x: -72.0, y: 0.0 };

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn close(a: CGPoint, b: CGPoint) -> bool {
        (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
    }

    /// Out fast to the wall, home at leisure: the shape the bounce always had.
    #[test]
    fn a_first_push_goes_out_once_and_comes_home() {
        let b = Bounce::from_rest(WALL, PLAYS);
        assert!(close(b.at(Duration::ZERO), CGPoint::new(0.0, 0.0)));
        assert!(close(b.at(PLAYS.mul_f64(BOUNCE_TURN)), WALL));
        assert!(close(b.at(PLAYS), CGPoint::new(0.0, 0.0)));
        let mut previous = 0.0;
        for i in 1..=350 {
            let d = -b.at(ms(i)).x;
            if (i as f64) <= 350.0 * BOUNCE_TURN {
                assert!(d >= previous - 1e-9, "still going out at {i}ms");
            } else {
                assert!(d <= previous + 1e-9, "coming home at {i}ms");
            }
            previous = d;
        }
        assert!(BOUNCE_TURN < 0.5, "out fast, home at leisure");
    }

    /// A push mid-bounce starts the next one from exactly where the last is drawn, so nothing
    /// jumps. Dropping the push instead read as a throttle (reported 2026-10-03).
    #[test]
    fn a_push_starts_where_the_last_is_drawn() {
        let first = Bounce::from_rest(WALL, PLAYS);
        for at in [5, 40, 90, 122, 160, 240, 349] {
            let next = first.pushed(ms(at), WALL, PLAYS);
            assert!(close(next.at(Duration::ZERO), first.at(ms(at))), "{at}ms");
            assert!(
                close(next.at(next.duration.mul_f64(next.turn)), WALL),
                "reaches the wall again, {at}ms"
            );
            assert!(
                close(next.at(next.duration), CGPoint::new(0.0, 0.0)),
                "and comes home, {at}ms"
            );
        }
    }

    /// A burst at the wall keeps the surface there and lets it go one way home after the last press,
    /// never after a backlog of bounces.
    #[test]
    fn a_burst_holds_the_wall_and_comes_home_after_the_last_press() {
        let mut b = Bounce::from_rest(WALL, PLAYS);
        let mut began = Duration::ZERO;
        let mut now = Duration::ZERO;
        for _ in 0..6 {
            now += ms(70);
            b = b.pushed(now - began, WALL, PLAYS);
            began = now;
        }
        for k in 0..=60 {
            assert!(
                -b.at(ms(k)).x > 0.5 * 72.0,
                "held near the wall {k}ms after the last press"
            );
        }
        assert!(b.duration <= PLAYS, "home within one bounce of the last press");
        assert!(b.duration >= PLAYS.mul_f64(1.0 - BOUNCE_TURN));
    }

    /// A push on the way home turns the surface back out.
    #[test]
    fn a_push_on_the_way_home_turns_it_back_out() {
        let first = Bounce::from_rest(WALL, PLAYS);
        let next = first.pushed(ms(250), WALL, PLAYS);
        assert!(-next.at(ms(20)).x > -first.at(ms(250)).x, "going out again");
    }

    #[test]
    fn a_push_after_it_came_home_starts_from_rest() {
        let first = Bounce::from_rest(WALL, PLAYS);
        assert_eq!(first.pushed(PLAYS, WALL, PLAYS), Bounce::from_rest(WALL, PLAYS));
    }

    /// The other end pushed mid-bounce: out toward it from where the surface is.
    #[test]
    fn a_push_the_other_way_starts_from_where_it_is() {
        let first = Bounce::from_rest(WALL, PLAYS);
        let back = CGPoint::new(72.0, 0.0);
        let next = first.pushed(ms(100), back, PLAYS);
        assert!(close(next.at(Duration::ZERO), first.at(ms(100))));
        assert!(close(next.at(next.duration.mul_f64(next.turn)), back));
        assert!(
            (next.turn - BOUNCE_TURN).abs() < 1e-9,
            "the whole way out: it has further to go than one reach"
        );
    }

    fn sampled(bounce: &Bounce, model: CGPoint, begin: f64, elapsed: Duration) -> CGPoint {
        let mut score = Score::default();
        let node = Node::Container(GroupKey::STILL);
        score.place(node, Value::Point(model));
        score.add(bounce.track(GroupKey::STILL, begin));
        match score.sample(node, begin + elapsed.as_secs_f64()) {
            Some(Value::Point(p)) => CGPoint::new(p.x - model.x, p.y - model.y),
            other => panic!("a container holds a point: {other:?}"),
        }
    }

    /// From rest, the track the overlay installs: `0, overshoot, 0` at `0, BOUNCE_TURN, 1`, the
    /// motion curve out and `EASE_IN_OUT` home.
    #[test]
    fn a_bounce_from_rest_is_the_keyframes_the_overlay_installs() {
        let track = Bounce::from_rest(WALL, PLAYS).track(GroupKey::STILL, 7.0);
        let rest = Value::Point(CGPoint::new(0.0, 0.0));
        assert_eq!(
            track.keys(),
            [(0.0, rest), (BOUNCE_TURN, Value::Point(WALL)), (1.0, rest)]
        );
        assert_eq!(
            track.curves(),
            [Curve::Bezier(MOTION_CURVE), Curve::Bezier(EASE_IN_OUT)]
        );
        assert_eq!(track.node(), Node::Container(GroupKey::STILL));
        assert_eq!(track.channel(), Channel::Bounce);
        assert_eq!((track.begin(), track.seconds()), (7.0, 0.35));
    }

    /// The score draws every bounce where `at` has it, on top of the model, from rest or pushed
    /// mid-way, and nothing once it is home.
    #[test]
    fn the_score_draws_a_bounce_where_it_is() {
        let first = Bounce::from_rest(WALL, PLAYS);
        let bounces = [
            first,
            first.pushed(ms(90), WALL, PLAYS),
            first.pushed(ms(250), WALL, PLAYS),
            first.pushed(ms(100), CGPoint::new(72.0, 0.0), PLAYS),
            Bounce::from_rest(CGPoint::new(0.0, 72.0), PLAYS),
        ];
        let model = CGPoint::new(-861.0, 32.0);
        for (k, bounce) in bounces.iter().enumerate() {
            let millis = bounce.duration.as_millis() as u64;
            for at in (0..millis).map(ms).chain([bounce.duration.mul_f64(bounce.turn)]) {
                let drawn = sampled(bounce, model, 1234.5, at);
                assert!(close(drawn, bounce.at(at)), "bounce {k} at {at:?}: {drawn:?}");
            }
            assert_eq!(
                sampled(bounce, model, 1234.5, bounce.duration),
                CGPoint::new(0.0, 0.0),
                "bounce {k} is home"
            );
        }
    }
}
