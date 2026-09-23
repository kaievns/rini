use crate::layout::WindowLayoutConstraints;

#[derive(Debug, Clone, Copy, Default)]
pub struct AxisConstraints {
    pub min: f64,
    pub fixed: Option<f64>,
    pub max: Option<f64>,
    pub weight: f64,
    pub can_grow: bool,
}

fn sanitize(v: f64) -> f64 {
    if v.is_finite() { v.max(0.0) } else { 0.0 }
}

/// The horizontal limits a column's windows impose TOGETHER.
///
/// A column is one width, so the windows in it have to agree on one. They agree pessimistically: the
/// largest minimum, the largest lock, and the smallest maximum. A column holding a window that cannot
/// go below 400pt and one that cannot go above 600pt is between 400 and 600, and a column whose
/// windows lock to different widths takes the larger, because the smaller one can be clipped but the
/// larger one cannot be conjured.
pub fn column_limits(
    windows: impl IntoIterator<Item = WindowLayoutConstraints>,
) -> AxisConstraints {
    let mut limits = AxisConstraints {
        min: 1.0,
        ..AxisConstraints::default()
    };
    for window in windows {
        let window = window.normalized();
        limits.min = limits.min.max(window.min_for_axis(true));
        if let Some(locked) = window.fixed_for_axis(true) {
            limits.fixed = Some(limits.fixed.map_or(locked, |current: f64| current.max(locked)));
        }
        let max = window.max_for_axis(true);
        if max > 0.0 {
            limits.max = Some(limits.max.map_or(max, |current: f64| current.min(max)));
        }
    }
    limits
}

/// How wide one column of the strip is.
///
/// `ratio` of the viewport, then widened to whatever the windows in it require, then narrowed to what
/// they accept — in that order, because a minimum beats a maximum. A column whose windows demand more
/// than they allow is given the minimum: a window clipped at the edge is recoverable, a window too
/// small to use is not.
///
/// Clamped to the viewport last of all. This layout scrolls BETWEEN column starts and does not pan
/// within one column, so a column wider than the screen has a region nothing can ever scroll to.
///
/// The gap comes out of the column rather than out of the space between columns, which is what keeps
/// two columns at ratio 0.5 adding up to the viewport instead of overflowing it by one gap. It is
/// skipped when it would take the column below a pixel.
pub fn column_width(ratio: f64, viewport_width: f64, gap: f64, limits: AxisConstraints) -> f64 {
    let required = limits.fixed.unwrap_or(limits.min).max(limits.min);
    let mut width = (viewport_width * ratio).max(1.0).max(required);
    if let Some(max) = limits.max {
        width = width.min(max).max(required);
    }

    width = width.min(viewport_width.max(1.0));

    let shrunk = width - crate::layout::domain::strip::gap_share(ratio, gap);
    if shrunk >= 1.0 { shrunk } else { width }
}

/// `size` reduced to what the window will actually accept, never grown.
///
/// Applied to a maximized frame as well as an ordinary column slot. It used to apply only to the
/// slot: a maximized window was handed the whole tiling width regardless of its maximum, while the
/// strip reserved the CLAMPED width for its column, so the next column was laid out overlapping it.
/// Both sides read this now, which is what makes them agree.
pub fn clamp_to_constraints(
    size: objc2_core_foundation::CGSize,
    constraints: crate::layout::WindowLayoutConstraints,
) -> objc2_core_foundation::CGSize {
    let c = constraints.normalized();
    let axis = |available: f64, horizontal: bool| {
        let desired = c
            .fixed_for_axis(horizontal)
            .unwrap_or(available)
            .max(c.min_for_axis(horizontal));
        let capped = if c.max_for_axis(horizontal) > 0.0 {
            desired.min(c.max_for_axis(horizontal))
        } else {
            desired
        };
        capped.min(available).max(0.0)
    };
    objc2_core_foundation::CGSize::new(axis(size.width, true), axis(size.height, false))
}

/// Solve 1D segment lengths for a container axis.
///
/// Rules:
/// - never negative
/// - `min` is a lower bound unless physically infeasible (then minima are scaled down proportionally)
/// - `fixed` values are enforced before distributing remainder
/// - `max` caps growth when positive
/// - growable nodes preserve their final weighted proportions whenever their bounds allow it
/// - zero-weight growable nodes share equally when no positive weights are present
/// - if nothing can grow, remainder becomes blank space
pub fn solve_axis_lengths(items: &[AxisConstraints], usable: f64) -> Vec<f64> {
    if items.is_empty() {
        return Vec::new();
    }
    let usable = sanitize(usable);
    let n = items.len();
    let mut mins: Vec<f64> = items.iter().map(|i| sanitize(i.min)).collect();
    let mut fixed: Vec<Option<f64>> =
        items.iter().map(|i| i.fixed.map(sanitize).filter(|v| v.is_finite())).collect();
    let maxs: Vec<Option<f64>> = items
        .iter()
        .map(|i| i.max.map(sanitize).filter(|v| *v > 0.0 && v.is_finite()))
        .collect();
    let weights: Vec<f64> = items.iter().map(|i| sanitize(i.weight)).collect();
    let can_grow: Vec<bool> = items.iter().map(|i| i.can_grow).collect();

    for (idx, f) in fixed.iter_mut().enumerate() {
        if let Some(v) = f {
            if *v < mins[idx] {
                *v = mins[idx];
            }
            if let Some(max) = maxs[idx] {
                if *v > max {
                    *v = max;
                }
            }
        }
    }

    let fixed_sum: f64 = fixed.iter().flatten().copied().sum();
    if fixed_sum > usable && fixed_sum > 0.0 {
        let scale = usable / fixed_sum;
        let mut lengths = vec![0.0; n];
        for idx in 0..n {
            if let Some(v) = fixed[idx] {
                lengths[idx] = v * scale;
            }
        }
        return lengths;
    }

    let min_indices: Vec<usize> = (0..n).filter(|&idx| fixed[idx].is_none()).collect();
    let min_sum: f64 = min_indices.iter().map(|&idx| mins[idx]).sum();
    let remaining_for_mins = (usable - fixed_sum).max(0.0);
    if min_sum > remaining_for_mins && min_sum > 0.0 {
        let scale = remaining_for_mins / min_sum;
        for &idx in &min_indices {
            mins[idx] *= scale;
        }
    }

    let mut lengths = vec![0.0; n];
    let mut remaining = usable;
    for idx in 0..n {
        if let Some(v) = fixed[idx] {
            let assigned = v.min(remaining);
            lengths[idx] = assigned;
            remaining = (remaining - assigned).max(0.0);
        }
    }

    // Non-growable segments stay at their minimum. Growable segments are solved from their
    // *final* weighted sizes, clamped to their bounds. Seeding every segment with its minimum and
    // distributing only the remainder would skew equal-weight splits whenever the minima differ.
    let mut growable: Vec<usize> = Vec::new();
    for idx in 0..n {
        if fixed[idx].is_none() {
            if can_grow[idx] {
                growable.push(idx);
            } else {
                let assigned = mins[idx].min(remaining);
                lengths[idx] = assigned;
                remaining = (remaining - assigned).max(0.0);
            }
        }
    }

    while !growable.is_empty() && remaining > f64::EPSILON {
        let total_weight: f64 = growable.iter().map(|&idx| weights[idx]).sum();
        let use_equal_weights = total_weight <= f64::EPSILON;
        let divisor = if use_equal_weights {
            growable.len() as f64
        } else {
            total_weight
        };

        let mut clamped = Vec::new();
        for &idx in &growable {
            let weight = if use_equal_weights { 1.0 } else { weights[idx] };
            let proposed = remaining * weight / divisor;
            let max = maxs[idx].map(|value| value.max(mins[idx]));
            if proposed + f64::EPSILON < mins[idx] {
                lengths[idx] = mins[idx].min(remaining);
                clamped.push(idx);
            } else if let Some(max) = max
                && proposed > max + f64::EPSILON
            {
                lengths[idx] = max.min(remaining);
                clamped.push(idx);
            }
        }

        if clamped.is_empty() {
            for &idx in &growable {
                let weight = if use_equal_weights { 1.0 } else { weights[idx] };
                lengths[idx] = remaining * weight / divisor;
            }
            remaining = 0.0;
            break;
        }

        let clamped_sum: f64 = clamped.iter().map(|&idx| lengths[idx]).sum();
        remaining = (remaining - clamped_sum).max(0.0);
        growable.retain(|idx| !clamped.contains(idx));
    }

    if remaining <= f64::EPSILON {
        let used: f64 = lengths.iter().sum();
        let drift = usable - used;
        if drift.abs() > f64::EPSILON {
            if let Some(idx) = (0..n).rfind(|&idx| lengths[idx] > 0.0) {
                lengths[idx] = (lengths[idx] + drift).max(0.0);
            }
        }
    }

    lengths
}

#[cfg(test)]
mod tests {
    use super::{AxisConstraints, solve_axis_lengths};

    #[test]
    fn scales_non_fixed_minima_after_reserving_fixed_segments() {
        let solved = solve_axis_lengths(
            &[
                AxisConstraints {
                    min: 0.0,
                    fixed: Some(600.0),
                    max: None,
                    weight: 1.0,
                    can_grow: false,
                },
                AxisConstraints {
                    min: 300.0,
                    fixed: None,
                    max: None,
                    weight: 1.0,
                    can_grow: true,
                },
                AxisConstraints {
                    min: 300.0,
                    fixed: None,
                    max: None,
                    weight: 1.0,
                    can_grow: true,
                },
            ],
            1000.0,
        );

        assert_eq!(solved.len(), 3);
        assert!((solved[0] - 600.0).abs() < 0.001);
        assert!((solved[1] - 200.0).abs() < 0.001);
        assert!((solved[2] - 200.0).abs() < 0.001);
    }

    #[test]
    fn scales_overcommitted_fixed_segments_symmetrically() {
        let solved = solve_axis_lengths(
            &[
                AxisConstraints {
                    min: 0.0,
                    fixed: Some(900.0),
                    max: None,
                    weight: 1.0,
                    can_grow: false,
                },
                AxisConstraints {
                    min: 0.0,
                    fixed: Some(900.0),
                    max: None,
                    weight: 1.0,
                    can_grow: false,
                },
            ],
            1400.0,
        );

        assert_eq!(solved.len(), 2);
        assert!((solved[0] - 700.0).abs() < 0.001);
        assert!((solved[1] - 700.0).abs() < 0.001);
    }

    #[test]
    fn max_caps_participate_in_growth_distribution() {
        let solved = solve_axis_lengths(
            &[
                AxisConstraints {
                    min: 0.0,
                    fixed: None,
                    max: Some(600.0),
                    weight: 1.0,
                    can_grow: true,
                },
                AxisConstraints {
                    min: 0.0,
                    fixed: None,
                    max: None,
                    weight: 1.0,
                    can_grow: true,
                },
            ],
            1600.0,
        );

        assert_eq!(solved.len(), 2);
        assert!((solved[0] - 600.0).abs() < 0.001);
        assert!((solved[1] - 1000.0).abs() < 0.001);
    }

    #[test]
    fn non_binding_minima_do_not_skew_equal_weight_segments() {
        let solved = solve_axis_lengths(
            &[
                AxisConstraints {
                    min: 0.0,
                    fixed: None,
                    max: None,
                    weight: 1.0,
                    can_grow: true,
                },
                AxisConstraints {
                    min: 400.0,
                    fixed: None,
                    max: None,
                    weight: 1.0,
                    can_grow: true,
                },
            ],
            1200.0,
        );

        assert_eq!(solved.len(), 2);
        assert!((solved[0] - 600.0).abs() < 0.001);
        assert!((solved[1] - 600.0).abs() < 0.001);
    }
}

#[cfg(test)]
mod clamp_tests {
    use objc2_core_foundation::CGSize;

    use super::clamp_to_constraints;
    use crate::layout::WindowLayoutConstraints;

    fn max_width(width: f64) -> WindowLayoutConstraints {
        WindowLayoutConstraints {
            is_resizable: true,
            max_width: width,
            ..Default::default()
        }
    }

    #[test]
    fn an_unconstrained_size_is_left_alone() {
        let size = CGSize::new(1000.0, 800.0);
        let got = clamp_to_constraints(size, WindowLayoutConstraints::default());
        assert_eq!((got.width, got.height), (1000.0, 800.0));
    }

    // The maximized case: the whole tiling width offered to a window that will not take it.
    #[test]
    fn a_maximum_caps_the_size_offered() {
        let got = clamp_to_constraints(CGSize::new(3008.0, 1692.0), max_width(800.0));
        assert_eq!(
            got.width, 800.0,
            "a window that cannot be 3008 wide must not be told it is"
        );
        assert_eq!(got.height, 1692.0, "the other axis is unconstrained");
    }

    #[test]
    fn a_fixed_size_wins_over_what_is_offered() {
        let fixed = WindowLayoutConstraints {
            is_resizable: false,
            locked_width: 600.0,
            locked_height: 400.0,
            ..Default::default()
        };
        let got = clamp_to_constraints(CGSize::new(3008.0, 1692.0), fixed);
        assert_eq!((got.width, got.height), (600.0, 400.0));
    }

    // Never GROWN: the frame is a slot the window has to fit inside, so a minimum larger than the
    // slot cannot push it out of the slot. Overlap is worse than a window smaller than it asked for.
    #[test]
    fn a_minimum_larger_than_the_slot_does_not_grow_past_it() {
        let min = WindowLayoutConstraints {
            is_resizable: true,
            min_width: 900.0,
            ..Default::default()
        };
        assert_eq!(clamp_to_constraints(CGSize::new(400.0, 400.0), min).width, 400.0);
    }

    #[test]
    fn a_negative_or_absurd_constraint_cannot_produce_a_negative_size() {
        let junk = WindowLayoutConstraints {
            is_resizable: true,
            min_width: -50.0,
            max_width: -10.0,
            ..Default::default()
        };
        let got = clamp_to_constraints(CGSize::new(500.0, 500.0), junk);
        assert!(got.width >= 0.0 && got.height >= 0.0, "got {got:?}");
    }
}

#[cfg(test)]
mod column_tests {
    use super::{AxisConstraints, column_limits, column_width};
    use crate::layout::WindowLayoutConstraints;

    fn window(min: f64, max: f64, locked: f64) -> WindowLayoutConstraints {
        WindowLayoutConstraints {
            is_resizable: locked <= 0.0,
            locked_width: locked,
            locked_height: 0.0,
            min_width: min,
            min_height: 0.0,
            max_width: max,
            max_height: 0.0,
        }
    }

    // --- column_limits ------------------------------------------------------------------------

    #[test]
    fn a_column_with_no_constrained_windows_has_only_the_one_pixel_floor() {
        let limits = column_limits(std::iter::empty());
        assert_eq!(limits.min, 1.0);
        assert_eq!(limits.fixed, None);
        assert_eq!(limits.max, None);
    }

    /// A column is one width, so its windows agree pessimistically: the largest minimum and the
    /// smallest maximum. Taking either the other way round hands a window a size it refuses.
    #[test]
    fn a_column_takes_the_largest_minimum_and_the_smallest_maximum() {
        let limits = column_limits([window(400., 900., 0.), window(250., 600., 0.)]);
        assert_eq!(limits.min, 400.0);
        assert_eq!(limits.max, Some(600.0));
    }

    /// Two windows locked to different widths take the LARGER. A window given less than its lock is
    /// clipped, which the user can see and scroll; one given more than it can fill leaves a hole.
    #[test]
    fn two_locks_in_one_column_take_the_larger() {
        let limits = column_limits([window(0., 0., 500.), window(0., 0., 700.)]);
        assert_eq!(limits.fixed, Some(700.0));
    }

    /// A zero maximum is "no maximum", not "zero wide". macOS reports 0 for a window with no limit,
    /// and reading it literally would collapse every column holding one.
    #[test]
    fn a_zero_maximum_means_no_maximum() {
        let limits = column_limits([window(100., 0., 0.)]);
        assert_eq!(limits.max, None);
        assert_eq!(limits.min, 100.0);
    }

    // --- column_width -------------------------------------------------------------------------

    fn free() -> AxisConstraints {
        AxisConstraints {
            min: 1.0,
            ..AxisConstraints::default()
        }
    }

    #[test]
    fn a_column_is_its_ratio_of_the_viewport_less_its_share_of_the_gap() {
        // Half of 1000 is 500; half the 20pt gap comes out of the column.
        assert_eq!(column_width(0.5, 1000., 20., free()), 490.0);
    }

    /// The gap comes out of the columns, which is what makes two half-width columns add up to the
    /// viewport instead of overflowing it by one gap.
    #[test]
    fn two_half_columns_and_the_gap_between_them_fit_the_viewport() {
        let each = column_width(0.5, 1000., 20., free());
        assert_eq!(each * 2.0 + 20.0, 1000.0);
    }

    #[test]
    fn a_minimum_widens_a_column_past_its_ratio() {
        let limits = AxisConstraints {
            min: 800.,
            ..AxisConstraints::default()
        };
        assert!(column_width(0.3, 1000., 0., limits) >= 800.0);
    }

    #[test]
    fn a_maximum_narrows_a_column_below_its_ratio() {
        let limits = AxisConstraints {
            min: 1.,
            max: Some(200.),
            ..AxisConstraints::default()
        };
        assert!(column_width(0.9, 1000., 0., limits) <= 200.0);
    }

    /// A window demanding more than it allows gets the MINIMUM. Clipped at the edge is recoverable;
    /// too small to use is not.
    #[test]
    fn a_minimum_beats_a_maximum_that_contradicts_it() {
        let limits = AxisConstraints {
            min: 700.,
            max: Some(300.),
            ..AxisConstraints::default()
        };
        assert_eq!(column_width(0.5, 1000., 0., limits), 700.0);
    }

    /// A lock raises the column's FLOOR and does not cap it.
    ///
    /// `normalized` does not derive a maximum from a lock, so a column holding a window locked to
    /// 650pt is at least 650 wide and may be wider. Capping is the maximum's job, and the window
    /// itself is held to its lock separately by `clamp_to_constraints` — so a non-resizable window in
    /// a wide column sits at its own size with space beside it, rather than the column shrinking to
    /// fit it and dragging its neighbours along.
    #[test]
    fn a_lock_raises_the_column_floor_without_capping_it() {
        let locked = AxisConstraints {
            min: 1.,
            fixed: Some(650.),
            ..AxisConstraints::default()
        };
        assert_eq!(
            column_width(0.1, 1000., 0., locked),
            650.0,
            "widened to the lock"
        );
        assert_eq!(
            column_width(0.9, 1000., 0., locked),
            900.0,
            "but not narrowed to it"
        );
    }

    /// A lock AND a maximum together do pin the column, which is what a window reporting both gets.
    #[test]
    fn a_lock_with_a_maximum_pins_the_column() {
        let pinned = AxisConstraints {
            min: 1.,
            fixed: Some(650.),
            max: Some(650.),
            ..AxisConstraints::default()
        };
        assert_eq!(column_width(0.9, 1000., 0., pinned), 650.0);
    }

    /// The strip scrolls BETWEEN column starts and never pans within one column, so a column wider
    /// than the viewport has a region nothing can scroll to. The clamp is what prevents that.
    #[test]
    fn no_column_is_ever_wider_than_the_viewport() {
        let huge = AxisConstraints {
            min: 5000.,
            ..AxisConstraints::default()
        };
        assert!(column_width(1.0, 1000., 0., huge) <= 1000.0);
        assert!(column_width(4.0, 1000., 0., free()) <= 1000.0);
    }

    /// A full-width column is ratio 1.0 and takes the whole viewport, gap and all: the gap share of a
    /// full-width column is the whole gap, and subtracting it would leave a strip of background down
    /// the side of a maximized window.
    #[test]
    fn a_full_width_column_fills_the_viewport() {
        let width = column_width(1.0, 1000., 20., free());
        assert!(
            width > 900.0,
            "a maximized window does not leave a band of background: {width}"
        );
    }

    /// Never zero and never negative, whatever it is asked for. A zero-width column is a window the
    /// user cannot see or click, and the layout has no way back from one.
    #[test]
    fn a_column_is_always_at_least_one_pixel() {
        for (ratio, viewport, gap) in [
            (0.0, 1000., 0.),
            (0.0001, 10., 100.),
            (0.5, 0., 0.),
            (1.0, 1., 500.),
        ] {
            let width = column_width(ratio, viewport, gap, free());
            assert!(
                width >= 1.0,
                "ratio {ratio} viewport {viewport} gap {gap} gave {width}"
            );
        }
    }

    /// The gap is skipped rather than applied when taking it would push the column under a pixel.
    #[test]
    fn a_gap_larger_than_the_column_is_not_taken() {
        let width = column_width(0.5, 10., 1000., free());
        assert!(width >= 1.0);
    }
}
