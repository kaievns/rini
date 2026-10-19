//! The scene: every workspace of a display as a row of thumbs, zoomed out, each row aligned on the
//! window a switch to its workspace focuses.
//!
//! One row per workspace, top to bottom in the stack's order, each the usable frame at the depth
//! `depth::depth` gives. A row's tiled windows are its strip at that depth, placed so its current
//! window is centred on one vertical axis, the tiling area's centre; with no tiled current window
//! the strip stays where it shows. Each tiled thumb is drawn a card's gap narrower than its place,
//! about its centre, so two side by side are that gap apart however close the strip packs them.
//! Columns past either edge collapse into the row's edge stacks (`stacks`). Only the strip's windows
//! are drawn: a floating window is in no row. A workspace with nothing on its strip keeps its row as
//! an outline of its tiling area.

use rini_core::ids::WindowId;
use rini_geometry::space::{OverlayPoint, OverlayRect, Point, ScreenRect, Size};
use rini_geometry::to_overlay_space;

use super::depth::{Span, depth, row_tops};
use super::stacks::{Edges, Side, edge_stacks};
use crate::chrome::domain::card::{GAP, STACK};

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

/// One workspace of the display: its strip, and nothing that floats over it.
#[derive(Debug, Clone, PartialEq)]
pub struct RowInput {
    /// The strip's columns, left to right.
    pub columns: Vec<ColumnInput>,
    /// The strip's window a switch to this workspace focuses, or with a floating window there the
    /// one a focus press off it returns to; on the active workspace, the focused window when it is
    /// on the strip.
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
}

/// A window's thumb.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneTile {
    pub window: WindowId,
    /// Where it is drawn.
    pub frame: OverlayRect,
    /// Where its row lays it out before any column stacks: `frame` but for a stacked thumb. A
    /// tiled one is its place on the strip `apart`, a card's gap narrower about its centre.
    pub in_row: OverlayRect,
    pub holder: Holder,
}

impl SceneTile {
    /// Whether it is drawn: every thumb but a card behind a stack's last drawn one.
    pub fn drawn(&self) -> bool {
        match self.holder {
            Holder::Stack { depth, .. } => depth < STACK.cards,
            Holder::Row => true,
        }
    }
}

/// One workspace's row.
#[derive(Debug, Clone, PartialEq)]
pub struct RowScene {
    /// The usable frame at the scene's depth, centred on the axis.
    pub band: OverlayRect,
    /// Where the row's strip starts: its first column's left edge, at the display's top. A tiled
    /// thumb is its window's place on the strip at the scene's depth from here, `apart`, before any
    /// column stacks, so a press that only pans the row moves this and no thumb within it.
    pub strip: OverlayPoint,
    /// Where the display's top-left is drawn in the row: what the zoom in lands the shown row on.
    pub display_origin: OverlayPoint,
    /// The tiled window centred on the axis.
    pub aligned_on: Option<WindowId>,
    /// The tiling area's outline at the scene's depth, for a workspace with nothing on its strip.
    pub outline: Option<OverlayRect>,
    /// The thumbs column by column, top to bottom.
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
    let places: Vec<Vec<OverlayRect>> = row
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
    let in_row: Vec<Vec<OverlayRect>> = places
        .iter()
        .map(|rects| rects.iter().map(|rect| apart(*rect)).collect())
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
    let empty = row.columns.iter().all(|column| column.windows.is_empty());
    RowScene {
        band: space.place(usable, display_anchor),
        strip: Point::new(
            places.first().map_or(space.origin(anchor).x, |first| span_of(first).min_x()),
            space.origin(anchor).y,
        ),
        display_origin: space.origin(display_anchor),
        aligned_on: current.map(|(_, laid)| laid.window),
        outline: empty.then(|| space.place(tiling, display_anchor)),
        tiles,
    }
}

/// `rect` drawn a card's `GAP` narrower and shorter about its centre, so a thumb beside it or
/// below it is that gap away; no smaller than half on either axis.
pub fn apart(rect: OverlayRect) -> OverlayRect {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return rect;
    }
    let narrowed = |length: f64| (length - GAP).max(length / 2.0);
    let size = Size::new(narrowed(rect.width()), narrowed(rect.height()));
    OverlayRect::new(rect.center() - size.to_vector() / 2.0, size)
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
    use super::super::fixture::{Col, GAP, display, overview, row, strip, tiling, usable, w};
    use super::*;
    use crate::chrome::domain::card;

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
            (below.in_row.center().x - stacked.in_row.center().x).abs() < 1e-9,
            "a stacked column is drawn as the strip draws it"
        );
        assert!(
            below.in_row.min_y() - stacked.in_row.max_y() > GAP * scene.scale,
            "its windows drawn clear of each other"
        );
    }

    /// Two thumbs side by side are a card's gap apart, the strip's own gap at the scene's depth
    /// besides, and two stacked in a column as far, each its window's place narrowed and shortened
    /// about its centre by it.
    #[test]
    fn side_by_side_thumbs_are_a_card_gap_apart() {
        let input = four_rows();
        let scene = lay_out(&input);
        let row = &scene.rows[1];
        let (left, right) = (row.tile(w(3)).unwrap(), row.tile(w(6)).unwrap());
        let middle = row.tile(w(4)).unwrap();
        for (a, b) in [(left, middle), (middle, right)] {
            let between = b.frame.min_x() - a.frame.max_x();
            assert!(
                (between - (card::GAP + GAP * scene.scale)).abs() < 1e-9,
                "{between}"
            );
        }
        let below = row.tile(w(5)).unwrap();
        let between = below.in_row.min_y() - middle.in_row.max_y();
        assert!(
            (between - (card::GAP + GAP * scene.scale)).abs() < 1e-9,
            "stacked: {between}"
        );
        let place = apart(OverlayRect::new(Point::origin(), Size::new(300.0, 100.0)));
        assert_eq!(place.size, Size::new(300.0 - card::GAP, 100.0 - card::GAP));
        let tiny = apart(OverlayRect::new(Point::origin(), Size::new(20.0, 10.0)));
        assert_eq!(tiny.size, Size::new(10.0, 5.0), "no smaller than half");
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

    /// Every thumb is its window's place on the strip at the scene's depth from its row's strip,
    /// and its row's band the usable frame at that depth from where the row draws the display's
    /// top-left: what the overlay draws a row from as one piece, the strip's own scroll moving
    /// neither.
    #[test]
    fn every_thumb_is_its_place_at_the_scenes_depth_from_its_rows_strip() {
        let input = four_rows();
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
                let expected = apart(at_depth(scene_row.strip, on(laid), left));
                let tile = scene_row.tile(laid.window).unwrap();
                assert!(
                    tile.in_row.is_within(1e-9, expected),
                    "{tile:?} against {expected:?}"
                );
                seen += 1;
            }
            let band = at_depth(
                scene_row.display_origin,
                to_overlay_space(usable(), display()),
                0.0,
            );
            assert!(
                scene_row.band.is_within(1e-9, band),
                "{:?} against {band:?}",
                scene_row.band
            );
        }
        assert_eq!(seen, 7);
    }

    #[test]
    fn a_row_with_no_current_window_keeps_its_strip_where_it_shows() {
        let mut input = four_rows();
        input.rows[0].current = None;
        let scene = lay_out(&input);
        assert_eq!(scene.rows[0].aligned_on, None);
        let band = scene.rows[0].band;
        let first = scene.rows[0].tile(w(1)).unwrap();
        let shown = tiling().min_x() * scene.scale;
        assert!(
            (first.frame.min_x() - (band.min_x() + shown + card::GAP / 2.0)).abs() < 1e-9,
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
