//! A move's nudge: the whole strip stepping a share of the viewport away from where a moved window
//! went and back, and the curve a flight carrying one flies on. When a move steps is the look's
//! (`crate::animation::domain::choreography::slide::stage`). See "The move flight" in
//! `src/animation/docs/animation-smoothness.md`.

use objc2_core_foundation::CGPoint;

use super::easing::{CubicBezier, EASE_IN_OUT, MOTION_CURVE, nudge_displacement};
use super::plan::GroupKey;
use super::score::{Channel, Node, Track, Value};

/// How far the strip steps away from where a moved window went, as a share of the viewport's width.
pub const NUDGE_SHARE: f64 = 1.0 / 3.0;

/// How much longer than a plain flight a flight carrying a nudge runs, so the nudge's peak speed
/// stays moderate.
pub const NUDGE_STRETCH: f64 = 1.3;

/// The curve a flight's containers fly on: the ease-in-out while the strip steps, so the column the
/// move passed lingers in the side the step opens; `MOTION_CURVE` otherwise.
pub fn flight_curve(stepping: bool) -> CubicBezier {
    if stepping { EASE_IN_OUT } else { MOTION_CURVE }
}

/// The nudge as evenly spaced keyframes about `step` seconds apart over `seconds`, from rest to rest.
pub fn nudge_samples(offset: CGPoint, seconds: f64, step: f64) -> Vec<CGPoint> {
    let steps = (seconds / step).ceil().max(1.0) as usize;
    (0..=steps)
        .map(|k| {
            let d = nudge_displacement(k as f64 / steps as f64);
            CGPoint::new(offset.x * d, offset.y * d)
        })
        .collect()
}

/// The nudge on `container` from `begin` on the media clock, as the overlay hands it to Core
/// Animation: `nudge_samples` evenly spaced over `seconds`, additive.
pub fn nudge_track(
    container: GroupKey,
    offset: CGPoint,
    begin: f64,
    seconds: f64,
    step: f64,
) -> Track {
    let samples = nudge_samples(offset, seconds, step).into_iter().map(Value::Point);
    Track::samples(
        Node::Container(container),
        Channel::Nudge,
        samples,
        begin,
        seconds,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::domain::motion::easing::nudge_rate;

    /// A flight carrying the nudge crosses on the ease-in-out; one without it keeps the motion curve.
    #[test]
    fn a_flight_crosses_on_the_ease_in_out_only_while_the_strip_steps() {
        assert_eq!(flight_curve(true), EASE_IN_OUT);
        assert_eq!(flight_curve(false), MOTION_CURVE);
    }

    #[test]
    fn the_nudge_keyframes_run_rest_to_rest_through_the_offset() {
        let offset = CGPoint::new(576.0, 0.0);
        let samples = nudge_samples(offset, 0.455, 1.0 / 120.0);
        assert_eq!(samples.first(), Some(&CGPoint::new(0.0, 0.0)));
        assert!(samples.last().expect("some").x.abs() < 1e-9);
        let furthest = samples.iter().map(|p| p.x).fold(0.0, f64::max);
        assert!((furthest - 576.0).abs() < 1.0, "{furthest}");
        assert!(samples.iter().all(|p| p.y == 0.0 && p.x >= 0.0));
    }

    /// The numbers "The move flight" quotes: a third of the laptop's 1720pt and of the 3008pt
    /// external, over the stretched default flight, peak well under the 28,338pt/s already reported
    /// as a jerk and near the edge bounce's own launch.
    #[test]
    fn the_nudge_peaks_at_a_moderate_speed() {
        let seconds = 0.35 * NUDGE_STRETCH;
        let peak = |offset: f64| offset * nudge_rate(0.25) / seconds;
        assert!((peak(573.0) - 3956.0).abs() < 1.0, "{}", peak(573.0));
        assert!((peak(1003.0) - 6925.0).abs() < 1.0, "{}", peak(1003.0));
        for i in 0..=1000 {
            let t = i as f64 / 1000.0;
            assert!(1003.0 * nudge_rate(t).abs() / seconds <= peak(1003.0) + 1e-6);
        }
    }

    /// The nudge plays as its samples, evenly spaced and joined by straight lines, on top of the
    /// container's model: exactly `nudge_samples` at the key times, and within half a point of
    /// `sin²(πt)` between them at a third of a 1720pt viewport.
    #[test]
    fn the_nudge_plays_as_its_samples() {
        use crate::animation::domain::motion::score::{Curve, Edit, Score};

        let (offset, begin, seconds, step) = (
            CGPoint::new(-1720.0 * NUDGE_SHARE, 0.0),
            3.0,
            0.35 * NUDGE_STRETCH,
            1.0 / 120.0,
        );
        let samples = nudge_samples(offset, seconds, step);
        let track = nudge_track(GroupKey::STILL, offset, begin, seconds, step);
        assert_eq!(track.channel(), Channel::Nudge);
        assert_eq!((track.begin(), track.seconds()), (begin, seconds));
        assert!(track.curves().iter().all(|c| *c == Curve::Linear));
        let keys: Vec<Value> = track.keys().iter().map(|(_, v)| *v).collect();
        assert_eq!(
            keys,
            samples.iter().copied().map(Value::Point).collect::<Vec<_>>()
        );

        let node = Node::Container(GroupKey::STILL);
        let model = CGPoint::new(-861.0, 0.0);
        let mut score = Score::default();
        score.apply(&Edit::Place(node, Value::Point(model)));
        score.apply(&Edit::Add(track));
        let drawn = |at: f64| match score.sample(node, at) {
            Some(Value::Point(p)) => p.x - model.x,
            other => panic!("a container holds a point: {other:?}"),
        };
        let gaps = samples.len() - 1;
        let mut furthest = 0.0f64;
        for k in 0..gaps {
            let at = begin + seconds * k as f64 / gaps as f64;
            assert!((drawn(at) - samples[k].x).abs() < 1e-9, "key {k}");
            let middle = begin + seconds * (k as f64 + 0.5) / gaps as f64;
            let line = (samples[k].x + samples[k + 1].x) / 2.0;
            assert!((drawn(middle) - line).abs() < 1e-9, "between {k} and {}", k + 1);
            let t = (k as f64 + 0.5) / gaps as f64;
            furthest = furthest.max((drawn(middle) - offset.x * nudge_displacement(t)).abs());
        }
        assert!(furthest > 0.3 && furthest < 0.5, "{furthest}");
        assert_eq!(drawn(begin + seconds), 0.0, "home");
    }
}
