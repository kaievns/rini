//! Which width the next `Mod+R` asks for.
//!
//! Lifted out of `scrolling.rs` so the rule can be exercised without a layout, a column or a
//! selection. The whole content is "given the width a column has now, which preset comes next" —
//! and the one subtlety that the full viewport is not held by the bounds like the others.

/// The width a preset step asks a column to take.
///
/// The full viewport is a case of its own rather than `Ratio(1.0)` because an ordinary column's
/// ratio is clamped to `max-column-width` — below 1.0 on purpose — so asking for 1.0 as a ratio
/// yields that, not the viewport. The caller gives it as a width held only to the viewport, which is
/// why it has to be told which of the two it is applying. Neither is the full-width mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PresetWidth {
    /// The whole viewport, gap included, past `max-column-width`.
    Full,
    /// A fraction of the viewport, still subject to the configured bounds.
    Ratio(f64),
}

/// A preset at or above this is the full viewport.
///
/// Not exactly 1.0: a ratio written as `0.9999` in a config, or one that has been through a frame
/// rounding trip, means the same thing as 1.0 and nothing between 0.999 and 1.0 is distinguishable
/// on any real display.
const FULL: f64 = 0.999;

/// Floating-point slack when comparing the current width against a preset.
///
/// Frames are rounded to whole points when they are written, so a column sitting at a preset reads
/// back very slightly off it. Without the slack that reads as "already past this preset" and the
/// cycle skips an entry.
const SAME: f64 = 0.01;

/// The next preset wider than `current`, wrapping to the narrowest.
///
/// Ascending with a wrap, which is niri's `switch-preset-column-width`: the widths grow under
/// repeated presses and a column the viewport wide starts over at the narrowest, because nothing
/// is wider than the viewport to grow into.
///
/// Presets outside `(0, 1]` are ignored — a zero-width column cannot be focused out of and one
/// wider than the viewport has a region nothing can scroll to. `None` means there is nothing to
/// cycle through and the key should do nothing at all.
///
/// Order comes from the config rather than being sorted here, so a list written out of order
/// behaves as written. The shipped list is ascending.
pub fn next_preset(current: f64, presets: &[f64]) -> Option<PresetWidth> {
    let usable: Vec<f64> = presets.iter().copied().filter(|r| *r > 0.0 && *r <= 1.0).collect();
    let next = usable
        .iter()
        .copied()
        .find(|preset| *preset > current + SAME)
        .or_else(|| usable.first().copied())?;

    Some(if next >= FULL {
        PresetWidth::Full
    } else {
        PresetWidth::Ratio(next)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRESETS: [f64; 4] = [0.33333, 0.5, 0.66667, 1.0];

    #[test]
    fn each_step_takes_the_next_preset_up() {
        assert_eq!(next_preset(0.33333, &PRESETS), Some(PresetWidth::Ratio(0.5)));
        assert_eq!(next_preset(0.5, &PRESETS), Some(PresetWidth::Ratio(0.66667)));
        assert_eq!(next_preset(0.66667, &PRESETS), Some(PresetWidth::Full));
    }

    /// The reported defect. A column the viewport wide, at 100% or maximised, has nothing wider to
    /// grow into, so the cycle starts over at the narrowest rather than standing still.
    #[test]
    fn a_column_the_viewport_wide_wraps_to_the_narrowest() {
        assert_eq!(next_preset(1.0, &PRESETS), Some(PresetWidth::Ratio(0.33333)));
    }

    /// A width that is not a preset at all — what a resize key leaves behind — joins the cycle at the
    /// first preset above it rather than at the start.
    #[test]
    fn an_arbitrary_width_joins_the_cycle_above_itself() {
        assert_eq!(next_preset(0.4, &PRESETS), Some(PresetWidth::Ratio(0.5)));
        assert_eq!(next_preset(0.7, &PRESETS), Some(PresetWidth::Full));
    }

    /// Rounding a frame to whole points moves the ratio off the preset it came from. Treating that
    /// as "already past" would skip an entry, so a width within 1% counts as being ON the preset.
    #[test]
    fn a_width_a_hair_off_a_preset_still_advances_past_it() {
        assert_eq!(next_preset(0.502, &PRESETS), Some(PresetWidth::Ratio(0.66667)));
        assert_eq!(next_preset(0.497, &PRESETS), Some(PresetWidth::Ratio(0.66667)));
    }

    #[test]
    fn a_preset_of_one_is_the_whole_viewport_rather_than_a_ratio() {
        assert_eq!(next_preset(0.9, &[1.0]), Some(PresetWidth::Full));
        assert_eq!(next_preset(0.5, &[0.9995]), Some(PresetWidth::Full));
    }

    /// A list of one still cycles: it returns the same width every time rather than nothing, so the
    /// key stays idempotent instead of looking broken.
    #[test]
    fn a_single_preset_is_its_own_successor() {
        assert_eq!(next_preset(0.5, &[0.5]), Some(PresetWidth::Ratio(0.5)));
    }

    #[test]
    fn widths_outside_the_viewport_are_not_presets() {
        assert_eq!(
            next_preset(0.4, &[0.0, -1.0, 1.5, 0.5]),
            Some(PresetWidth::Ratio(0.5))
        );
    }

    /// Nothing configured means the key does nothing, rather than a zero-width column.
    #[test]
    fn no_usable_presets_is_no_answer() {
        assert_eq!(next_preset(0.5, &[]), None);
        assert_eq!(next_preset(0.5, &[0.0, 2.0]), None);
    }
}
