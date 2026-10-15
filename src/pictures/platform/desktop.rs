//! The desktop and the bar as the window server lists them: the windows a picture of either is
//! composited from. The flight overlay draws them behind and over its tiles, and the bar grounds
//! itself on the desktop. See "The wallpaper is not reliably a window" and "The bar has to be captured
//! on its own" in `src/pictures/docs/capture-research.md`.
use objc2_core_foundation::CGRect;
use rini_core::ids::WindowServerId;
use rini_geometry::CGRectExt;
use rini_geometry::space::{FromCg, ScreenRect, ToCg};

use crate::pictures::domain::backdrop::{is_bar_layer, is_desktop_layer};
use crate::windows::platform::window_server::{WindowServer, owner_name};

/// The windows making up the desktop backdrop, which of them are the wallpaper, and the rect they
/// cover together.
#[derive(Clone, Debug, Default)]
pub struct DesktopBackdrop {
    pub windows: Vec<WindowServerId>,
    pub wallpaper: Vec<WindowServerId>,
    /// The union of the windows' bounds, or `None` with none listed.
    pub bounds: Option<ScreenRect>,
}

impl DesktopBackdrop {
    /// macOS recreates the wallpaper window; a listing without it composites to black.
    pub fn has_wallpaper(&self) -> bool { !self.wallpaper.is_empty() }
}
/// Window server ids of the desktop backdrop: everything at or below the desktop level on `display`.
/// See "The wallpaper is not reliably a window" in `src/pictures/docs/capture-research.md`.
pub fn desktop_backdrop_windows(server: &dyn WindowServer, display: ScreenRect) -> DesktopBackdrop {
    let display = display.to_cg();
    let mut windows = Vec::new();
    let mut wallpaper = Vec::new();
    let mut union: Option<CGRect> = None;
    for window in server.on_screen_windows(true) {
        if !is_desktop_layer(window.level as i64) || !window.bounds.overlaps(&display) {
            continue;
        }
        // Owner "Wallpaper" (wallpaper agent) or owner "Dock" with name "Wallpaper-<uuid>".
        let names_wallpaper = |name: &str| name.contains("Wallpaper");
        if window.title.as_deref().is_some_and(names_wallpaper)
            || names_wallpaper(&owner_name(window.pid))
        {
            wallpaper.push(window.id);
        }
        union = Some(union.map_or(window.bounds, |union| union.union(&window.bounds)));
        windows.push(window.id);
    }
    DesktopBackdrop {
        windows,
        wallpaper,
        bounds: union.map(ScreenRect::from_cg),
    }
}
/// The bar's windows and the rect they occupy together.
pub struct BarStrip {
    pub windows: Vec<WindowServerId>,
    /// Union of the windows' bounds, or `None` without a bar. A composite capture covers only this
    /// union, so it must be drawn at the union's origin ("The bar has to be captured on its own" in
    /// `src/pictures/docs/capture-research.md`).
    pub bounds: Option<ScreenRect>,
}
/// The bar sitting in the menu bar strip, with the rect it occupies.
pub fn bar_strip(server: &dyn WindowServer, display: ScreenRect) -> BarStrip {
    let display = display.to_cg();
    let mut windows: Vec<WindowServerId> = Vec::new();
    let mut bounds: Option<CGRect> = None;
    for window in server.on_screen_windows(false) {
        if !is_bar_layer(window.level as i64) || !window.bounds.overlaps(&display) {
            continue;
        }
        bounds = Some(match bounds {
            Some(union) => union.union(&window.bounds),
            None => window.bounds,
        });
        windows.push(window.id);
    }
    BarStrip {
        windows,
        bounds: bounds.map(ScreenRect::from_cg),
    }
}
