//! The width a column was given, as the layout means it rather than in points: what a column holds
//! (`GivenWidth`), and what a window carries to another layout or display (`ColumnWidth`).

/// The width a window occupied, as the layout means it rather than in points.
///
/// Points would not survive a resolution change or a move between displays of different widths,
/// which is exactly when a remembered width is consulted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColumnWidth {
    /// Full viewport width, as `Mod+F` produces. Kept distinct from `Offset` because it is a MODE,
    /// not a ratio: it must stay full width on a display of any size, and it round-trips back to
    /// the width it had when toggled off.
    FullWidth,
    /// A deliberate width, as an offset from the configured `column_width_ratio`.
    Offset(f64),
    /// An offset as `Offset` is, but unlike it held by no display's `min-column-width` or
    /// `max-column-width`: the width a mouse drag gives, and the preset cycle's 100%.
    Unbounded(f64),
}

/// A width a column was given, as an offset from its layout's `column_width_ratio`. A column with
/// none takes the ratio itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GivenWidth {
    /// Held to `min-column-width` and `max-column-width`, as a key's width is.
    Bounded(f64),
    /// Held only to the viewport, as a mouse drag or `Mod+R`'s 100% gives it.
    Unbounded(f64),
}

impl GivenWidth {
    pub fn offset(self) -> f64 {
        match self {
            Self::Bounded(offset) | Self::Unbounded(offset) => offset,
        }
    }

    /// The same bounds, `by` further from the ratio.
    pub fn shifted(self, by: f64) -> Self {
        match self {
            Self::Bounded(offset) => Self::Bounded(offset + by),
            Self::Unbounded(offset) => Self::Unbounded(offset + by),
        }
    }

    /// The width it carries to another layout.
    pub fn carried(self) -> ColumnWidth {
        match self {
            Self::Bounded(offset) => ColumnWidth::Offset(offset),
            Self::Unbounded(offset) => ColumnWidth::Unbounded(offset),
        }
    }

    /// The width a carried one gives a column, `None` for full width, which is a mode the column's
    /// window enters rather than a width the column holds.
    pub fn from_carried(width: ColumnWidth) -> Option<Self> {
        match width {
            ColumnWidth::FullWidth => None,
            ColumnWidth::Offset(offset) => Some(Self::Bounded(offset)),
            ColumnWidth::Unbounded(offset) => Some(Self::Unbounded(offset)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_given_width_carries_as_the_column_width_of_its_bounds_and_comes_back_the_same() {
        let cases = [
            (GivenWidth::Bounded(0.25), ColumnWidth::Offset(0.25)),
            (GivenWidth::Unbounded(0.33797), ColumnWidth::Unbounded(0.33797)),
        ];
        for (given, carried) in cases {
            assert_eq!(given.carried(), carried);
            assert_eq!(GivenWidth::from_carried(carried), Some(given));
        }
        assert_eq!(GivenWidth::from_carried(ColumnWidth::FullWidth), None);
    }

    #[test]
    fn a_shifted_width_keeps_its_bounds() {
        assert_eq!(GivenWidth::Unbounded(0.3).offset(), 0.3);
        assert_eq!(
            GivenWidth::Bounded(0.1).shifted(-0.05),
            GivenWidth::Bounded(0.05)
        );
        assert_eq!(
            GivenWidth::Unbounded(0.3).shifted(0.2),
            GivenWidth::Unbounded(0.5)
        );
    }
}
