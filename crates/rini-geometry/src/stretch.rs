//! How far a size may be stretched before the stretch shows.
//!
//! A layer's `contentsGravity` defaults to resize, so contents are stretched to fill the layer with no
//! regard for their own proportions. Half a percent either way is rounding; more than that is
//! distortion. One tolerance answers both "does this picture still fit" and "is this a resize", so the
//! two agree by construction.

use crate::space::Size;

/// The largest ratio of a size to the size it is drawn at that still reads as the same size.
pub const MAX_STRETCH: f64 = 1.005;
/// The smallest.
pub const MIN_STRETCH: f64 = 0.995;

/// Whether `from` drawn at `to` stretches within the tolerance on both axes. Never onto an empty size.
pub fn within_stretch<U>(from: Size<U>, to: Size<U>) -> bool {
    if to.width <= 0.0 || to.height <= 0.0 {
        return false;
    }
    let wide = from.width / to.width;
    let tall = from.height / to.height;
    (MIN_STRETCH..=MAX_STRETCH).contains(&wide) && (MIN_STRETCH..=MAX_STRETCH).contains(&tall)
}

/// Whether going from `from` to `to` is a resize rather than a move.
///
/// Rounding is not a resize; treating a one-point re-fit as one measurably tore the strip apart. See
/// "A one-point size change sent the whole strip to the Accessibility engine" in
/// `src/animation/docs/capture-overlay-research.md`.
pub fn is_a_resize<U>(from: Size<U>, to: Size<U>) -> bool { !within_stretch(from, to) }

/// Whether going from `from` to `to` grows on either axis beyond the tolerance: a resize the picture
/// taken at `from` cannot cover at `to`, whatever the other axis does.
pub fn grows<U>(from: Size<U>, to: Size<U>) -> bool {
    from.width / to.width < MIN_STRETCH || from.height / to.height < MIN_STRETCH
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::ScreenSize;

    /// The measured case. A strip re-fit took a window from 918pt to 917pt, and treating that one
    /// point as a resize sent the whole layout to the Accessibility engine, which writes every
    /// window separately and lets the strip come apart.
    #[test]
    fn a_point_of_rounding_is_not_a_resize() {
        assert!(!is_a_resize(
            ScreenSize::new(918.0, 1081.0),
            ScreenSize::new(917.0, 1081.0)
        ));
        assert!(!is_a_resize(
            ScreenSize::new(1720.0, 1081.0),
            ScreenSize::new(1719.0, 1081.0)
        ));
        assert!(!is_a_resize(
            ScreenSize::new(859.0, 1081.0),
            ScreenSize::new(859.0, 1081.0)
        ));
    }

    /// A real resize is drawn anchored and cropped rather than stretched, so the overlay has to
    /// know which one it is looking at.
    #[test]
    fn a_column_changing_width_is_a_resize() {
        assert!(is_a_resize(
            ScreenSize::new(1440.0, 1081.0),
            ScreenSize::new(859.0, 1081.0)
        ));
        assert!(is_a_resize(
            ScreenSize::new(859.0, 1081.0),
            ScreenSize::new(1720.0, 1081.0)
        ));
        assert!(is_a_resize(
            ScreenSize::new(859.0, 1081.0),
            ScreenSize::new(859.0, 540.0)
        ));
    }

    /// The tolerance is proportional, so a point means more on a small window than a large one. That
    /// is the right way round: a point of stretch is invisible across 918pt and obvious across 40pt.
    #[test]
    fn the_tolerance_scales_with_the_window() {
        assert!(!is_a_resize(
            ScreenSize::new(400.0, 400.0),
            ScreenSize::new(401.0, 400.0)
        ));
        assert!(is_a_resize(
            ScreenSize::new(40.0, 400.0),
            ScreenSize::new(41.0, 400.0)
        ));
    }

    /// A grow on either axis grows, one shrinking on the other included; a shrink and a point of
    /// rounding do not, and every grow is a resize.
    #[test]
    fn a_grow_on_either_axis_grows_and_a_shrink_or_rounding_does_not() {
        let size = ScreenSize::new;
        let column = size(859.0, 1081.0);
        for (case, from, to) in [
            ("wider", column, size(1720.0, 1081.0)),
            ("taller", column, size(859.0, 1300.0)),
            ("wider and shorter", column, size(1148.0, 538.0)),
            ("from nothing", size(0.0, 0.0), column),
        ] {
            assert!(grows(from, to), "{case}");
            assert!(is_a_resize(from, to), "{case}");
        }
        assert!(!grows(column, size(859.0, 535.0)), "a shrink");
        assert!(!grows(column, column), "a move");
        assert!(!grows(column, size(863.0, 1081.0)), "rounding");
        assert!(!grows(column, size(0.0, 1081.0)), "onto an empty size");
    }

    #[test]
    fn nothing_stretches_onto_an_empty_size() {
        assert!(!within_stretch(
            ScreenSize::new(859.0, 1081.0),
            ScreenSize::new(0.0, 1081.0)
        ));
        assert!(!within_stretch(
            ScreenSize::new(0.0, 0.0),
            ScreenSize::new(0.0, 0.0)
        ));
        assert!(is_a_resize(ScreenSize::new(0.0, 0.0), ScreenSize::new(0.0, 0.0)));
    }
}
