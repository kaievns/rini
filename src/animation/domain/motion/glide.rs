//! A group's movement as a function of time, so a new destination mid-flight continues the motion
//! from where it is and as fast as it is going. See "One flight however many presses" in
//! `src/animation/docs/animation-smoothness.md`.
use objc2_core_foundation::CGPoint;

use super::easing::{MOTION_CURVE, ease};

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
    /// `from` to `to` along `MOTION_CURVE`, from `begin` for `seconds`.
    Curve {
        from: CGPoint,
        to: CGPoint,
        begin: f64,
        seconds: f64,
    },
    /// A critically damped spring pulling toward `to` at `omega`, let go at `from` moving at
    /// `velocity` points a second.
    Spring {
        from: CGPoint,
        to: CGPoint,
        velocity: CGPoint,
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
    pub fn to(&self) -> CGPoint {
        match *self {
            Leg::Curve { to, .. } | Leg::Spring { to, .. } => to,
        }
    }

    pub fn position_at(&self, now: f64) -> CGPoint {
        match *self {
            Leg::Curve { from, to, begin, seconds } => {
                let p = ease(((now - begin) / seconds.max(1e-9)).clamp(0.0, 1.0));
                CGPoint::new(from.x + (to.x - from.x) * p, from.y + (to.y - from.y) * p)
            }
            Leg::Spring {
                from,
                to,
                velocity,
                begin,
                omega,
            } => {
                let t = (now - begin).max(0.0);
                CGPoint::new(
                    spring_axis(from.x, to.x, velocity.x, omega, t).0,
                    spring_axis(from.y, to.y, velocity.y, omega, t).0,
                )
            }
        }
    }

    /// Points a second, per axis.
    pub fn velocity_at(&self, now: f64) -> CGPoint {
        match *self {
            Leg::Curve { from, to, begin, seconds } => {
                let seconds = seconds.max(1e-9);
                let rate = MOTION_CURVE.slope((now - begin) / seconds) / seconds;
                CGPoint::new((to.x - from.x) * rate, (to.y - from.y) * rate)
            }
            Leg::Spring {
                from,
                to,
                velocity,
                begin,
                omega,
            } => {
                let t = (now - begin).max(0.0);
                CGPoint::new(
                    spring_axis(from.x, to.x, velocity.x, omega, t).1,
                    spring_axis(from.y, to.y, velocity.y, omega, t).1,
                )
            }
        }
    }

    /// The leg that takes over at `now` for a new destination: where this one is, as fast as it is
    /// going, pulled toward `to`.
    pub fn retarget(&self, to: CGPoint, now: f64, omega: f64) -> Leg {
        Leg::Spring {
            from: self.position_at(now),
            to,
            velocity: self.velocity_at(now),
            begin: now,
            omega,
        }
    }

    /// The leg a container takes toward `to` at `now`: its own leg bent there, or, with none, a
    /// spring from its model position at rest. Never from the presented position, which carries
    /// any bounce or nudge riding the container additively; a leg begun there carries it twice.
    pub fn toward(leg: Option<&Leg>, model: CGPoint, to: CGPoint, now: f64, omega: f64) -> Leg {
        match leg {
            Some(leg) => leg.retarget(to, now, omega),
            None => Leg::Spring {
                from: model,
                to,
                velocity: CGPoint::new(0.0, 0.0),
                begin: now,
                omega,
            },
        }
    }

    /// The leg of a container opened mid-flight for a member leaving `source`: from where the
    /// member is drawn, `install`, as fast as `source`'s leg is going at `now` (at rest with none),
    /// pulled toward `to`. A curve there started it from rest, and the curve leaves at 6.25x its
    /// average speed: the column a second move passes left the strip at 12x the speed it had a
    /// frame before.
    pub fn leaving(
        source: Option<&Leg>,
        install: CGPoint,
        to: CGPoint,
        now: f64,
        omega: f64,
    ) -> Leg {
        Leg::Spring {
            from: install,
            to,
            velocity: source.map_or(CGPoint::new(0.0, 0.0), |leg| leg.velocity_at(now)),
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
    pub fn samples(&self, step: f64) -> (Vec<CGPoint>, f64) {
        let begin = match *self {
            Leg::Curve { begin, .. } | Leg::Spring { begin, .. } => begin,
        };
        let steps = (self.seconds() / step).ceil().max(1.0) as usize;
        let mut points: Vec<CGPoint> =
            (0..steps).map(|k| self.position_at(begin + k as f64 * step)).collect();
        points.push(self.to());
        (points, steps as f64 * step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DURATION: f64 = 0.35;
    const COLUMN: f64 = 861.0;

    fn curve(from: f64, to: f64) -> Leg {
        Leg::Curve {
            from: CGPoint::new(from, 0.0),
            to: CGPoint::new(to, 0.0),
            begin: 0.0,
            seconds: DURATION,
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
        let next = first.retarget(CGPoint::new(-2.0 * COLUMN, 0.0), now, spring_omega(DURATION));

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
            leg = leg.retarget(CGPoint::new(-(press as f64) * COLUMN, 0.0), at, omega);
        }
        let end = t + leg.seconds();
        assert!(close(leg.position_at(end).x, -6.0 * COLUMN, SETTLED_PT));
    }

    /// A press after the strip has landed starts from rest, with no jump in speed.
    #[test]
    fn a_new_destination_after_landing_starts_from_rest() {
        let first = curve(0.0, -COLUMN);
        let next = first.retarget(CGPoint::new(-2.0 * COLUMN, 0.0), 1.0, spring_omega(DURATION));
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
        let back = first.retarget(CGPoint::new(0.0, 0.0), now, spring_omega(DURATION));
        assert!(back.velocity_at(now).x < 0.0, "still heading the old way");
        assert!(back.position_at(now + 0.01).x < back.position_at(now).x);
        assert!(close(back.position_at(now + back.seconds()).x, 0.0, SETTLED_PT));
    }

    /// Keyframes start where the leg is and end exactly on its destination.
    #[test]
    fn samples_run_from_the_leg_to_its_destination() {
        let leg = curve(0.0, -COLUMN).retarget(CGPoint::new(-2.0 * COLUMN, 0.0), 0.1, 31.0);
        let (points, seconds) = leg.samples(1.0 / 120.0);
        assert!(close(points[0].x, leg.position_at(0.1).x, 1e-9));
        assert_eq!(points.last().copied(), Some(CGPoint::new(-2.0 * COLUMN, 0.0)));
        assert!(close(seconds, (points.len() - 1) as f64 / 120.0, 1e-9));
    }

    /// A still container a nudge is riding is drawn a third of the display out; the model says where
    /// it is. A leg begun from the drawn position would add the nudge to it a second time.
    #[test]
    fn a_container_with_no_leg_leaves_from_its_model_at_rest() {
        let omega = spring_omega(DURATION);
        let model = CGPoint::new(0.0, 0.0);
        let to = CGPoint::new(-COLUMN, 0.0);
        let leg = Leg::toward(None, model, to, 2.0, omega);
        assert_eq!(leg.position_at(2.0), model);
        assert_eq!(leg.velocity_at(2.0), CGPoint::new(0.0, 0.0));
        assert!(close(
            leg.position_at(2.0 + leg.seconds()).x,
            -COLUMN,
            SETTLED_PT
        ));

        let moving = curve(0.0, -COLUMN);
        assert_eq!(
            Leg::toward(Some(&moving), model, to, 0.1, omega),
            moving.retarget(to, 0.1, omega),
            "a container with a leg bends it"
        );
    }

    /// A member leaving a moving container mid-flight goes on from where it is drawn exactly as
    /// fast as the container it left, and lands where it is going.
    #[test]
    fn a_member_leaving_a_container_keeps_its_speed() {
        let omega = spring_omega(DURATION);
        let strip = curve(0.0, -2.0 * COLUMN);
        let now = 0.12;
        let install = CGPoint::new(strip.position_at(now).x + 36.0, 0.0);
        let to = CGPoint::new(-3.0 * COLUMN, 0.0);
        let leg = Leg::leaving(Some(&strip), install, to, now, omega);
        assert_eq!(leg.position_at(now), install);
        assert!(close(leg.velocity_at(now).x, strip.velocity_at(now).x, 1e-6));
        assert!(strip.velocity_at(now).x.abs() > 1000.0, "it was moving");
        assert!(close(leg.position_at(now + leg.seconds()).x, to.x, SETTLED_PT));

        let still = Leg::leaving(None, install, to, now, omega);
        assert_eq!(
            still.velocity_at(now),
            CGPoint::new(0.0, 0.0),
            "from a still one, at rest"
        );
    }

    /// A chained leg lands in about the time a fresh one takes.
    #[test]
    fn a_spring_lands_in_about_one_flight() {
        let leg = curve(0.0, 0.0).retarget(CGPoint::new(-COLUMN, 0.0), 0.0, spring_omega(DURATION));
        let seconds = leg.seconds();
        assert!(seconds > 0.2 && seconds < 0.4, "{seconds}");
    }
}
