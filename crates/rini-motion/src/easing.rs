//! The one curve every movement runs on, and the shape of an edge bounce. Core Animation takes
//! the same control points, so the clock and the render server agree.

/// The one curve every movement runs on, as CSS-style cubic Bezier control points `(x1, y1, x2, y2)`.
///
/// An exponential ease-out: off the line at once and 97% of the way there by half time, so the
/// motion reads as finished well inside `animation_duration` and the tail is a settle, not a crawl.
/// Ease-out cubic (`(1/3, 1, 2/3, 1)`) was tried first and felt sluggish at the same duration:
/// it spends the whole second half of the flight on the last 12.5% of the distance. Derivation and
/// the numbers in "The curve" in `docs/animation-smoothness.md`.
pub const MOTION_CURVE: CubicBezier = CubicBezier { x1: 0.16, y1: 1.0, x2: 0.3, y2: 1.0 };

/// A CSS-style cubic Bezier timing curve from `(0,0)` to `(1,1)`, evaluated as progress in terms
/// of time. Core Animation takes the same four numbers (`CAMediaTimingFunction`), so what the
/// actor's clock computes and what the render server draws are one curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CubicBezier {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl CubicBezier {
    fn coordinate(s: f64, p1: f64, p2: f64) -> f64 {
        let inv = 1.0 - s;
        3.0 * inv * inv * s * p1 + 3.0 * inv * s * s * p2 + s * s * s
    }

    /// `(x, y)` at parameter `s`.
    pub fn at(&self, s: f64) -> (f64, f64) {
        (Self::coordinate(s, self.x1, self.x2), Self::coordinate(s, self.y1, self.y2))
    }

    /// Progress at time `t` in `[0, 1]`: the `y` where the curve's `x` is `t`. Newton's method from
    /// `s = t`, bisection when it strays; both converge fast because `x(s)` is monotone for control
    /// x's inside `[0, 1]`.
    pub fn ease(&self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        if t == 0.0 || t == 1.0 {
            return t;
        }
        let x = |s: f64| Self::coordinate(s, self.x1, self.x2);
        let dx = |s: f64| {
            let inv = 1.0 - s;
            3.0 * inv * inv * self.x1 + 6.0 * inv * s * (self.x2 - self.x1) + 3.0 * s * s * (1.0 - self.x2)
        };
        let mut s = t;
        for _ in 0..8 {
            let error = x(s) - t;
            if error.abs() < 1e-7 {
                return Self::coordinate(s, self.y1, self.y2);
            }
            let slope = dx(s);
            if slope.abs() < 1e-6 {
                break;
            }
            s -= error / slope;
            if !(0.0..=1.0).contains(&s) {
                break;
            }
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..64 {
            s = (lo + hi) / 2.0;
            if x(s) < t { lo = s } else { hi = s }
            if hi - lo < 1e-9 {
                break;
            }
        }
        Self::coordinate(s, self.y1, self.y2)
    }
}

/// Progress at time `t` on the motion curve: what the actor's clock uses for the apply point and
/// what the AX engine interpolates with, so both engines and the render server agree.
pub fn ease(t: f64) -> f64 {
    MOTION_CURVE.ease(t)
}

/// Where the return leg of a bounce begins, as a fraction of its duration. Out fast, back at
/// leisure: a rubber band snaps taut and eases home.
pub const BOUNCE_TURN: f64 = 0.35;

/// The displacement of a bounce at progress `t`, for a unit overshoot: out along an ease-out to
/// the turn, back along an ease-in-out to rest. What `bounce_animation` asks Core Animation to
/// draw, kept here so the shape can be checked on plain numbers.
pub fn bounce_displacement(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t <= BOUNCE_TURN {
        ease(t / BOUNCE_TURN)
    } else {
        let u = (t - BOUNCE_TURN) / (1.0 - BOUNCE_TURN);
        // ease-in-out cubic, from 1 down to 0
        let s = if u < 0.5 { 4.0 * u * u * u } else { 1.0 - (-2.0 * u + 2.0).powi(3) / 2.0 };
        1.0 - s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_is_pinned_at_both_ends() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
    }

    /// Why this curve: the motion is nearly done by half time, so the flight reads as finished
    /// inside its duration and the rest is a settle. Ease-out cubic reached 87.5% at half time and
    /// crawled through the last 12.5% for the whole second half, which read as sluggish at 350ms.
    #[test]
    fn the_motion_is_nearly_home_by_half_time() {
        assert!(ease(0.5) > 0.95, "{}", ease(0.5));
        assert!(ease(0.25) > 0.8, "{}", ease(0.25));
        assert!(ease(0.7) > 0.99, "{}", ease(0.7));
        assert!(ease(0.1) < 0.6, "not a cut: {}", ease(0.1));
    }

    #[test]
    fn easing_is_monotonic() {
        // A non-monotonic easing curve makes windows visibly step backwards mid-slide.
        let mut previous = -1.0;
        for i in 0..=100 {
            let value = ease(i as f64 / 100.0);
            assert!(value >= previous, "easing went backwards at t = {}", i);
            previous = value;
        }
    }

    #[test]
    fn easing_clamps_out_of_range_input() {
        // A time-based driver can hand over t slightly outside 0..1 when a frame is late, and an
        // unclamped cubic would overshoot the target position.
        assert_eq!(ease(-0.5), 0.0);
        assert_eq!(ease(1.5), 1.0);
    }

    /// A bounce leaves rest, peaks at the turn, and is back at rest at the end, with no
    /// second hump: out is monotone up to the turn, back is monotone down after it. The turn is
    /// early, so the snap out is quicker than the settle home.
    #[test]
    fn a_bounce_goes_out_once_and_comes_home() {
        assert_eq!(bounce_displacement(0.0), 0.0);
        assert!((bounce_displacement(BOUNCE_TURN) - 1.0).abs() < 1e-12);
        assert!(bounce_displacement(1.0).abs() < 1e-12);
        let mut previous = 0.0;
        for i in 1..=1000 {
            let t = i as f64 / 1000.0;
            let d = bounce_displacement(t);
            if t <= BOUNCE_TURN {
                assert!(d >= previous, "still going out at t={t}");
            } else {
                assert!(d <= previous + 1e-12, "coming back at t={t}");
            }
            previous = d;
        }
        assert!(BOUNCE_TURN < 0.5, "out fast, home at leisure");
    }
}
