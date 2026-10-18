//! The scene: every workspace of a display as a row of thumbs, zoomed out, each row aligned on the
//! window a switch to its workspace focuses.
//!
//! One row per workspace, top to bottom in the stack's order, each the usable frame at the depth
//! `depth::depth` gives. A row's tiled windows are its strip at that depth, placed so its current
//! window is centred on one vertical axis, the tiling area's centre; with no tiled current window
//! the strip stays where it shows. Columns past either edge collapse into the row's edge stacks
//! (`stacks`). Floating windows keep their place on the display, scaled into their row, over its
//! tiles. An empty workspace keeps its row as an outline of its tiling area.

use rini_core::ids::WindowId;
use rini_geometry::space::{OverlayPoint, OverlayRect, Point, ScreenRect, Size};
use rini_geometry::to_overlay_space;

use super::depth::{Span, depth, row_tops};
use super::stacks::{DRAWN_CARDS, Edges, Side, edge_stacks};

/// A window and the frame it is laid out at when its workspace is shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Laid {
    pub window: WindowId,
    pub frame: ScreenRect,
}

/// A column of a workspace's strip: its windows, top to bottom.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnInput {
    pub windows: Vec<Laid>,
}

/// One workspace of the display.
#[derive(Debug, Clone, PartialEq)]
pub struct RowInput {
    /// The strip's columns, left to right.
    pub columns: Vec<ColumnInput>,
    pub floating: Vec<Laid>,
    /// The window a switch to this workspace focuses; on the active workspace, the focused window.
    pub current: Option<WindowId>,
}

/// What the scene is laid out from.
#[derive(Debug, Clone, PartialEq)]
pub struct OverviewInput {
    /// The display's workspaces, in the stack's order.
    pub rows: Vec<RowInput>,
    /// The workspace the display shows.
    pub active: usize,
    /// The display's bounds: the overlay's points are the window server's less its origin.
    pub display: ScreenRect,
    /// What the bar and the Dock leave of the display.
    pub usable: ScreenRect,
    /// The usable frame less the outer gaps.
    pub tiling: ScreenRect,
    /// How far the stack was scrolled when last laid out: 0 at the first layout.
    pub scroll: f64,
}

/// What holds a thumb where it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    /// Its row's strip.
    Row,
    /// An edge stack, `depth` cards behind its top one.
    Stack { side: Side, depth: usize },
    /// Its place on the display, over the row's tiles.
    Floating,
}

/// A window's thumb.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneTile {
    pub window: WindowId,
    /// Where it is drawn.
    pub frame: OverlayRect,
    /// Where its row lays it out before any column stacks: `frame` but for a stacked thumb.
    pub in_row: OverlayRect,
    pub holder: Holder,
}

impl SceneTile {
    /// Whether it is drawn: every thumb but a card behind a stack's last drawn one.
    pub fn drawn(&self) -> bool {
        match self.holder {
            Holder::Stack { depth, .. } => depth < DRAWN_CARDS,
            Holder::Row | Holder::Floating => true,
        }
    }
}

/// One workspace's row.
#[derive(Debug, Clone, PartialEq)]
pub struct RowScene {
    /// The usable frame at the scene's depth, centred on the axis.
    pub band: OverlayRect,
    /// Where the row's strip starts: its first column's left edge, at the display's top. A tiled
    /// thumb is its window's place on the strip at the scene's depth from here, before any column
    /// stacks, so a press that only pans the row moves this and no thumb within it.
    pub strip: OverlayPoint,
    /// Where the display's top-left is drawn for the row's floating thumbs, each its window's frame
    /// on the display at the scene's depth from here.
    pub floating_origin: OverlayPoint,
    /// The tiled window centred on the axis.
    pub aligned_on: Option<WindowId>,
    /// The tiling area's outline at the scene's depth, for a workspace with no windows.
    pub outline: Option<OverlayRect>,
    /// The tiled thumbs column by column, top to bottom, then the floating ones.
    pub tiles: Vec<SceneTile>,
}

impl RowScene {
    pub fn tile(&self, window: WindowId) -> Option<&SceneTile> {
        self.tiles.iter().find(|tile| tile.window == window)
    }
}

/// The window the active ring is on, and its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub row: usize,
    /// None on an empty workspace, whose outline is selected instead.
    pub window: Option<WindowId>,
}

/// The overview of one display, on its overlay.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    /// How far every row is zoomed out.
    pub scale: f64,
    /// How far the stack is scrolled up: 0 when every row fits.
    pub scroll: f64,
    /// The x every row's current window is centred on.
    pub axis: f64,
    pub rows: Vec<RowScene>,
    pub selected: Selection,
}

impl Scene {
    /// The row holding `window`, and its thumb.
    pub fn find(&self, window: WindowId) -> Option<(usize, &SceneTile)> {
        self.rows
            .iter()
            .enumerate()
            .find_map(|(row, scene)| scene.tile(window).map(|tile| (row, tile)))
    }
}

/// The scene `input` is laid out as.
pub fn lay_out(input: &OverviewInput) -> Scene {
    let usable = to_overlay_space(input.usable, input.display);
    let tiling = to_overlay_space(input.tiling, input.display);
    let scale = depth(input.rows.len(), usable.height());
    let axis = tiling.center().x;
    let span = Span {
        top: usable.min_y(),
        bottom: usable.max_y(),
    };
    let placed = row_tops(
        input.rows.len(),
        usable.height() * scale,
        span,
        input.scroll,
        input.active,
    );
    let edges = Edges {
        left: usable.min_x(),
        right: usable.max_x(),
    };
    let rows = input
        .rows
        .iter()
        .zip(&placed.tops)
        .map(|(row, &top)| {
            let space = RowSpace {
                axis,
                scale,
                top,
                usable_top: usable.min_y(),
                display: input.display,
            };
            lay_out_row(row, &space, tiling, usable, edges)
        })
        .collect();
    Scene {
        scale,
        scroll: placed.scroll,
        axis,
        rows,
        selected: Selection {
            row: input.active,
            window: input.rows.get(input.active).and_then(|row| row.current),
        },
    }
}

/// How one row maps the display's overlay points: about `anchor`, which lands on `axis`, at
/// `scale`, the usable frame's top landing on `top`.
struct RowSpace {
    axis: f64,
    scale: f64,
    top: f64,
    usable_top: f64,
    display: ScreenRect,
}

impl RowSpace {
    /// Where the display's top-left lands about `anchor`.
    fn origin(&self, anchor: f64) -> OverlayPoint {
        Point::new(
            self.axis - anchor * self.scale,
            self.top - self.usable_top * self.scale,
        )
    }

    fn place(&self, rect: OverlayRect, anchor: f64) -> OverlayRect {
        OverlayRect::new(
            Point::new(
                self.axis + (rect.min_x() - anchor) * self.scale,
                self.top + (rect.min_y() - self.usable_top) * self.scale,
            ),
            Size::new(rect.width() * self.scale, rect.height() * self.scale),
        )
    }

    fn overlay(&self, frame: ScreenRect) -> OverlayRect { to_overlay_space(frame, self.display) }
}

fn lay_out_row(
    row: &RowInput,
    space: &RowSpace,
    tiling: OverlayRect,
    usable: OverlayRect,
    edges: Edges,
) -> RowScene {
    let display_anchor = tiling.center().x;
    let current = row.current.and_then(|current| {
        row.columns.iter().enumerate().find_map(|(index, column)| {
            column
                .windows
                .iter()
                .find(|laid| laid.window == current)
                .map(|laid| (index, laid))
        })
    });
    let anchor = current.map_or(display_anchor, |(_, laid)| space.overlay(laid.frame).center().x);
    let in_row: Vec<Vec<OverlayRect>> = row
        .columns
        .iter()
        .map(|column| {
            column
                .windows
                .iter()
                .map(|laid| space.place(space.overlay(laid.frame), anchor))
                .collect()
        })
        .collect();
    let column_rects: Vec<OverlayRect> = in_row.iter().map(|rects| span_of(rects)).collect();
    let pivot = current.map(|(index, _)| index).or_else(|| nearest(&column_rects, space.axis));
    let stacks = pivot.map(|pivot| edge_stacks(&column_rects, pivot, edges)).unwrap_or_default();
    let mut tiles = Vec::new();
    for (index, column) in row.columns.iter().enumerate() {
        let card = stacks.card(index);
        for (laid, &placed) in column.windows.iter().zip(&in_row[index]) {
            let (frame, holder) = match card {
                Some((side, card)) => (
                    into_card(placed, column_rects[index], card.frame),
                    Holder::Stack { side, depth: card.depth },
                ),
                None => (placed, Holder::Row),
            };
            tiles.push(SceneTile {
                window: laid.window,
                frame,
                in_row: placed,
                holder,
            });
        }
    }
    for laid in &row.floating {
        let placed = space.place(space.overlay(laid.frame), display_anchor);
        tiles.push(SceneTile {
            window: laid.window,
            frame: placed,
            in_row: placed,
            holder: Holder::Floating,
        });
    }
    let empty =
        row.columns.iter().all(|column| column.windows.is_empty()) && row.floating.is_empty();
    RowScene {
        band: space.place(usable, display_anchor),
        strip: Point::new(
            column_rects.first().map_or(space.origin(anchor).x, |first| first.min_x()),
            space.origin(anchor).y,
        ),
        floating_origin: space.origin(display_anchor),
        aligned_on: current.map(|(_, laid)| laid.window),
        outline: empty.then(|| space.place(tiling, display_anchor)),
        tiles,
    }
}

/// The smallest rect holding every one of `rects`.
fn span_of(rects: &[OverlayRect]) -> OverlayRect {
    let Some(first) = rects.first() else {
        return OverlayRect::zero();
    };
    let (min, max) = rects.iter().fold((first.min(), first.max()), |(min, max), rect| {
        (min.min(rect.min()), max.max(rect.max()))
    });
    OverlayRect::new(min, (max - min).to_size())
}

/// The column whose centre is nearest `x`.
fn nearest(columns: &[OverlayRect], x: f64) -> Option<usize> {
    columns
        .iter()
        .enumerate()
        .filter(|(_, rect)| rect.width() > 0.0)
        .min_by(|(_, a), (_, b)| (a.center().x - x).abs().total_cmp(&(b.center().x - x).abs()))
        .map(|(index, _)| index)
}

/// `rect`, a part of `column`, at the same part of `card`.
fn into_card(rect: OverlayRect, column: OverlayRect, card: OverlayRect) -> OverlayRect {
    if column.width() <= 0.0 || column.height() <= 0.0 {
        return card;
    }
    let sx = card.width() / column.width();
    let sy = card.height() / column.height();
    OverlayRect::new(
        Point::new(
            card.min_x() + (rect.min_x() - column.min_x()) * sx,
            card.min_y() + (rect.min_y() - column.min_y()) * sy,
        ),
        Size::new(rect.width() * sx, rect.height() * sy),
    )
}

#[cfg(test)]
mod tests {
    use rini_geometry::IsWithin;

    use super::super::depth::{MARGIN, ROW_GAP};
    use super::super::fixture::{Col, GAP, display, overview, rect, row, strip, tiling, usable, w};
    use super::*;

    const HALF: f64 = 832.0;

    fn four_rows() -> OverviewInput {
        overview(
            vec![
                row(
                    strip(
                        &[Col { width: HALF, windows: &[1] }, Col {
                            width: 540.0,
                            windows: &[2],
                        }],
                        0.0,
                    ),
                    Some(2),
                ),
                row(
                    strip(
                        &[
                            Col { width: 1100.0, windows: &[3] },
                            Col { width: HALF, windows: &[4, 5] },
                            Col { width: 400.0, windows: &[6] },
                        ],
                        900.0,
                    ),
                    Some(3),
                ),
                row(Vec::new(), None),
                row(strip(&[Col { width: 1696.0, windows: &[7] }], 0.0), Some(7)),
            ],
            1,
        )
    }

    #[test]
    fn every_rows_current_window_sits_on_one_axis_at_the_tiling_areas_centre() {
        let scene = lay_out(&four_rows());
        assert!((scene.axis - tiling().center().x).abs() < 1e-9);
        for (row, current) in [(0, 2), (1, 3), (3, 7)] {
            let tile = scene.rows[row].tile(w(current)).unwrap();
            assert!(
                (tile.frame.center().x - scene.axis).abs() < 1e-9,
                "row {row}: {tile:?}"
            );
            assert_eq!(tile.holder, Holder::Row);
            assert_eq!(scene.rows[row].aligned_on, Some(w(current)));
        }
    }

    #[test]
    fn every_workspace_is_a_row_in_the_stacks_order_at_the_scene_s_depth() {
        let scene = lay_out(&four_rows());
        assert_eq!(scene.rows.len(), 4);
        let height = usable().height() * scene.scale;
        assert!((scene.rows[0].band.min_y() - (usable().min_y() + MARGIN)).abs() < 1e-9);
        for pair in scene.rows.windows(2) {
            assert!((pair[1].band.min_y() - pair[0].band.max_y() - ROW_GAP).abs() < 1e-9);
        }
        for row in &scene.rows {
            assert!((row.band.height() - height).abs() < 1e-9);
            assert!((row.band.center().x - scene.axis).abs() < 1e-9);
        }
        let stacked = scene.rows[1].tile(w(4)).unwrap();
        let below = scene.rows[1].tile(w(5)).unwrap();
        assert!(
            (below.in_row.min_y() - stacked.in_row.max_y() - GAP * scene.scale).abs() < 1e-9,
            "a stacked column is drawn as the strip draws it"
        );
    }

    #[test]
    fn the_active_rows_current_window_is_the_selection() {
        let scene = lay_out(&four_rows());
        assert_eq!(scene.selected, Selection { row: 1, window: Some(w(3)) });
    }

    #[test]
    fn an_empty_workspace_keeps_its_row() {
        let mut input = four_rows();
        input.active = 2;
        let scene = lay_out(&input);
        let empty = &scene.rows[2];
        assert!(empty.tiles.is_empty());
        let outline = empty.outline.expect("an outline of its tiling area");
        assert!((outline.width() - tiling().width() * scene.scale).abs() < 1e-9);
        assert!((outline.height() - tiling().height() * scene.scale).abs() < 1e-9);
        assert!((outline.center().x - scene.axis).abs() < 1e-9);
        assert!(empty.band.min_x() <= outline.min_x() && outline.max_x() <= empty.band.max_x());
        assert!(empty.band.min_y() <= outline.min_y() && outline.max_y() <= empty.band.max_y());
        assert_eq!(scene.selected, Selection { row: 2, window: None });
        assert!(scene.rows.iter().enumerate().all(|(i, row)| (i == 2) == row.outline.is_some()));
    }

    #[test]
    fn floating_windows_keep_their_display_place_in_their_row() {
        let mut input = four_rows();
        let place = rect(300.0, 200.0, 600.0, 400.0);
        input.rows[0].floating.push(Laid { window: w(20), frame: place });
        input.rows[1].floating.push(Laid { window: w(21), frame: place });
        let scene = lay_out(&input);
        for (row, window) in [(0, 20), (1, 21)] {
            let scene_row = &scene.rows[row];
            let tile = scene_row.tile(w(window)).unwrap();
            assert_eq!(tile.holder, Holder::Floating);
            let band = scene_row.band;
            let expected = OverlayRect::new(
                Point::new(
                    band.min_x() + place.min_x() * scene.scale,
                    band.min_y() + (place.min_y() - usable().min_y()) * scene.scale,
                ),
                Size::new(place.width() * scene.scale, place.height() * scene.scale),
            );
            assert!(
                tile.frame.is_within(1e-9, expected),
                "{tile:?} against {expected:?}"
            );
            assert_eq!(
                scene_row.tiles.last().unwrap().window,
                w(window),
                "drawn over the tiles"
            );
        }

        input.rows[1].current = Some(w(6));
        let panned = lay_out(&input);
        assert_eq!(
            panned.rows[1].tile(w(21)),
            scene.rows[1].tile(w(21)),
            "a floating thumb stays still while its row pans"
        );
    }

    /// Every tiled thumb is its window's place on the strip at the scene's depth from its row's
    /// strip, and every floating one its frame on the display from its floating origin: what the
    /// overlay draws a row from as one piece, the strip's own scroll moving neither.
    #[test]
    fn every_thumb_is_its_place_at_the_scenes_depth_from_its_rows_strip() {
        let mut input = four_rows();
        input.rows[1].floating.push(Laid {
            window: w(21),
            frame: rect(300.0, 200.0, 600.0, 400.0),
        });
        let scene = lay_out(&input);
        let at_depth = |origin: OverlayPoint, frame: OverlayRect, left: f64| {
            OverlayRect::new(
                Point::new(
                    origin.x + (frame.min_x() - left) * scene.scale,
                    origin.y + frame.min_y() * scene.scale,
                ),
                Size::new(frame.width() * scene.scale, frame.height() * scene.scale),
            )
        };
        let on = |laid: &Laid| to_overlay_space(laid.frame, display());
        let mut seen = 0;
        for (row, scene_row) in input.rows.iter().zip(&scene.rows) {
            let left = row.columns.first().map_or(0.0, |column| on(&column.windows[0]).min_x());
            for laid in row.columns.iter().flat_map(|column| &column.windows) {
                let expected = at_depth(scene_row.strip, on(laid), left);
                let tile = scene_row.tile(laid.window).unwrap();
                assert!(
                    tile.in_row.is_within(1e-9, expected),
                    "{tile:?} against {expected:?}"
                );
                seen += 1;
            }
            for laid in &row.floating {
                let expected = at_depth(scene_row.floating_origin, on(laid), 0.0);
                let tile = scene_row.tile(laid.window).unwrap();
                assert!(
                    tile.in_row.is_within(1e-9, expected),
                    "{tile:?} against {expected:?}"
                );
                seen += 1;
            }
        }
        assert_eq!(seen, 8);
    }

    #[test]
    fn a_row_whose_current_window_floats_keeps_its_strip_where_it_shows() {
        let mut input = four_rows();
        input.rows[0].floating.push(Laid {
            window: w(20),
            frame: rect(300.0, 200.0, 600.0, 400.0),
        });
        input.rows[0].current = Some(w(20));
        let scene = lay_out(&input);
        assert_eq!(scene.rows[0].aligned_on, None);
        let band = scene.rows[0].band;
        let first = scene.rows[0].tile(w(1)).unwrap();
        let shown = tiling().min_x() * scene.scale;
        assert!(
            (first.frame.min_x() - (band.min_x() + shown)).abs() < 1e-9,
            "{first:?}"
        );
    }

    #[test]
    fn a_scroll_of_the_real_strip_moves_no_thumb() {
        let input = four_rows();
        let mut scrolled = input.clone();
        let columns = [
            Col { width: 1100.0, windows: &[3] },
            Col { width: HALF, windows: &[4, 5] },
            Col { width: 400.0, windows: &[6] },
        ];
        scrolled.rows[1].columns = strip(&columns, 0.0);
        assert_eq!(lay_out(&scrolled), lay_out(&input));
    }

    #[test]
    fn a_stacked_columns_windows_keep_their_places_in_its_card() {
        let ids: Vec<Vec<u32>> =
            (0..8).map(|i| if i == 1 { vec![11, 12] } else { vec![30 + i] }).collect();
        let columns: Vec<Col> = ids
            .iter()
            .enumerate()
            .map(|(i, windows)| Col {
                width: if i == 1 { HALF } else { 700.0 },
                windows,
            })
            .collect();
        let input = overview(vec![row(strip(&columns, 0.0), Some(36))], 0);
        let scene = lay_out(&input);
        let top = scene.rows[0].tile(w(11)).unwrap();
        let bottom = scene.rows[0].tile(w(12)).unwrap();
        assert!(
            matches!(top.holder, Holder::Stack { side: Side::Left, .. }),
            "{top:?}"
        );
        assert_eq!(top.holder, bottom.holder);
        assert!((top.frame.min_x() - bottom.frame.min_x()).abs() < 1e-9);
        assert!(
            top.frame.max_y() < bottom.frame.min_y(),
            "still one above the other"
        );
        assert!(top.frame.width() <= top.in_row.width() + 1e-9);
    }
}
