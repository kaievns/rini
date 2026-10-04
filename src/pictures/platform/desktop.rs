//! The desktop and the bar as the window server lists them: the windows a picture of either is
//! composited from. The flight overlay draws them behind and over its tiles, and the bar grounds
//! itself on the desktop. See "The wallpaper is not reliably a window" and "The bar has to be captured
//! on its own" in `src/pictures/docs/capture-research.md`.
use objc2_core_foundation::{CFDictionary, CFString, CFType, CGRect};
use objc2_core_graphics::{
    CGWindowListOption, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer, kCGWindowName,
    kCGWindowNumber, kCGWindowOwnerName,
};
use rini_core::ids::WindowServerId;
use rini_geometry::CGRectExt;

use crate::pictures::domain::backdrop::{is_bar_layer, is_desktop_layer};
use crate::windows::platform::window_server::{
    bounds_from_dict, get_num, get_string, get_windows_raw, overlaps,
};

/// The windows making up the desktop backdrop, and which of them are the wallpaper.
pub struct DesktopBackdrop {
    pub windows: Vec<WindowServerId>,
    pub wallpaper: Vec<WindowServerId>,
}

impl DesktopBackdrop {
    /// macOS recreates the wallpaper window; a listing without it composites to black.
    pub fn has_wallpaper(&self) -> bool {
        !self.wallpaper.is_empty()
    }
}
/// Window server ids of the desktop backdrop: everything at or below the desktop level on `display`.
/// See "The wallpaper is not reliably a window" in `src/pictures/docs/capture-research.md`.
pub fn desktop_backdrop_windows(display: CGRect) -> DesktopBackdrop {
    let mut windows = Vec::new();
    let mut wallpaper = Vec::new();
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
        let names_wallpaper =
            |key| get_string(&window, key).is_some_and(|value: String| value.contains("Wallpaper"));
        let id = WindowServerId::new(id as u32);
        if names_wallpaper(unsafe { kCGWindowOwnerName })
            || names_wallpaper(unsafe { kCGWindowName })
        {
            wallpaper.push(id);
        }
        windows.push(id);
    }
    DesktopBackdrop { windows, wallpaper }
}
/// The bar's windows and the rect they occupy together.
pub struct BarStrip {
    pub windows: Vec<WindowServerId>,
    /// Union of the windows' bounds, or `None` without a bar. A composite capture covers only this
    /// union, so it must be drawn at the union's origin ("The bar has to be captured on its own" in
    /// `src/pictures/docs/capture-research.md`).
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
            Some(union) => union.union(&frame),
            None => frame,
        });
        windows.push(WindowServerId::new(id as u32));
    }
    BarStrip { windows, bounds }
}
