//! What the flight engine reads to make a tile: how deep each of a pass's windows is and the radius
//! it rounds at, read once for all of them (`Stacking`), where the window server has each window
//! (`frames_of`, `live_frame`), and what a tile's rules read of a captured picture (`TilePicture`
//! for `Picture`).

use std::collections::HashMap;
use std::time::Instant;

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{FromCg, PointSpace, ScreenRect, Size};

use crate::decorations::domain::style::Worn;
use crate::pictures::domain::route::Route;
use crate::pictures::platform::picture::Picture;
use crate::transitions::domain::running::{TileOf, TilePicture};
use crate::windows::platform::window_server::WindowServer;

impl TilePicture for Picture {
    fn source(&self) -> Route { self.source }

    fn carries_blur(&self) -> bool { self.carries_blur }

    fn taken(&self) -> Instant { self.taken }

    fn fits<U: PointSpace>(&self, size: Size<U>) -> bool { Picture::fits(self, size) }
}

/// What the window server says of a pass's windows, one query for all of them: how deep each is,
/// front to back, and the radius each rounds at.
pub(super) struct Stacking {
    depths: HashMap<u32, usize>,
    radii: HashMap<WindowServerId, f64>,
}

impl Stacking {
    pub(super) fn read(ws: &dyn WindowServer, ids: &[WindowServerId]) -> Self {
        let depths = ws.front_to_back_depths();
        Stacking {
            depths,
            radii: ws.corner_radii(ids),
        }
    }

    /// `window`, `floating` or not and wearing `worn`, at the depth and radius read for it.
    pub(super) fn tile_of(
        &self,
        window: WindowId,
        server_id: WindowServerId,
        floating: bool,
        worn: Worn,
    ) -> TileOf {
        TileOf {
            window,
            floating,
            server_order: self.depths.get(&server_id.as_u32()).copied(),
            worn,
            radius: self.radii.get(&server_id).copied(),
        }
    }
}

/// The frame the window server has for each of `ids` it knows, in one query.
pub(super) fn frames_of(
    ws: &dyn WindowServer,
    ids: &[WindowServerId],
) -> HashMap<WindowServerId, ScreenRect> {
    let windows = ws.get_windows(ids).into_iter();
    windows.map(|info| (info.id, ScreenRect::from_cg(info.frame))).collect()
}

/// The frame the window server reports for a window, if it reports a real one: the reactor's
/// `from` can be the previous pass's destination rather than where the window sits.
///
/// A zero-sized frame is macOS saying it has not laid the window out yet, which is not a position to
/// animate from. The geometry decision is `travel::tile_path`; this is the read it needs.
pub(super) fn live_frame(ws: &dyn WindowServer, server_id: WindowServerId) -> Option<ScreenRect> {
    ws.get_window(server_id)
        .map(|info| ScreenRect::from_cg(info.frame))
        .filter(|frame| frame.size.width > 0.0 && frame.size.height > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::domain::motion::testing::rect;
    use crate::pictures::platform::picture::test_picture;
    use crate::transitions::domain::running::Drawn;

    fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

    #[test]
    fn a_tile_is_made_at_the_depth_and_radius_read_for_its_window_and_at_none_for_one_not_read() {
        let stacking = Stacking {
            depths: HashMap::from([(1, 3)]),
            radii: HashMap::from([(WindowServerId::new(1), 10.0)]),
        };
        let worn = Worn::default();
        let read = stacking.tile_of(wid(1), WindowServerId::new(1), true, worn);
        assert_eq!(read, TileOf {
            window: wid(1),
            floating: true,
            server_order: Some(3),
            worn,
            radius: Some(10.0),
        });
        assert_eq!(
            stacking.tile_of(wid(2), WindowServerId::new(2), false, worn),
            TileOf {
                window: wid(2),
                floating: false,
                server_order: None,
                worn,
                radius: None,
            }
        );
        let (from, to) = (rect(0.0, 32.0, 859.0, 1081.0), rect(863.0, 32.0, 859.0, 1081.0));
        let drawn = Drawn {
            snapshot: test_picture(to.size),
            clipped: true,
        };
        let tile = read.flying((from, to), drawn, true);
        assert_eq!(
            (tile.window, tile.from, tile.to, tile.floating, tile.server_order),
            (wid(1), from, to, true, Some(3))
        );
        assert_eq!(
            (tile.depth, tile.focused, tile.radius, tile.clipped),
            (0, true, Some(10.0), true)
        );
        assert!(tile.from_corner.is_none());
    }
}
