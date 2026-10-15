//! The desktop and the bar as the window server lists them: the windows a picture of either is
//! composited from. The flight overlay draws them behind and over its tiles, and the bar grounds
//! itself on the desktop. See "The wallpaper is not reliably a window" and "The bar has to be captured
//! on its own" in `src/pictures/docs/capture-research.md`.
use objc2_core_foundation::CGRect;
use rini_core::ids::WindowServerId;
use rini_geometry::CGRectExt;
use rini_geometry::space::{FromCg, ScreenRect, ToCg};

use crate::pictures::domain::backdrop::{is_bar_layer, is_desktop_layer};
use crate::windows::platform::window_server::WindowServer;

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
            || server.owner_name(window.pid).is_some_and(|owner| names_wallpaper(&owner))
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

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};
    use rini_core::ids::pid_t;

    use super::*;
    use crate::windows::platform::window_server::{FakeWindowServer, Listed};

    const WALLPAPER_LEVEL: i32 = -2147483624;
    const DESKTOP_ICON_LEVEL: i32 = -2147483603;

    fn listed(id: u32, level: i32, pid: pid_t, x: f64, title: Option<&str>) -> Listed {
        Listed {
            id: WindowServerId::new(id),
            level,
            pid,
            bounds: CGRect::new(CGPoint::new(x, 0.0), CGSize::new(100.0, 100.0)),
            title: title.map(str::to_owned),
        }
    }

    #[test]
    fn a_desktop_window_owned_or_titled_wallpaper_is_the_wallpaper() {
        let ws = FakeWindowServer::default();
        ws.set_on_screen_windows(vec![
            listed(4, 0, 10, 0.0, None),
            listed(1, WALLPAPER_LEVEL, 10, 0.0, None),
            listed(2, WALLPAPER_LEVEL, 11, 50.0, Some("Wallpaper-0F3A")),
            listed(3, DESKTOP_ICON_LEVEL, 12, 0.0, None),
            listed(5, WALLPAPER_LEVEL, 10, 5000.0, None),
        ])
        .set_owner_name(10, Some("Wallpaper"))
        .set_owner_name(11, Some("Dock"))
        .set_owner_name(12, Some("Finder"));
        let display = CGRect::new(CGPoint::ZERO, CGSize::new(1000.0, 1000.0));

        let backdrop = desktop_backdrop_windows(&ws, ScreenRect::from_cg(display));

        let ids =
            |windows: &[WindowServerId]| windows.iter().map(|id| id.as_u32()).collect::<Vec<_>>();
        assert_eq!(
            ids(&backdrop.windows),
            [1, 2, 3],
            "the desktop's, on the display"
        );
        assert_eq!(ids(&backdrop.wallpaper), [1, 2]);
        assert_eq!(
            backdrop.bounds.map(|bounds| bounds.to_cg()),
            Some(CGRect::new(CGPoint::ZERO, CGSize::new(150.0, 100.0)))
        );
    }

    #[test]
    fn with_no_owner_known_only_a_wallpaper_title_names_the_wallpaper() {
        let ws = FakeWindowServer::default();
        ws.set_on_screen_windows(vec![
            listed(1, WALLPAPER_LEVEL, 10, 0.0, None),
            listed(2, WALLPAPER_LEVEL, 11, 0.0, Some("Wallpaper-0F3A")),
        ]);
        let display = CGRect::new(CGPoint::ZERO, CGSize::new(1000.0, 1000.0));

        let backdrop = desktop_backdrop_windows(&ws, ScreenRect::from_cg(display));

        assert_eq!(backdrop.wallpaper, [WindowServerId::new(2)]);
    }
}
