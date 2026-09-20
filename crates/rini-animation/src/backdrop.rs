//! The desktop and the bar as the window server lists them: what the overlay draws behind and over
//! the tiles. See "The wallpaper is not reliably a window" and "The bar has to be captured on its
//! own" in `docs/capture-overlay-research.md`.
use objc2_core_foundation::{CFDictionary, CFString, CFType, CGRect};
use objc2_core_graphics::{
    CGWindowListOption, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer, kCGWindowName,
    kCGWindowNumber, kCGWindowOwnerName,
};
use rini_windows::ids::WindowServerId;
use rini_windows::window_server::{bounds_from_dict, get_num, get_windows_raw, overlaps};

fn get_string(dict: &CFDictionary<CFString, CFType>, key: &'static CFString) -> Option<String> {
    Some(dict.get(key)?.downcast::<CFString>().ok()?.to_string())
}

/// The windows making up the desktop backdrop, and whether the wallpaper is among them.
pub struct DesktopBackdrop {
    pub windows: Vec<WindowServerId>,
    /// macOS recreates the wallpaper window; a listing without it composites to black.
    pub has_wallpaper: bool,
}
/// Window server ids of the desktop backdrop: everything at or below the desktop level on `display`.
/// See "The wallpaper is not reliably a window" in `docs/capture-overlay-research.md`.
pub fn desktop_backdrop_windows(display: CGRect) -> DesktopBackdrop {

    let mut windows = Vec::new();
    let mut has_wallpaper = false;
    for window in get_windows_raw::<CFDictionary<CFString, CFType>>(
        CGWindowListOption::OptionOnScreenOnly,
        kCGNullWindowID,
    )
    .iter()
    {
        let Some(layer) = get_num(&window, unsafe { kCGWindowLayer }) else {
            continue;
        };
        if !is_desktop_layer(layer) {
            continue;
        }
        let Some(bounds) = window
            .get(unsafe { kCGWindowBounds })
            .and_then(|bounds| bounds.downcast::<CFDictionary>().ok())
            .and_then(bounds_from_dict)
        else {
            continue;
        };
        if !overlaps(bounds, display) {
            continue;
        }
        let Some(id) = get_num(&window, unsafe { kCGWindowNumber }) else {
            continue;
        };
        // Owner "Wallpaper" (wallpaper agent) or owner "Dock" with name "Wallpaper-<uuid>".
        let names_wallpaper = |key| {
            get_string(&window, key).is_some_and(|value: String| value.contains("Wallpaper"))
        };
        if names_wallpaper(unsafe { kCGWindowOwnerName }) || names_wallpaper(unsafe { kCGWindowName }) {
            has_wallpaper = true;
        }
        windows.push(WindowServerId::new(id as u32));
    }
    DesktopBackdrop { windows, has_wallpaper }
}
/// Anything at or below this level is the desktop behind every app window.
const DESKTOP_CEILING: i64 = -2147483600;
/// Levels measured in `docs/capture-overlay-research.md` ("The wallpaper is not reliably a window");
/// the wallpaper at -2147483624 is owned by the Dock process and must count as desktop.
pub fn is_desktop_layer(layer: i64) -> bool {
    layer <= DESKTOP_CEILING
}
/// Between the desktop backdrop and normal windows: where a status bar lives (sketchybar: -20).
/// The bar is captured on its own, so it must be out of the desktop picture.
pub fn is_bar_layer(layer: i64) -> bool {
    !is_desktop_layer(layer) && layer < 0
}
/// The bar's windows and the rect they occupy together.
pub struct BarStrip {
    pub windows: Vec<WindowServerId>,
    /// Union of the windows' bounds, or `None` without a bar. A composite capture covers only this
    /// union, so it must be drawn at the union's origin ("The bar has to be captured on its own" in
    /// `docs/capture-overlay-research.md`).
    pub bounds: Option<CGRect>,
}
/// The bar sitting in the menu bar strip, with the rect it occupies.
pub fn bar_strip(display: CGRect) -> BarStrip {
    let mut windows = Vec::new();
    let mut bounds: Option<CGRect> = None;
    for window in get_windows_raw::<CFDictionary<CFString, CFType>>(
        CGWindowListOption::OptionOnScreenOnly,
        kCGNullWindowID,
    )
    .iter()
    {
        let Some(layer) = get_num(&window, unsafe { kCGWindowLayer }) else {
            continue;
        };
        if !is_bar_layer(layer) {
            continue;
        }
        let Some(frame) = window
            .get(unsafe { kCGWindowBounds })
            .and_then(|bounds| bounds.downcast::<CFDictionary>().ok())
            .and_then(bounds_from_dict)
        else {
            continue;
        };
        if !overlaps(frame, display) {
            continue;
        }
        let Some(id) = get_num(&window, unsafe { kCGWindowNumber }) else {
            continue;
        };
        bounds = Some(match bounds {
            Some(union) => union_rect(union, frame),
            None => frame,
        });
        windows.push(WindowServerId::new(id as u32));
    }
    BarStrip { windows, bounds }
}
/// Smallest rect containing both.
fn union_rect(a: CGRect, b: CGRect) -> CGRect {
    let x0 = a.origin.x.min(b.origin.x);
    let y0 = a.origin.y.min(b.origin.y);
    let x1 = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
    let y1 = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
    CGRect::new(
        objc2_core_foundation::CGPoint::new(x0, y0),
        objc2_core_foundation::CGSize::new(x1 - x0, y1 - y0),
    )
}

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
        for layer in [-2147483626, -2147483624, -2147483603, -2147483600, -2147483599, -20, -1, 0, 101] {
            assert!(!(is_desktop_layer(layer) && is_bar_layer(layer)), "layer {layer} counted twice");
        }
    }
    #[test]
    fn the_bar_range_excludes_both_of_its_own_edges() {
        assert!(!is_bar_layer(-2147483600), "the desktop ceiling itself is backdrop");
        assert!(is_bar_layer(-2147483599));
        assert!(is_bar_layer(-1));
    }
}
