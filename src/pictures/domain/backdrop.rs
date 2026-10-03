//! Which window-server levels are the desktop and which are the bar, so the two capture routes agree on
//! what goes in each picture. See "The wallpaper is not reliably a window" and "The bar has to be
//! captured on its own" in `src/animation/docs/capture-overlay-research.md`.

/// Anything at or below this level is the desktop behind every app window.
const DESKTOP_CEILING: i64 = -2147483600;

/// Levels measured in `src/animation/docs/capture-overlay-research.md` ("The wallpaper is not reliably
/// a window"); the wallpaper at -2147483624 is owned by the Dock process and must count as desktop.
pub fn is_desktop_layer(layer: i64) -> bool { layer <= DESKTOP_CEILING }

/// Between the desktop backdrop and normal windows: where a status bar lives (sketchybar: -20).
/// The bar is captured on its own, so it must be out of the desktop picture.
pub fn is_bar_layer(layer: i64) -> bool { !is_desktop_layer(layer) && layer < 0 }

#[cfg(test)]
mod tests {
    use super::{DESKTOP_CEILING, is_bar_layer, is_desktop_layer};
    /// Every level measured on this machine, so the two capture routes cannot drift apart: the bar has to
    /// be in the bar's picture and out of the desktop's, or the menu bar strip either goes dark or gets a
    /// second copy of the bar depending on which route served the backdrop.
    #[test]
    fn only_the_status_bar_counts_as_the_bar() {
        assert!(is_bar_layer(-20), "sketchybar, 24 windows across the strip");
        assert!(!is_bar_layer(0), "normal windows");
        assert!(!is_bar_layer(101), "rini's own overlay");
        assert!(!is_bar_layer(-2147483601), "Notification Center widgets");
        assert!(!is_bar_layer(-2147483603), "Finder's desktop icons");
        assert!(!is_bar_layer(-2147483624), "the wallpaper, owned by Dock");
        assert!(!is_bar_layer(-2147483626), "the display backstop");
    }
    /// Every level measured on this machine. The wallpaper's belongs here: leaving it out is what made the
    /// whole background go almost black during animations, because the composite was then desktop icons on
    /// a bare backstop.
    #[test]
    fn the_desktop_is_everything_at_or_below_the_ceiling() {
        assert!(is_desktop_layer(-2147483626), "the display backstop");
        assert!(is_desktop_layer(-2147483624), "the wallpaper, owned by Dock");
        assert!(is_desktop_layer(-2147483603), "Finder's desktop icons");
        assert!(is_desktop_layer(-2147483602), "the window server's underbelly");
        assert!(is_desktop_layer(-2147483601), "Notification Center's widgets");
        assert!(is_desktop_layer(DESKTOP_CEILING), "the ceiling itself");
        assert!(!is_desktop_layer(-20), "sketchybar");
        assert!(!is_desktop_layer(0), "normal windows");
        assert!(!is_desktop_layer(101), "rini's own overlay");
    }
    /// The desktop and the bar are what the two capture routes have to agree on, and nothing may be both:
    /// a window counted twice is composited twice.
    #[test]
    fn no_level_is_both_desktop_and_bar() {
        for layer in [
            -2147483626,
            -2147483624,
            -2147483603,
            -2147483600,
            -2147483599,
            -20,
            -1,
            0,
            101,
        ] {
            assert!(
                !(is_desktop_layer(layer) && is_bar_layer(layer)),
                "layer {layer} counted twice"
            );
        }
    }
    #[test]
    fn the_bar_range_excludes_both_of_its_own_edges() {
        assert!(
            !is_bar_layer(-2147483600),
            "the desktop ceiling itself is backdrop"
        );
        assert!(is_bar_layer(-2147483599));
        assert!(is_bar_layer(-1));
    }
}
