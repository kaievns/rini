//! Strips laid out as the layout lays them, on the built-in display, for the scene's tests.

use rini_core::ids::WindowId;
use rini_geometry::space::{Point, ScreenRect, Size};

use super::scene::{ColumnInput, Laid, OverviewInput, RowInput};

/// Between columns, between a column's windows, and around the tiling area.
pub const GAP: f64 = 16.0;

pub fn rect(x: f64, y: f64, width: f64, height: f64) -> ScreenRect {
    ScreenRect::new(Point::new(x, y), Size::new(width, height))
}

/// The built-in display.
pub fn display() -> ScreenRect { rect(0.0, 0.0, 1728.0, 1117.0) }

/// The display less the 32pt bar.
pub fn usable() -> ScreenRect { rect(0.0, 32.0, 1728.0, 1085.0) }

/// The usable frame less the outer gaps.
pub fn tiling() -> ScreenRect { rect(GAP, 32.0 + GAP, 1728.0 - 2.0 * GAP, 1085.0 - 2.0 * GAP) }

pub fn w(idx: u32) -> WindowId { WindowId::new(1, idx) }

/// A column `width` wide holding `windows` top to bottom.
pub struct Col<'a> {
    pub width: f64,
    pub windows: &'a [u32],
}

/// A strip of `columns`, `GAP` apart, its first column `scroll` left of the tiling area's left
/// edge.
pub fn strip(columns: &[Col], scroll: f64) -> Vec<ColumnInput> {
    let tiling = tiling();
    let mut x = tiling.min_x() - scroll;
    columns
        .iter()
        .map(|col| {
            let n = col.windows.len() as f64;
            let height = (tiling.height() - (n - 1.0) * GAP) / n;
            let windows = col
                .windows
                .iter()
                .enumerate()
                .map(|(i, &idx)| Laid {
                    window: w(idx),
                    frame: rect(x, tiling.min_y() + i as f64 * (height + GAP), col.width, height),
                })
                .collect();
            x += col.width + GAP;
            ColumnInput { windows }
        })
        .collect()
}

/// A row of `columns` with no floating windows, aligned on `current`.
pub fn row(columns: Vec<ColumnInput>, current: Option<u32>) -> RowInput {
    RowInput {
        columns,
        floating: Vec::new(),
        current: current.map(w),
    }
}

/// An overview of `rows` on the built-in display, the `active`th shown, at the top of its stack.
pub fn overview(rows: Vec<RowInput>, active: usize) -> OverviewInput {
    OverviewInput {
        rows,
        active,
        display: display(),
        usable: usable(),
        tiling: tiling(),
        scroll: 0.0,
    }
}
