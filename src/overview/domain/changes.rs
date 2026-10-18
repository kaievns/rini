//! What changed between two scenes: the scene before a batch of commands and the one laid out from
//! what they did.
//!
//! Every thumb that moved, changed holder or changed row, with where it was and where it is; each
//! row's pan, by how far its strip moved under the axis; how far the stack scrolled; and whether the
//! selection moved.

use rini_core::ids::WindowId;
use rini_geometry::space::OverlayRect;

use super::scene::{Holder, RowScene, Scene, Selection};

/// Where a thumb is in a scene.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub row: usize,
    pub frame: OverlayRect,
    pub holder: Holder,
}

/// A thumb that is not where it was. `from` is None for a window new to the overview, `to` for
/// one gone from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileChange {
    pub window: WindowId,
    pub from: Option<Placed>,
    pub to: Option<Placed>,
}

impl TileChange {
    pub fn holder_changed(&self) -> bool {
        match (self.from, self.to) {
            (Some(from), Some(to)) => from.holder != to.holder,
            _ => false,
        }
    }

    pub fn row_changed(&self) -> bool {
        match (self.from, self.to) {
            (Some(from), Some(to)) => from.row != to.row,
            _ => false,
        }
    }
}

/// How far a row's strip moved under the axis, on the overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowPan {
    pub row: usize,
    pub by: f64,
}

/// The selection, before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moved {
    pub from: Selection,
    pub to: Selection,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Changes {
    pub tiles: Vec<TileChange>,
    pub pans: Vec<RowPan>,
    /// How much further the stack scrolled up.
    pub scroll: f64,
    pub selection: Option<Moved>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
            && self.pans.is_empty()
            && self.scroll == 0.0
            && self.selection.is_none()
    }

    pub fn tile(&self, window: WindowId) -> Option<&TileChange> {
        self.tiles.iter().find(|change| change.window == window)
    }

    pub fn pan(&self, row: usize) -> Option<f64> {
        self.pans.iter().find(|pan| pan.row == row).map(|pan| pan.by)
    }
}

/// What changed from `before` to `after`.
pub fn changes(before: &Scene, after: &Scene) -> Changes {
    let placed = |scene: &Scene, window: WindowId| {
        scene.find(window).map(|(row, tile)| Placed {
            row,
            frame: tile.frame,
            holder: tile.holder,
        })
    };
    let windows = after
        .rows
        .iter()
        .chain(&before.rows)
        .flat_map(|row| row.tiles.iter().map(|tile| tile.window));
    let mut tiles: Vec<TileChange> = Vec::new();
    for window in windows {
        if tiles.iter().any(|change| change.window == window) {
            continue;
        }
        let from = placed(before, window);
        let to = placed(after, window);
        if from != to {
            tiles.push(TileChange { window, from, to });
        }
    }
    let pans = before
        .rows
        .iter()
        .zip(&after.rows)
        .enumerate()
        .filter_map(|(row, (was, is))| {
            pan(was, is).filter(|by| by.abs() > 1e-9).map(|by| RowPan { row, by })
        })
        .collect();
    Changes {
        tiles,
        pans,
        scroll: after.scroll - before.scroll,
        selection: (before.selected != after.selected).then_some(Moved {
            from: before.selected,
            to: after.selected,
        }),
    }
}

/// How far `is`'s strip sits from `was`'s, read off one window on the strip in both: the one it is
/// aligned on now, else the one it was aligned on, else any.
fn pan(was: &RowScene, is: &RowScene) -> Option<f64> {
    let tiled = |row: &RowScene, window: WindowId| {
        row.tile(window)
            .filter(|tile| tile.holder != Holder::Floating)
            .map(|tile| tile.in_row)
    };
    let both = |window: WindowId| Some((tiled(was, window)?, tiled(is, window)?));
    let (from, to) = is
        .aligned_on
        .and_then(both)
        .or_else(|| was.aligned_on.and_then(both))
        .or_else(|| is.tiles.iter().find_map(|tile| both(tile.window)))?;
    Some(to.center().x - from.center().x)
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{Col, GAP, overview, row, strip, w};
    use super::super::scene::{OverviewInput, lay_out};
    use super::super::stacks::Side;
    use super::*;

    const WIDE: f64 = 900.0;
    const NARROW: f64 = 500.0;

    /// Ten columns on the first workspace, the fifth a stack of two, and a second workspace of two.
    fn columns(order: &[u32]) -> Vec<Col<'static>> {
        order
            .iter()
            .map(|&id| match id {
                5 => Col { width: WIDE, windows: &[5, 50] },
                id => Col {
                    width: if id % 2 == 0 { WIDE } else { NARROW },
                    windows: std::slice::from_ref(&IDS[id as usize]),
                },
            })
            .collect()
    }

    static IDS: [u32; 13] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

    const STRIP: [u32; 10] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

    fn input(
        first: &[u32],
        current: u32,
        second: &[u32],
        second_current: Option<u32>,
    ) -> OverviewInput {
        overview(
            vec![
                row(strip(&columns(first), 0.0), Some(current)),
                row(strip(&columns(second), 0.0), second_current),
            ],
            0,
        )
    }

    fn scene(first: &[u32], current: u32) -> Scene {
        lay_out(&input(first, current, &[11, 12], Some(11)))
    }

    fn centre(scene: &Scene, window: u32) -> f64 {
        scene.find(w(window)).unwrap().1.frame.center().x
    }

    fn in_row(scene: &Scene, window: u32) -> f64 {
        scene.find(w(window)).unwrap().1.in_row.center().x
    }

    #[test]
    fn nothing_changed_is_no_change() {
        let before = scene(&STRIP, 3);
        assert!(changes(&before, &before).is_empty());
    }

    #[test]
    fn a_step_right_pans_the_row_by_the_change_in_its_current_window() {
        let before = scene(&STRIP, 3);
        let after = scene(&STRIP, 4);
        let found = changes(&before, &after);
        let step = (NARROW / 2.0 + GAP + WIDE / 2.0) * before.scale;
        assert!((found.pan(0).unwrap() + step).abs() < 1e-9, "{found:?}");
        assert_eq!(found.pan(1), None, "the row beside stays");
        assert!((centre(&after, 4) - after.axis).abs() < 1e-9);
        assert!((in_row(&before, 4) - (before.axis + step)).abs() < 1e-9);
        assert_eq!(found.selection.unwrap().to.window, Some(w(4)));
        assert!(found.tile(w(3)).is_some());
        assert!((in_row(&after, 3) - (after.axis - step)).abs() < 1e-9);
    }

    #[test]
    fn a_window_crossing_an_edge_changes_holder() {
        let before = scene(&STRIP, 3);
        let after = scene(&STRIP, 4);
        let found = changes(&before, &after);
        let crossed: Vec<&TileChange> = found.tiles.iter().filter(|c| c.holder_changed()).collect();
        assert!(!crossed.is_empty(), "{found:?}");
        let out_of_right = crossed
            .iter()
            .find(|c| matches!(c.from.unwrap().holder, Holder::Stack { side: Side::Right, .. }))
            .expect("a column comes out of the right stack");
        assert_eq!(out_of_right.to.unwrap().holder, Holder::Row);
        let into_left = crossed
            .iter()
            .find(|c| matches!(c.to.unwrap().holder, Holder::Stack { side: Side::Left, .. }))
            .expect("a column goes into the left stack");
        assert_eq!(into_left.from.unwrap().holder, Holder::Row);
    }

    #[test]
    fn a_focus_press_to_an_end_pans_the_row_to_that_end_and_restacks_both_sides() {
        let before = scene(&STRIP, 3);
        let after = scene(&STRIP, 10);
        let found = changes(&before, &after);
        assert!((centre(&after, 10) - after.axis).abs() < 1e-9);
        let pan = found.pan(0).unwrap();
        assert!(pan < 0.0, "the strip pans left, toward the last column");
        assert!((pan + (in_row(&before, 10) - before.axis)).abs() < 1e-9, "{pan}");
        let first_row = &after.rows[0];
        assert!(
            first_row
                .tiles
                .iter()
                .all(|tile| !matches!(tile.holder, Holder::Stack { side: Side::Right, .. })),
            "nothing is right of the last column"
        );
        assert!(matches!(first_row.tile(w(1)).unwrap().holder, Holder::Stack {
            side: Side::Left,
            ..
        }));
    }

    #[test]
    fn a_step_within_a_stacked_column_moves_the_selection_and_pans_nothing() {
        let before = scene(&STRIP, 5);
        let after = scene(&STRIP, 50);
        let found = changes(&before, &after);
        assert_eq!(found.pan(0), None);
        assert!(found.tiles.is_empty(), "{found:?}");
        assert_eq!(found.selection.unwrap().to, Selection {
            row: 0,
            window: Some(w(50)),
        });
    }

    #[test]
    fn a_workspace_step_moves_the_selection_to_the_row_below_and_pans_no_row() {
        let before = lay_out(&input(&STRIP, 3, &[11, 12], Some(12)));
        let mut down = input(&STRIP, 3, &[11, 12], Some(12));
        down.active = 1;
        let after = lay_out(&down);
        let found = changes(&before, &after);
        assert!(found.pans.is_empty());
        assert!(found.tiles.is_empty());
        assert_eq!(found.selection.unwrap().to, Selection {
            row: 1,
            window: Some(w(12)),
        });
    }

    #[test]
    fn a_workspace_step_past_the_floor_scrolls_the_stack() {
        let rows = |active: usize| {
            overview(
                (0..6).map(|i| row(strip(&columns(&[i + 1]), 0.0), Some(i + 1))).collect(),
                active,
            )
        };
        let before = lay_out(&rows(0));
        let mut down = rows(5);
        down.scroll = before.scroll;
        let after = lay_out(&down);
        let found = changes(&before, &after);
        assert!(found.scroll > 0.0, "{found:?}");
        assert!(
            found
                .tiles
                .iter()
                .all(|change| change.from.unwrap().row == change.to.unwrap().row)
        );
    }

    #[test]
    fn a_move_along_the_strip_keeps_the_moved_thumb_on_the_axis_and_the_passed_column_crosses_it() {
        let before = scene(&STRIP, 3);
        let after = scene(&[1, 2, 4, 3, 5, 6, 7, 8, 9, 10], 3);
        let found = changes(&before, &after);
        assert_eq!(found.pan(0), None, "the moved window stays on the axis");
        assert!(found.tile(w(3)).is_none(), "{found:?}");
        assert!((centre(&after, 3) - after.axis).abs() < 1e-9);
        let passed = found.tile(w(4)).unwrap();
        assert!(passed.from.unwrap().frame.center().x > before.axis);
        assert!(passed.to.unwrap().frame.center().x < after.axis);
        assert!(found.selection.is_none());
    }

    #[test]
    fn a_move_to_an_end_crosses_every_column_it_passes() {
        let before = scene(&STRIP, 3);
        let after = scene(&[3, 1, 2, 4, 5, 6, 7, 8, 9, 10], 3);
        let found = changes(&before, &after);
        assert_eq!(found.pan(0), None);
        for passed in [1, 2] {
            let change = found.tile(w(passed)).unwrap();
            assert!(change.from.unwrap().frame.center().x < before.axis);
            assert!(change.to.unwrap().frame.center().x > after.axis, "{change:?}");
        }
    }

    #[test]
    fn a_move_within_a_column_swaps_the_two_thumbs_and_pans_nothing() {
        let before = scene(&STRIP, 5);
        let mut swapped = input(&STRIP, 5, &[11, 12], Some(11));
        let column = &mut swapped.rows[0].columns[4].windows;
        column[0].window = w(50);
        column[1].window = w(5);
        let after = lay_out(&swapped);
        let found = changes(&before, &after);
        assert_eq!(found.pan(0), None);
        let five = found.tile(w(5)).unwrap();
        let fifty = found.tile(w(50)).unwrap();
        assert_eq!(five.to.unwrap().frame, fifty.from.unwrap().frame);
        assert_eq!(fifty.to.unwrap().frame, five.from.unwrap().frame);
        assert!(found.selection.is_none());
    }

    #[test]
    fn a_move_to_the_workspace_below_carries_its_thumb_to_that_row_and_recentres_both() {
        let before = scene(&STRIP, 3);
        let mut moved = input(&[1, 2, 4, 5, 6, 7, 8, 9, 10], 4, &[11, 3, 12], Some(3));
        moved.active = 1;
        let after = lay_out(&moved);
        let found = changes(&before, &after);
        let carried = found.tile(w(3)).unwrap();
        assert!(carried.row_changed());
        assert_eq!(carried.to.unwrap().row, 1);
        assert!((carried.to.unwrap().frame.center().x - after.axis).abs() < 1e-9);
        assert!(
            found.pan(0).is_some(),
            "the row it left recentres on its new current window"
        );
        assert!(found.pan(1).is_some(), "the row it joined recentres on it");
        assert_eq!(found.selection.unwrap().to, Selection {
            row: 1,
            window: Some(w(3)),
        });
    }
}
