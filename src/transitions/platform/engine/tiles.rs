//! How the flight engine makes a tile: whose it is, how it stacks and what it wears, read once for
//! a pass's windows, and the tile it flies on.

use std::collections::HashMap;

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::OverlayRect;

use crate::animation::domain::motion::plan::entrance_from;
use crate::animation::platform::overlay::OverlayTile;
use crate::decorations::domain::style::Worn;
use crate::pictures::platform::picture::Picture;
use crate::transitions::domain::admission::PendingEntrance;
use crate::windows::platform::window_server::WindowServer;

/// The tile for a reserved entrance whose picture has landed, `clipped` or whole and rounded at
/// `radius`: frontmost and focused, since a window is raised on open and about to hold focus.
pub(super) fn entrance_tile(
    entrance: &PendingEntrance,
    snapshot: &Picture,
    clipped: bool,
    radius: Option<f64>,
) -> OverlayTile {
    let tile = TileOf {
        window: entrance.window,
        floating: entrance.floating,
        server_order: Some(0),
        worn: entrance.worn,
        radius,
    };
    tile.flying(
        (entrance_from(entrance.to), entrance.to),
        snapshot.clone(),
        clipped,
        true,
    )
}

/// Whose a tile is, and how it stacks and what it wears: what a tile is made of beside where it
/// flies and the picture it is drawn with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TileOf {
    pub(super) window: WindowId,
    pub(super) floating: bool,
    pub(super) server_order: Option<usize>,
    pub(super) worn: Worn,
    pub(super) radius: Option<f64>,
}

impl TileOf {
    /// Its tile flying `from` to `to` on `snapshot`, `clipped` or whole and drawn `focused` or not,
    /// at no depth until `restack` bands it.
    pub(super) fn flying(
        self,
        (from, to): (OverlayRect, OverlayRect),
        snapshot: Picture,
        clipped: bool,
        focused: bool,
    ) -> OverlayTile {
        OverlayTile {
            window: self.window,
            from,
            to,
            snapshot,
            from_corner: None,
            floating: self.floating,
            server_order: self.server_order,
            depth: 0,
            focused,
            worn: self.worn,
            radius: self.radius,
            clipped,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::domain::motion::testing::rect;
    use crate::pictures::platform::picture::test_picture;

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
        let tile = read.flying((from, to), test_picture(to.size), true, true);
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
