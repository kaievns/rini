//! What a trackpad gesture amounts to, decided over normalised touch positions.
//!
//! `crate::input::platform::gesture_tap` reads the touches off an `NSEvent` and owns the state
//! machine; every rule it applies to them is here, so the rules can be tested without a trackpad.
//! Positions are normalised to the pad: 0.0 to 1.0 on each axis, so a distance is a fraction of the
//! pad's width rather than a number of points.

/// A tolerance the user may write either way round: `0.4` and `40` both mean four tenths.
///
/// Above 100 is taken as "no limit" rather than an error, because the setting reads as a percentage
/// and a typo there should not make every gesture fail to arm.
pub fn normalized_fraction(raw: f64) -> f64 {
    if raw > 1.0 && raw <= 100.0 {
        (raw / 100.0).clamp(0.0, 1.0)
    } else if raw > 100.0 {
        1.0
    } else {
        raw.clamp(0.0, 1.0)
    }
}

/// Where the fingers are, when this frame is the gesture rini is watching for.
///
/// `None` means the frame is not: a different number of fingers is down, or every one of them has
/// lifted. The caller treats that as the end of the gesture.
pub fn touch_centroid(
    total: usize,
    active: usize,
    sum: (f64, f64),
    wanted: usize,
) -> Option<(f64, f64)> {
    if total != wanted || active == 0 {
        return None;
    }
    Some((sum.0 / active as f64, sum.1 / active as f64))
}

/// Which way a committed swipe went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeToward {
    Next,
    Prev,
}

/// What a swipe's travel from its start amounts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwipeStep {
    /// Swallow the event at the tap: this gesture is rini's.
    pub consuming: bool,
    /// Far enough to act on.
    pub commit: Option<SwipeToward>,
}

/// A swipe is judged against where it began, so a slow drag commits at the same distance as a flick.
///
/// `consuming` and `commit` are decided separately, and a swipe travelling exactly as far vertically
/// as horizontally can commit without ever being consumed: the consuming test is strict (`>`) and the
/// committing test is not. That is the behaviour the tap has always had.
pub fn swipe_step(delta: (f64, f64), tolerance: f64, distance: f64, invert: bool) -> SwipeStep {
    let (horizontal, vertical) = (delta.0.abs(), delta.1.abs());
    let within_tolerance = vertical <= tolerance;
    let mut toward = if delta.0 < 0.0 { SwipeToward::Next } else { SwipeToward::Prev };
    if invert {
        toward = match toward {
            SwipeToward::Next => SwipeToward::Prev,
            SwipeToward::Prev => SwipeToward::Next,
        };
    }
    SwipeStep {
        consuming: horizontal > vertical && within_tolerance,
        commit: (horizontal >= distance && within_tolerance).then_some(toward),
    }
}

/// What one frame of a horizontal scroll gesture amounts to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollStep {
    /// Too vertical to be a strip scroll. The accumulator and the consuming flag are left alone, so a
    /// gesture that wanders off-axis for a frame resumes rather than restarting.
    OffAxis,
    /// Horizontal enough to own, not yet far enough to move anything.
    Accumulating { accumulated: f64 },
    /// Past the step: move the strip by this much and start accumulating again.
    Scroll { delta: f64 },
}

/// A scroll is judged frame by frame against the last one, not against the start: the gesture is
/// continuous, so what matters is how far the fingers moved since rini last looked.
pub fn scroll_step(
    delta: (f64, f64),
    accumulated: f64,
    tolerance: f64,
    step: f64,
    invert: bool,
) -> ScrollStep {
    let (horizontal, vertical) = (delta.0.abs(), delta.1.abs());
    if vertical > tolerance || vertical >= horizontal {
        return ScrollStep::OffAxis;
    }
    let accumulated = accumulated + delta.0;
    if accumulated.abs() < step {
        return ScrollStep::Accumulating { accumulated };
    }
    ScrollStep::Scroll { delta: if invert { -accumulated } else { accumulated } }
}

#[cfg(test)]
mod tests {
    use rini_geometry::IsWithin;

    use super::*;

    #[test]
    fn a_tolerance_reads_the_same_as_a_fraction_or_as_a_percentage() {
        assert_eq!(normalized_fraction(0.4), 0.4);
        assert_eq!(normalized_fraction(40.0), 0.4);
        assert_eq!(normalized_fraction(0.0), 0.0);
        assert_eq!(normalized_fraction(1.0), 1.0, "1.0 is a whole pad, not one percent");
        assert_eq!(normalized_fraction(100.0), 1.0);
    }

    #[test]
    fn a_tolerance_outside_the_range_becomes_the_nearest_end() {
        assert_eq!(normalized_fraction(1000.0), 1.0);
        assert_eq!(normalized_fraction(-5.0), 0.0);
    }

    #[test]
    fn the_centroid_averages_only_the_fingers_still_down() {
        assert_eq!(touch_centroid(3, 2, (1.0, 0.6), 3), Some((0.5, 0.3)));
    }

    #[test]
    fn a_frame_with_the_wrong_number_of_fingers_is_not_the_gesture() {
        assert_eq!(touch_centroid(2, 2, (1.0, 1.0), 3), None);
        assert_eq!(touch_centroid(4, 4, (1.0, 1.0), 3), None);
        assert_eq!(touch_centroid(3, 0, (0.0, 0.0), 3), None, "every finger lifted");
    }

    #[test]
    fn a_swipe_past_the_distance_commits_toward_the_direction_it_travelled() {
        let far = swipe_step((-0.3, 0.01), 0.1, 0.2, false);
        assert_eq!(far.commit, Some(SwipeToward::Next));
        assert!(far.consuming);
        let back = swipe_step((0.3, 0.01), 0.1, 0.2, false);
        assert_eq!(back.commit, Some(SwipeToward::Prev));
    }

    #[test]
    fn inverting_swaps_which_way_a_swipe_means() {
        assert_eq!(swipe_step((-0.3, 0.0), 0.1, 0.2, true).commit, Some(SwipeToward::Prev));
        assert_eq!(swipe_step((0.3, 0.0), 0.1, 0.2, true).commit, Some(SwipeToward::Next));
    }

    #[test]
    fn a_swipe_too_far_off_axis_neither_consumes_nor_commits() {
        let step = swipe_step((0.3, 0.2), 0.1, 0.2, false);
        assert!(!step.consuming);
        assert_eq!(step.commit, None);
    }

    #[test]
    fn a_swipe_short_of_the_distance_is_consumed_without_committing() {
        let step = swipe_step((0.1, 0.01), 0.1, 0.2, false);
        assert!(step.consuming);
        assert_eq!(step.commit, None);
    }

    // The two tests are separate and one is strict: a swipe moving exactly as far down as across is
    // not consumed, but will still commit once it is far enough. Pinned rather than corrected,
    // because correcting it changes what the trackpad does.
    #[test]
    fn an_exactly_diagonal_swipe_commits_without_being_consumed() {
        let step = swipe_step((0.3, 0.3), 0.5, 0.2, false);
        assert!(!step.consuming, "the consuming test needs horizontal strictly greater");
        assert_eq!(step.commit, Some(SwipeToward::Prev));
    }

    fn scrolled(step: ScrollStep) -> f64 {
        match step {
            ScrollStep::Scroll { delta } => delta,
            other => panic!("expected a scroll, got {other:?}"),
        }
    }

    #[test]
    fn a_scroll_accumulates_until_it_passes_the_step() {
        assert_eq!(
            scroll_step((0.02, 0.0), 0.0, 0.1, 0.05, false),
            ScrollStep::Accumulating { accumulated: 0.02 }
        );
        // The whole accumulator goes out, not just the step: a fast frame must not be truncated.
        assert!(scrolled(scroll_step((0.02, 0.0), 0.04, 0.1, 0.05, false)).is_within(1e-9, 0.06));
    }

    #[test]
    fn a_scroll_carries_its_accumulator_through_an_off_axis_frame() {
        assert_eq!(scroll_step((0.01, 0.9), 0.04, 0.1, 0.05, false), ScrollStep::OffAxis);
        assert!(
            scrolled(scroll_step((0.02, 0.0), 0.04, 0.1, 0.05, false)).is_within(1e-9, 0.06),
            "the frame that was dropped did not reset the accumulator"
        );
    }

    #[test]
    fn a_scroll_no_more_horizontal_than_vertical_is_off_axis() {
        assert_eq!(scroll_step((0.2, 0.2), 0.0, 0.5, 0.05, false), ScrollStep::OffAxis);
        assert!(matches!(
            scroll_step((0.2, 0.19), 0.0, 0.5, 0.05, false),
            ScrollStep::Scroll { .. }
        ));
    }

    #[test]
    fn inverting_flips_the_delta_a_scroll_moves_the_strip_by() {
        assert_eq!(
            scroll_step((0.2, 0.0), 0.0, 0.5, 0.05, true),
            ScrollStep::Scroll { delta: -0.2 }
        );
    }
}
