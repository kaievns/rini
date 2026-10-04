//! A group's movement as a function of time, so a new destination mid-flight continues the motion
//! from where it is and as fast as it is going. See "One flight however many presses" in
//! `src/animation/docs/animation-smoothness.md`.
use rini_geometry::space::{OverlayPoint, OverlayVector};

use super::easing::CubicBezier;
use super::plan::GroupKey;
use super::score::{Channel, Curve, Node, Track, Value};

/// Within this of its target and slower than `SETTLED_SPEED`, a spring has landed.
const SETTLED_PT: f64 = 0.5;
/// Points a second. One 120Hz frame at this speed moves a sixth of a point.
const SETTLED_SPEED: f64 = 20.0;

/// The retargeting spring's natural frequency for a flight of `duration` seconds. Critically damped,
/// it is 97% of the way from rest at half the duration, where `MOTION_CURVE` is too, so a chained leg
/// lands like a fresh one: `(1 + wt)e^-wt = 0.03` at `wt = 5.35`.
pub fn spring_omega(duration: f64) -> f64 {
    10.7 / duration.max(0.05)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Leg {
    /// `from` to `to` along `curve`, from `begin` for `seconds`: `MOTION_CURVE`, or `EASE_IN_OUT`
    /// for the strip a move steps.
    Curve {
        from: OverlayPoint,
        to: OverlayPoint,
        begin: f64,
        seconds: f64,
        curve: CubicBezier,
    },
    /// A critically damped spring pulling toward `to` at `omega`, let go at `from` moving at
    /// `velocity` points a second.
    Spring {
        from: OverlayPoint,
        to: OverlayPoint,
        velocity: OverlayVector,
        begin: f64,
        omega: f64,
    },
}

/// One axis of a critically damped spring: `x(t) = to + (a + bt)e^-wt`.
fn spring_axis(from: f64, to: f64, v0: f64, omega: f64, t: f64) -> (f64, f64) {
    let a = from - to;
    let b = v0 + omega * a;
    let decay = (-omega * t).exp();
    let position = to + (a + b * t) * decay;
    let velocity = (b - omega * (a + b * t)) * decay;
    (position, velocity)
}

impl Leg {
    pub fn to(&self) -> OverlayPoint {
        match *self {
            Leg::Curve { to, .. } | Leg::Spring { to, .. } => to,
        }
    }

    pub fn position_at(&self, now: f64) -> OverlayPoint {
        match *self {
            Leg::Curve {
                from,
                to,
                begin,
                seconds,
                curve,
            } => {
                let p = curve.ease(((now - begin) / seconds.max(1e-9)).clamp(0.0, 1.0));
                OverlayPoint::new(from.x + (to.x - from.x) * p, from.y + (to.y - from.y) * p)
            }
            Leg::Spring {
                from,
                to,
                velocity,
                begin,
                omega,
            } => {
                let t = (now - begin).max(0.0);
                OverlayPoint::new(
                    spring_axis(from.x, to.x, velocity.x, omega, t).0,
                    spring_axis(from.y, to.y, velocity.y, omega, t).0,
                )
            }
        }
    }

    /// Points a second, per axis.
    pub fn velocity_at(&self, now: f64) -> OverlayVector {
        match *self {
            Leg::Curve {
                from,
                to,
                begin,
                seconds,
                curve,
            } => {
                let seconds = seconds.max(1e-9);
                let rate = curve.slope((now - begin) / seconds) / seconds;
                OverlayVector::new((to.x - from.x) * rate, (to.y - from.y) * rate)
            }
            Leg::Spring {
                from,
                to,
                velocity,
                begin,
                omega,
            } => {
                let t = (now - begin).max(0.0);
                OverlayVector::new(
                    spring_axis(from.x, to.x, velocity.x, omega, t).1,
                    spring_axis(from.y, to.y, velocity.y, omega, t).1,
                )
            }
        }
    }

    /// The leg a container takes toward `to` at `now`: a spring let go at `from`, where the score
    /// has its movement, as fast as `carried` is going then, at rest with none. `carried` is the
    /// container's own leg when a merge bends it, or the leg of the one a member leaves when a merge
    /// opens a container for it, and lends only its velocity. `from` is never `carried`'s
    /// `position_at`, which a spring is drawn up to 10.8pt off in its first frame
    /// (`a_spring_plays_as_the_polyline_of_its_samples`), nor the drawn position, which would
    /// carry any bounce or nudge riding the container a second time.
    pub fn toward(
        carried: Option<&Leg>,
        from: OverlayPoint,
        to: OverlayPoint,
        now: f64,
        omega: f64,
    ) -> Leg {
        Leg::Spring {
            from,
            to,
            velocity: carried.map_or(OverlayVector::zero(), |leg| leg.velocity_at(now)),
            begin: now,
            omega,
        }
    }

    /// How long the leg runs. A spring runs until it has landed; see `SETTLED_PT`.
    pub fn seconds(&self) -> f64 {
        match *self {
            Leg::Curve { seconds, .. } => seconds,
            Leg::Spring { begin, omega, .. } => {
                let limit = 30.0 / omega;
                let step = 1.0 / 1000.0;
                let mut t = 0.0;
                while t < limit {
                    let (p, v) = (self.position_at(begin + t), self.velocity_at(begin + t));
                    let to = self.to();
                    if (p.x - to.x).abs() <= SETTLED_PT
                        && (p.y - to.y).abs() <= SETTLED_PT
                        && v.x.abs() <= SETTLED_SPEED
                        && v.y.abs() <= SETTLED_SPEED
                    {
                        return t;
                    }
                    t += step;
                }
                limit
            }
        }
    }

    /// Where the leg is every `step` seconds from its begin, ending exactly at its destination, and
    /// the time that covers: what Core Animation is handed as evenly spaced keyframes.
    pub fn samples(&self, step: f64) -> (Vec<OverlayPoint>, f64) {
        let begin = match *self {
            Leg::Curve { begin, .. } | Leg::Spring { begin, .. } => begin,
        };
        let steps = (self.seconds() / step).ceil().max(1.0) as usize;
        let mut points: Vec<OverlayPoint> =
            (0..steps).map(|k| self.position_at(begin + k as f64 * step)).collect();
        points.push(self.to());
        (points, steps as f64 * step)
    }

    /// The leg on `container` as the overlay hands it to Core Animation: a curve as one, a spring
    /// as its `samples` every `step`, which Core Animation joins with straight lines.
    pub fn track(&self, container: GroupKey, step: f64) -> Track {
        let node = Node::Container(container);
        match *self {
            Leg::Curve {
                from,
                to,
                begin,
                seconds,
                curve,
            } => Track::between(
                node,
                Channel::Base,
                Value::Point(from),
                Value::Point(to),
                begin,
                seconds,
                Curve::Bezier(curve),
            ),
            Leg::Spring { begin, .. } => {
                let (points, seconds) = self.samples(step);
                let values = points.into_iter().map(Value::Point);
                Track::samples(node, Channel::Base, values, begin, seconds)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::domain::motion::easing::{EASE_IN_OUT, MOTION_CURVE};
    use crate::animation::domain::motion::score::{Edit, Score};

    const DURATION: f64 = 0.35;
    const COLUMN: f64 = 861.0;

    fn curve(from: f64, to: f64) -> Leg {
        Leg::Curve {
            from: OverlayPoint::new(from, 0.0),
            to: OverlayPoint::new(to, 0.0),
            begin: 0.0,
            seconds: DURATION,
            curve: MOTION_CURVE,
        }
    }

    fn close(a: f64, b: f64, within: f64) -> bool {
        (a - b).abs() <= within
    }

    /// The reported case: each press restarted the curve, which leaves at 6.25x its average speed.
    /// 150ms into a column's travel the strip moves at about 890pt/s, and the restart put it at
    /// 15,375pt/s in one frame. The next leg starts where the last one is and exactly as fast.
    #[test]
    fn a_new_destination_continues_the_motion_without_a_kick() {
        let first = curve(0.0, -COLUMN);
        let now = 0.15;
        let next = Leg::toward(
            Some(&first),
            first.position_at(now),
            OverlayPoint::new(-2.0 * COLUMN, 0.0),
            now,
            spring_omega(DURATION),
        );

        assert!(close(next.position_at(now).x, first.position_at(now).x, 1e-6));
        assert!(close(next.velocity_at(now).x, first.velocity_at(now).x, 1e-6));

        let restart_speed = -COLUMN * MOTION_CURVE.slope(0.0) / DURATION;
        assert!(
            restart_speed.abs() > 15.0 * first.velocity_at(now).x.abs(),
            "restarting the curve here would be a {restart_speed:.0}pt/s kick"
        );
    }

    /// Presses in one direction every 150ms: the strip never turns back and never stops between
    /// presses, then lands on the last destination.
    #[test]
    fn a_burst_in_one_direction_is_one_movement() {
        let omega = spring_omega(DURATION);
        let mut leg = curve(0.0, -COLUMN);
        let mut last = 0.0;
        let mut t = 0.0;
        for press in 2..=6 {
            let at = 0.15 * (press - 1) as f64;
            while t < at {
                let x = leg.position_at(t).x;
                assert!(x <= last + 1e-6, "turned back at {t:.3}s");
                last = x;
                t += 0.004;
            }
            assert!(leg.velocity_at(at).x < -100.0, "nearly stopped at press {press}");
            let to = OverlayPoint::new(-(press as f64) * COLUMN, 0.0);
            leg = Leg::toward(Some(&leg), leg.position_at(at), to, at, omega);
        }
        let end = t + leg.seconds();
        assert!(close(leg.position_at(end).x, -6.0 * COLUMN, SETTLED_PT));
    }

    /// A press after the strip has landed starts from rest, with no jump in speed.
    #[test]
    fn a_new_destination_after_landing_starts_from_rest() {
        let first = curve(0.0, -COLUMN);
        let to = OverlayPoint::new(-2.0 * COLUMN, 0.0);
        let next = Leg::toward(
            Some(&first),
            first.position_at(1.0),
            to,
            1.0,
            spring_omega(DURATION),
        );
        assert_eq!(next.velocity_at(1.0).x, 0.0);
        assert!(
            next.position_at(1.0 + DURATION / 2.0).x < -1.9 * COLUMN,
            "and lands as briskly"
        );
    }

    /// A reversal mid-flight carries on the way it was going for a moment, then comes back: the
    /// speed changes smoothly, it does not flip.
    #[test]
    fn a_reversal_turns_smoothly() {
        let first = curve(0.0, -COLUMN);
        let now = 0.05;
        let home = OverlayPoint::new(0.0, 0.0);
        let back = Leg::toward(
            Some(&first),
            first.position_at(now),
            home,
            now,
            spring_omega(DURATION),
        );
        assert!(back.velocity_at(now).x < 0.0, "still heading the old way");
        assert!(back.position_at(now + 0.01).x < back.position_at(now).x);
        assert!(close(back.position_at(now + back.seconds()).x, 0.0, SETTLED_PT));
    }

    /// Keyframes start where the leg is and end exactly on its destination.
    #[test]
    fn samples_run_from_the_leg_to_its_destination() {
        let first = curve(0.0, -COLUMN);
        let to = OverlayPoint::new(-2.0 * COLUMN, 0.0);
        let leg = Leg::toward(Some(&first), first.position_at(0.1), to, 0.1, 31.0);
        let (points, seconds) = leg.samples(1.0 / 120.0);
        assert!(close(points[0].x, leg.position_at(0.1).x, 1e-9));
        assert_eq!(points.last().copied(), Some(OverlayPoint::new(-2.0 * COLUMN, 0.0)));
        assert!(close(seconds, (points.len() - 1) as f64 / 120.0, 1e-9));
    }

    /// A still container a nudge is riding is drawn a third of the display out; the model says where
    /// it is. A leg begun from the drawn position would add the nudge to it a second time. A moving
    /// one bends from where the score has its movement, which on a spring's polyline is up to 10.8pt
    /// off the leg's own `position_at`.
    #[test]
    fn a_container_with_no_leg_leaves_from_its_model_at_rest() {
        let omega = spring_omega(DURATION);
        let model = OverlayPoint::new(0.0, 0.0);
        let to = OverlayPoint::new(-COLUMN, 0.0);
        let leg = Leg::toward(None, model, to, 2.0, omega);
        assert_eq!(leg.position_at(2.0), model);
        assert_eq!(leg.velocity_at(2.0), OverlayVector::zero());
        assert!(close(
            leg.position_at(2.0 + leg.seconds()).x,
            -COLUMN,
            SETTLED_PT
        ));

        let moving = curve(0.0, -COLUMN);
        let from = OverlayPoint::new(moving.position_at(0.1).x + 5.4, 0.0);
        let bent = Leg::toward(Some(&moving), from, to, 0.1, omega);
        assert_eq!(
            bent.position_at(0.1),
            from,
            "a carried leg lends its velocity and nothing else: the spring starts at `from`"
        );
        assert!(close(bent.velocity_at(0.1).x, moving.velocity_at(0.1).x, 1e-9));
        assert!(moving.velocity_at(0.1).x.abs() > 100.0, "it was moving");
    }

    /// A member leaving a moving container mid-flight goes on from where it is drawn exactly as
    /// fast as the container it left, and lands where it is going.
    #[test]
    fn a_member_leaving_a_container_keeps_its_speed() {
        let omega = spring_omega(DURATION);
        let strip = curve(0.0, -2.0 * COLUMN);
        let now = 0.12;
        let install = OverlayPoint::new(strip.position_at(now).x + 36.0, 0.0);
        let to = OverlayPoint::new(-3.0 * COLUMN, 0.0);
        let leg = Leg::toward(Some(&strip), install, to, now, omega);
        assert_eq!(leg.position_at(now), install);
        assert!(close(leg.velocity_at(now).x, strip.velocity_at(now).x, 1e-6));
        assert!(strip.velocity_at(now).x.abs() > 1000.0, "it was moving");
        assert!(close(leg.position_at(now + leg.seconds()).x, to.x, SETTLED_PT));

        let still = Leg::toward(None, install, to, now, omega);
        assert_eq!(
            still.velocity_at(now),
            OverlayVector::zero(),
            "from a still one, at rest"
        );
    }

    /// A chained leg lands in about the time a fresh one takes.
    #[test]
    fn a_spring_lands_in_about_one_flight() {
        let still = curve(0.0, 0.0);
        let to = OverlayPoint::new(-COLUMN, 0.0);
        let leg = Leg::toward(
            Some(&still),
            still.position_at(0.0),
            to,
            0.0,
            spring_omega(DURATION),
        );
        let seconds = leg.seconds();
        assert!(seconds > 0.2 && seconds < 0.4, "{seconds}");
    }

    /// The strip a move steps crosses on the ease-in-out, and its leg says where it is and how fast
    /// it goes on that curve: at rest at both ends, the rate the derivative of the position. A
    /// press arriving mid-crossing bends it from there, and a column leaving it mid-crossing leaves
    /// as fast as it was going.
    #[test]
    fn a_leg_on_the_ease_in_out_bends_without_a_kick() {
        let seconds = DURATION * crate::animation::domain::motion::nudge::NUDGE_STRETCH;
        let crossing = Leg::Curve {
            from: OverlayPoint::new(0.0, 0.0),
            to: OverlayPoint::new(-862.0, 0.0),
            begin: 0.0,
            seconds,
            curve: EASE_IN_OUT,
        };
        assert_eq!(crossing.velocity_at(0.0).x, 0.0, "leaves at rest");
        assert!(
            crossing.velocity_at(seconds * 0.999).x.abs() < 10.0,
            "lands at rest"
        );
        assert!(
            close(crossing.position_at(seconds / 2.0).x, -431.0, 1e-6),
            "half way at half time"
        );
        for now in [0.03, 0.1, 0.2, 0.3, 0.4] {
            let h = 1e-5;
            let numeric =
                (crossing.position_at(now + h).x - crossing.position_at(now - h).x) / (2.0 * h);
            assert!(close(crossing.velocity_at(now).x, numeric, 0.5), "t={now}");
            let p = crossing.position_at(now).x / -862.0;
            assert!(
                close(p, EASE_IN_OUT.ease(now / seconds), 1e-9),
                "on its own curve at {now}"
            );
        }

        let omega = spring_omega(seconds);
        let now = 0.15;
        let bent = Leg::toward(
            Some(&crossing),
            crossing.position_at(now),
            OverlayPoint::new(-2013.0, 0.0),
            now,
            omega,
        );
        assert!(close(bent.position_at(now).x, crossing.position_at(now).x, 1e-6));
        assert!(close(bent.velocity_at(now).x, crossing.velocity_at(now).x, 1e-6));
        assert!(
            crossing.velocity_at(now).x < -1000.0,
            "it was crossing: {:?}",
            crossing.velocity_at(now)
        );
        let install = OverlayPoint::new(crossing.position_at(now).x - 576.0, 0.0);
        let to = OverlayPoint::new(-1723.0, 0.0);
        let left = Leg::toward(Some(&crossing), install, to, now, omega);
        assert_eq!(left.position_at(now), install);
        assert!(close(left.velocity_at(now).x, crossing.velocity_at(now).x, 1e-6));
    }

    const STEP: f64 = 1.0 / 120.0;

    fn drawn(leg: &Leg, at: f64) -> f64 {
        let mut score = Score::default();
        let node = Node::Container(GroupKey::STILL);
        score.apply(&Edit::Place(node, Value::Point(leg.to())));
        score.apply(&Edit::Add(leg.track(GroupKey::STILL, STEP)));
        match score.sample(node, at) {
            Some(Value::Point(p)) => p.x,
            other => panic!("a container holds a point: {other:?}"),
        }
    }

    /// A leg on one curve plays as that curve, so the score draws it where the leg has it.
    #[test]
    fn a_curve_leg_plays_as_its_curve() {
        let leg = curve(0.0, -COLUMN);
        let track = leg.track(GroupKey::STILL, STEP);
        assert_eq!(track.curves(), [Curve::Bezier(MOTION_CURVE)]);
        assert_eq!(
            track.keys(),
            [
                (0.0, Value::Point(OverlayPoint::new(0.0, 0.0))),
                (1.0, Value::Point(OverlayPoint::new(-COLUMN, 0.0)))
            ]
        );
        for step in 0..350 {
            let at = step as f64 / 1000.0;
            assert!(close(drawn(&leg, at), leg.position_at(at).x, 1e-9), "{at}");
        }
    }

    /// A spring is handed over as its samples and Core Animation draws straight lines between
    /// them, so the score draws that polyline: exactly the samples at the key times, and on the
    /// line between them. It is not `position_at`: from rest, the spring leaves slowly and the
    /// first line cuts the corner, 10.8pt off at 1722pt and 5.4pt at 861pt in the first 1/120s,
    /// and less on every frame after.
    #[test]
    fn a_spring_plays_as_the_polyline_of_its_samples() {
        let omega = spring_omega(DURATION);
        for (reach, first_frame) in [(2.0 * COLUMN, 10.8), (COLUMN, 5.4)] {
            let begin = 2.0;
            let rest = OverlayPoint::new(0.0, 0.0);
            let leg = Leg::toward(None, rest, OverlayPoint::new(reach, 0.0), begin, omega);
            let (points, seconds) = leg.samples(STEP);
            let track = leg.track(GroupKey::STILL, STEP);
            assert_eq!((track.begin(), track.seconds()), (begin, seconds));
            assert!(track.curves().iter().all(|c| *c == Curve::Linear));

            let mut worst = vec![0.0f64; points.len() - 1];
            for k in 0..points.len() - 1 {
                for f in 0..20 {
                    let f = f as f64 / 20.0;
                    let at = begin + (k as f64 + f) * STEP;
                    let line = points[k].x + (points[k + 1].x - points[k].x) * f;
                    let x = drawn(&leg, at);
                    assert!(close(x, line, 1e-9), "{reach}pt, frame {k} + {f}");
                    worst[k] = worst[k].max((x - leg.position_at(at).x).abs());
                }
                if k + 1 < points.len() - 1 {
                    let at = begin + k as f64 * STEP;
                    assert!(close(drawn(&leg, at), leg.position_at(at).x, 1e-9), "key {k}");
                }
            }
            assert!(
                close(worst[0], first_frame, 0.05),
                "{reach}pt: {:.3}pt in the first frame",
                worst[0]
            );
            assert!(worst[1..].iter().all(|w| *w < worst[0]), "{reach}pt: {worst:?}");
        }
    }
}
