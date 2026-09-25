//! Whether a redraw of the strip travels to its new places or is simply already there.
//!
//! Pure, and here rather than in the panel, because getting it wrong looks like a different bug than
//! it is: animate a draw whose layers were just built and the whole strip flies in from the corner of
//! the panel, which reads as a rendering fault rather than as an animation decision.

/// How long the selection and the strip take to travel, in seconds.
///
/// Short. The switcher is walked by holding a key down, so this has to finish before the next repeat
/// arrives or the highlight lags behind the selection it is supposed to be showing. macOS's own key
/// repeat is about 30ms at its fastest and 500ms at its slowest default; this sits under the default
/// repeat and simply gets interrupted at the fast end, which is the right failure — an interrupted
/// glide still ends up in the right place.
pub const GLIDE_SECONDS: f64 = 0.13;

/// Whether this draw should travel.
///
/// `showing` is how many rows were on screen before this draw, and `None` when the panel was not up.
///
/// Travel only when the panel is already up with the same number of rows. A panel that is appearing
/// has no previous positions to travel FROM, and rows that were just rebuilt are new layers sitting at
/// the origin — so animating either one flies the strip in from the corner.
pub fn glides(showing: Option<usize>, rows: usize) -> bool {
    showing == Some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stepping the selection with the popup already up: the one case that animates.
    #[test]
    fn a_step_within_an_open_switch_travels() {
        assert!(glides(Some(7), 7));
    }

    /// The layers are at the origin until the first draw places them, so this would fly the strip in
    /// from the panel's corner.
    #[test]
    fn a_panel_that_is_appearing_does_not_travel() {
        assert!(!glides(None, 7));
    }

    /// Rows are rebuilt when the count changes, which means new layers at the origin again.
    #[test]
    fn rebuilt_rows_do_not_travel() {
        assert!(!glides(Some(7), 9));
        assert!(!glides(Some(9), 7));
    }

    /// Under macOS's default key repeat, so holding the trigger does not leave the highlight trailing
    /// the selection it is meant to be reporting.
    #[test]
    fn a_glide_finishes_inside_a_key_repeat() {
        assert!(GLIDE_SECONDS < 0.5, "slower than the default repeat");
        assert!(GLIDE_SECONDS > 0.0, "an animation with no duration is not one");
    }
}
