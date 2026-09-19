//! Window-server reads that belong to the displays, input and animation contexts, waiting for
//! those crates. The windows context's reads are `rini_windows::window_server`.
use std::ffi::c_int;

use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFRetained, CFString, CFType, CGRect, Type, kCFBooleanTrue,
};
use objc2_core_graphics::{
    CGError, CGWindowListOption, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer, kCGWindowName,
    kCGWindowNumber, kCGWindowOwnerName,
};
use rini_skylight_sys::*;
use rini_windows::ids::WindowServerId;
use rini_windows::window_server::{
    bounds_from_dict, get_num, get_visible_windows_raw, get_windows_raw, overlaps,
};

use crate::cg_ok;
use crate::mach::mach_get_window_sub_level;
use rini_displays::screen::ScreenInfo;

fn get_string(dict: &CFDictionary<CFString, CFType>, key: &'static CFString) -> Option<String> {
    Some(dict.get(key)?.downcast::<CFString>().ok()?.to_string())
}
/// Global CG on-screen window snapshot.
///
/// This is intentionally *not* space-aware and should not be used for ordinary
/// reactor/spaces reconciliation. Native-space truth comes from the spaces actor
/// via `space_window_list_for_connection(...)`.
/// Returns whether Mission Control's Dock-owned layer-18 overlay is still
/// visible in the global on-screen window list.
///
/// This intentionally uses the global CG on-screen snapshot because the signal
/// we want is the presence of the Dock's Mission Control UI itself, not
/// ordinary native-space membership.
pub fn mission_control_dock_overlay_visible() -> bool {

    const MISSION_CONTROL_DOCK_LAYER: i64 = 18;

    get_visible_windows_raw::<CFDictionary<CFString, CFType>>()
        .iter()
        .any(|window| {
            if window.get(unsafe { kCGWindowName }).is_some() {
                return false;
            }

            let Some(owner_name) = get_string(&window, unsafe { kCGWindowOwnerName }) else {
                return false;
            };
            if owner_name != "Dock" {
                return false;
            }

            get_num(&window, unsafe { kCGWindowLayer }) == Some(MISSION_CONTROL_DOCK_LAYER)
        })
}
/// The windows making up the desktop backdrop, and whether the wallpaper is among them.
pub struct DesktopBackdrop {
    pub windows: Vec<WindowServerId>,
    /// macOS recreates the wallpaper window; a listing without it composites to black.
    pub has_wallpaper: bool,
}
/// Window server ids of the desktop backdrop: everything at or below the desktop level on `display`.
/// See "The wallpaper is not reliably a window" in `crates/rini-overlay/docs/capture-overlay-research.md`.
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
/// Levels measured in `crates/rini-overlay/docs/capture-overlay-research.md` ("The wallpaper is not reliably a window");
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
    /// `crates/rini-overlay/docs/capture-overlay-research.md`).
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
#[cfg(not(any(test, feature = "test-support")))]
pub fn focus_desktop_window(screen: &ScreenInfo) -> bool {
    use objc2_core_foundation::CFArray;
    use rini_shared::geometry::CGRectExt;
    use rini_windows::window_server::{get_window, make_key_window};
    use std::ptr::NonNull;
    let Some(display_uuid) = screen.display_uuid_opt() else {
        return false;
    };
    let uuid = CFString::from_str(display_uuid);
    let displays = CFArray::from_objects(&[&*uuid]);
    let Some(windows) = NonNull::new(unsafe {
        SLSManagedDisplaysCopyRoleWindows(*G_CONNECTION, CFRetained::as_ptr(&displays).as_ptr(), 1)
    }) else {
        return false;
    };
    let windows = unsafe { CFRetained::from_raw(windows) };
    windows.iter().any(|number| {
        let Some(id) = number.as_i64().and_then(|id| u32::try_from(id).ok()) else {
            return false;
        };
        let wsid = WindowServerId::new(id);
        let Some(info) = get_window(wsid) else {
            return false;
        };
        info.layer < 0
            && screen.frame.contains(info.frame.mid())
            && make_key_window(info.pid, wsid).is_ok()
    })
}
#[cfg(any(test, feature = "test-support"))]
pub fn focus_desktop_window(_screen: &ScreenInfo) -> bool {
    false
}
pub fn window_sub_level(wid: u32) -> c_int {
    unsafe { mach_get_window_sub_level(wid) }
}
pub fn allow_hide_mouse() -> Result<(), CGError> {
    let cid = unsafe { SLSMainConnectionID() };
    let property = CFString::from_str("SetsCursorInBackground");
    let value = CFBoolean::retain(unsafe { kCFBooleanTrue.unwrap_unchecked() });

    cg_ok(unsafe {
        CGSSetConnectionProperty(
            cid,
            cid,
            CFRetained::<CFString>::as_ptr(&property).as_ptr(),
            CFRetained::<CFBoolean>::as_ptr(&value).as_ptr() as *mut CFType,
        )
    })
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
