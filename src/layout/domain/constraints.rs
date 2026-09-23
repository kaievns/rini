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
