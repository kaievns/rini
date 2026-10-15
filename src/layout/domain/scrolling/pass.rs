//! One layout pass over a strip: each column's width and start and where the selected column
//! stands (`Geometry`), the scroll offset the pending `ScrollIntent` resolves to
//! (`LayoutState::resolve_scroll`), and every window's frame at that offset (`LayoutState::frames`).

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::WindowId;
use rustc_hash::FxHashMap as HashMap;

use super::{
    Column, LayoutState, Measured, Pin, ScrollIntent, ScrollingLayoutSystem, rebased_offset,
};
use crate::layout::WindowLayoutConstraints;
use crate::layout::domain::area::compute_tiling_area;
use crate::layout::domain::constraints::{
    AxisConstraints, clamp_to_constraints, column_limits, column_width, solve_axis_lengths,
};
use crate::layout::domain::strip::{
    Reveal, anchor_x, column_starts, offset_keeping_right_edge, place_offset, place_share,
    reveal_offset,
};
use crate::layout::domain::tile;
use crate::layout::settings::{Gaps, ScrollingLayoutSettings};

/// A strip measured for one pass, before it scrolls.
#[derive(Debug)]
pub(super) struct Geometry {
    tiling: CGRect,
    gap_x: f64,
    gap_y: f64,
    /// One width per column, the slot it reserves.
    widths: Vec<f64>,
    /// One start per column, along the strip.
    starts: Vec<f64>,
    /// The offset that puts the last column at the anchor.
    max_offset: f64,
    /// The selected column, the first with no selection.
    selected_col: usize,
    selected_width: f64,
    anchor_x: f64,
    /// How far the anchor stands right of where it would centre the selected column.
    center_offset_delta: f64,
}

impl Geometry {
    fn start(&self, col: usize) -> f64 { self.starts.get(col).copied().unwrap_or(0.0) }

    fn width(&self, col: usize) -> f64 { self.widths.get(col).copied().unwrap_or(0.0) }

    /// What a pass at `offset` measured, for `selected` and the `slot` it gave that selection.
    fn measured(&self, offset: f64, selected: Option<WindowId>, slot: Option<CGSize>) -> Measured {
        let selected_rel = self.start(self.selected_col) - offset;
        let selected_x = self.anchor_x + selected_rel;
        Measured {
            screen_width: self.tiling.size.width,
            gap_x: self.gap_x,
            center_offset_delta: self.center_offset_delta,
            selected_rel,
            strip_origin: self.anchor_x - self.tiling.origin.x - offset,
            selected,
            share: Some(place_share(self.tiling, selected_x, self.selected_width)),
            slot,
        }
    }
}

impl ScrollingLayoutSystem {
    /// Every tiled window's frame: its slot in the strip, inset by the border each window wears
    /// (`borders`, `tile::window_in`) and held to the window's own limits. Every width, gap and
    /// limit the strip is laid out from is a slot's.
    pub fn calculate_layout(
        &mut self,
        layout: crate::layout::LayoutId,
        screen: CGRect,
        constraints: &HashMap<WindowId, WindowLayoutConstraints>,
        borders: &tile::Borders,
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let Some(state) = self.layouts.get_mut(layout) else {
            return Vec::new();
        };
        let geometry = state.geometry(&self.settings, screen, constraints, borders, gaps);
        let offset = state.resolve_scroll(&geometry);
        let frames = state.frames(&geometry, offset, constraints, borders);
        let selected = state.selected_location().and(state.selected);
        let slot = selected
            .and_then(|wid| frames.iter().rev().find(|(id, _)| *id == wid))
            .map(|(wid, frame)| tile::slot_of(*frame, borders.of(*wid)).size);
        state.measured = Some(geometry.measured(offset, selected, slot));
        frames
    }

    /// The frames `calculate_layout` gives now, leaving the layout as it is.
    pub fn preview_layout(
        &self,
        layout: crate::layout::LayoutId,
        screen: CGRect,
        constraints: &HashMap<WindowId, WindowLayoutConstraints>,
        borders: &tile::Borders,
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let Some(state) = self.layouts.get(layout) else {
            return Vec::new();
        };
        let mut scratch = Self::new(&self.settings);
        let copy = scratch.layouts.insert(state.clone());
        scratch.calculate_layout(copy, screen, constraints, borders, gaps)
    }
}

impl LayoutState {
    /// Each column's width and start in `screen` under `settings`, and where the selected column
    /// stands.
    pub(super) fn geometry(
        &self,
        settings: &ScrollingLayoutSettings,
        screen: CGRect,
        constraints: &HashMap<WindowId, WindowLayoutConstraints>,
        borders: &tile::Borders,
        gaps: &Gaps,
    ) -> Geometry {
        let tiling = compute_tiling_area(screen, gaps);
        let gap_x = gaps.inner;
        let min_ratio = settings.min_column_width_ratio;
        let max_ratio = settings.max_column_width_ratio;
        let base_ratio =
            ScrollingLayoutSystem::clamp_base_ratio(self.column_width_ratio, min_ratio, max_ratio);
        let widths: Vec<f64> =
            self.columns
                .iter()
                .map(|col| {
                    // A maximised column must be width 1.0 here as well as in its frame: these widths
                    // are what reserve horizontal space in the strip, and reserving the ordinary width
                    // laid the next column out on top of it.
                    //
                    // A lone column keeps its width; expanding it tied a window's size to its
                    // neighbours. See "Column width" in `src/layout/docs/strip.md`.
                    let (kind, ratio) =
                        ScrollingLayoutSystem::laid_out_ratio(self, col, min_ratio, max_ratio);
                    let limits = column_limits(col.windows.iter().filter_map(|wid| {
                        Some((constraints.get(wid).copied()?, borders.of(*wid)))
                    }));
                    column_width(kind, ratio, tiling.size.width, gap_x, limits)
                })
                .collect();
        let (starts, max_offset) = column_starts(&widths, gap_x);
        let selected_col = self.selected_location().map(|(idx, _)| idx).unwrap_or(0);
        let selected_width = widths
            .get(selected_col)
            .copied()
            .unwrap_or((tiling.size.width * base_ratio).max(1.0));
        let anchor_x = anchor_x(
            tiling,
            selected_width,
            selected_col,
            self.columns.len(),
            settings.center_focused_column,
            self.center_override_window.is_some(),
        );
        let center_anchor_x = tiling.origin.x + (tiling.size.width - selected_width) / 2.0;
        Geometry {
            tiling,
            gap_x,
            gap_y: gaps.inner,
            widths,
            starts,
            max_offset,
            selected_col,
            selected_width,
            anchor_x,
            center_offset_delta: anchor_x - center_anchor_x,
        }
    }

    /// The scroll offset this pass lays the strip out at: the last pass's, rebased on a change of
    /// gap, with the pending intent spent on it and a reveal under it run, then held inside the
    /// strip.
    pub(super) fn resolve_scroll(&mut self, geometry: &Geometry) -> f64 {
        if let Some(previous) = self.measured
            && let Some(offset) = rebased_offset(
                previous.gap_x,
                geometry.gap_x,
                previous.selected_rel,
                geometry.start(geometry.selected_col),
            )
        {
            self.scroll_offset = offset;
        }
        if let Some(reveal) = self.spend_intent(geometry)
            && self.selected_location().is_some()
            && let Some(corrected) = reveal_offset(
                reveal,
                geometry.tiling,
                geometry.anchor_x,
                geometry.start(geometry.selected_col),
                geometry.selected_width,
                self.scroll_offset,
            )
        {
            self.scroll_offset = corrected;
        }
        let (min_offset, max_offset) = if self.center_override_window.is_some() {
            let delta = geometry.center_offset_delta;
            (delta, geometry.max_offset + delta)
        } else {
            (0.0, geometry.max_offset)
        };
        self.scroll_offset = self.scroll_offset.clamp(min_offset, max_offset);
        self.scroll_offset
    }

    /// The offset the pending intent asks for, set, and the reveal to run after it.
    fn spend_intent(&mut self, geometry: &Geometry) -> Option<Reveal> {
        let selected_col = |state: &Self| state.selected_location().map(|(col, _)| col);
        match std::mem::take(&mut self.intent) {
            ScrollIntent::Keep => None,
            ScrollIntent::Center => {
                self.scroll_offset = selected_col(self)
                    .map(|col| geometry.center_offset_delta + geometry.start(col))
                    .unwrap_or(0.0);
                None
            }
            ScrollIntent::Align => {
                self.scroll_offset =
                    selected_col(self).map(|col| geometry.start(col)).unwrap_or(0.0);
                None
            }
            ScrollIntent::Reveal(pending) => Some(pending),
            ScrollIntent::Pin(pinned) => {
                let mut reveal = pinned.reveal;
                if self.selected == Some(pinned.window)
                    && let Some(col) = selected_col(self)
                {
                    let start = geometry.start(col);
                    let width = geometry.width(col);
                    self.scroll_offset = match pinned.pin {
                        Pin::RightEdge => {
                            reveal = reveal.map(|_| Reveal::KeepingRightEdge);
                            offset_keeping_right_edge(geometry.anchor_x, start, width, pinned.value)
                        }
                        Pin::Start => start - pinned.value,
                        Pin::Place => {
                            let strip_end = geometry.starts.last().copied().unwrap_or(0.0)
                                + geometry.widths.last().copied().unwrap_or(0.0);
                            let (tiling, anchor) = (geometry.tiling, geometry.anchor_x);
                            place_offset(tiling, anchor, start, width, pinned.value, strip_end)
                        }
                    };
                }
                reveal
            }
        }
    }

    /// Every tiled window's frame with the strip at `offset`.
    pub(super) fn frames(
        &self,
        geometry: &Geometry,
        offset: f64,
        constraints: &HashMap<WindowId, WindowLayoutConstraints>,
        borders: &tile::Borders,
    ) -> Vec<(WindowId, CGRect)> {
        let tiling = geometry.tiling;
        let mut out = Vec::new();
        let columns = self.columns.iter().zip(&geometry.widths).zip(&geometry.starts);
        for ((col, &column_width), &start) in columns {
            if col.windows.is_empty() {
                continue;
            }
            let x = geometry.anchor_x + start - offset;
            let available_height = col.available_height(tiling.size.height, geometry.gap_y);
            let solved_row_heights = col.solved_row_heights(available_height, constraints, borders);
            let fallback_row_height = (available_height / col.windows.len() as f64).max(1.0);
            let mut y_cursor = tiling.origin.y;
            for (row_idx, wid) in col.windows.iter().enumerate() {
                let row_height = solved_row_heights
                    .get(row_idx)
                    .copied()
                    .unwrap_or(fallback_row_height)
                    .max(0.0);
                // round position and size independently to avoid size jitter from min/max rounding.
                let mut frame = CGRect::new(
                    CGPoint::new(x.round(), y_cursor.round()),
                    CGSize::new(column_width.round(), row_height.round()),
                );
                if self.fullscreen_within_gaps.contains(wid) {
                    // The tiling rect's SIZE at the column's own x: keeping the strip-relative x is
                    // what lets the column keep scrolling with the strip (`src/layout/docs/strip.md`).
                    frame = CGRect::new(
                        CGPoint::new(x.round(), tiling.origin.y.round()),
                        CGSize::new(tiling.size.width.round(), tiling.size.height.round()),
                    );
                }
                frame = tile::window_in(frame, borders.of(*wid));
                if let Some(c) = constraints.get(wid).copied() {
                    frame.size = clamp_to_constraints(frame.size, c);
                }
                out.push((*wid, frame));
                y_cursor += row_height;
                if row_idx < col.windows.len() - 1 {
                    y_cursor += geometry.gap_y;
                }
            }
        }
        out
    }
}

impl Column {
    /// Each row's height out of `available`, solved against its window's limits.
    fn solved_row_heights(
        &self,
        available: f64,
        constraints: &HashMap<WindowId, WindowLayoutConstraints>,
        borders: &tile::Borders,
    ) -> Vec<f64> {
        let rows: Vec<AxisConstraints> = self
            .windows
            .iter()
            .enumerate()
            .map(|(row_idx, wid)| {
                let (min, fixed, max, can_grow) = constraints
                    .get(wid)
                    .copied()
                    .map(|c| {
                        let c = tile::slot_limits(c, borders.of(*wid));
                        (
                            c.min_for_axis(false),
                            c.fixed_for_axis(false),
                            (c.max_for_axis(false) > 0.0).then(|| c.max_for_axis(false)),
                            c.resizable_for_axis(false),
                        )
                    })
                    .unwrap_or((0.0, None, None, true));
                // Two meanings, and they are read differently.
                //
                // A column nobody has resized vertically divides evenly: equal weights, and
                // `solve_axis_lengths` gives equal FINAL heights clamped by each window's own
                // limits. Subtracting `min` here is what broke that. Weights are 1.0 while the
                // minima are pixels, so `1.0 - min` floors at 0.001 for any window macOS reported a
                // minimum height for, and 1.0 for any it did not. A folded pair with one reported
                // minimum split 700/100 instead of 400/400: the window with the minimum was left at
                // it, which on screen is its title bar. It only happened when the two windows
                // disagreed about having a minimum, which is why it came and went.
                //
                // After a deliberate vertical resize the weights ARE desired pixel heights, and
                // then the subtraction is right: the solver reserves every minimum first and shares
                // out the remainder, so weights of [900, 100] against minima of [100, 100] have to
                // become [800, 0] to land on 900/100.
                let weight = if self.height_overridden {
                    let raw_weight = self.height_weights.get(row_idx).copied().unwrap_or(1.0);
                    (raw_weight - min).max(0.001)
                } else {
                    1.0
                };
                AxisConstraints {
                    min,
                    fixed,
                    max,
                    weight,
                    can_grow,
                }
            })
            .collect();
        solve_axis_lengths(&rows, available)
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use rini_core::ids::WindowId;
    use rustc_hash::FxHashMap as HashMap;

    use super::{Geometry, LayoutState, ScrollIntent};
    use crate::layout::LayoutId;
    use crate::layout::domain::scrolling::ScrollingLayoutSystem;
    use crate::layout::domain::tile;
    use crate::layout::settings::{Gaps, ScrollingLayoutSettings};

    fn wid(idx: u32) -> WindowId {
        WindowId {
            pid: 1,
            idx: std::num::NonZeroU32::new(idx).unwrap(),
        }
    }

    fn gaps(inner: f64) -> Gaps { Gaps { inner, ..Gaps::default() } }

    /// Three columns, the second selected.
    fn three_columns() -> (ScrollingLayoutSystem, LayoutId) {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        for idx in 1..=3 {
            system.add_window_after_selection(layout, wid(idx));
        }
        assert!(system.select_window(layout, wid(2)));
        (system, layout)
    }

    fn geometry_of(system: &ScrollingLayoutSystem, state: &LayoutState) -> Geometry {
        let screen = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 800.0));
        let constraints = HashMap::default();
        let borders = tile::Borders::default();
        state.geometry(&system.settings, screen, &constraints, &borders, &gaps(10.0))
    }

    #[test]
    fn each_column_starts_one_width_and_one_gap_after_the_last() {
        let (system, layout) = three_columns();
        let state = &system.layouts[layout];
        let geometry = geometry_of(&system, state);
        assert_eq!(geometry.widths.len(), 3);
        assert_eq!(geometry.starts[0], 0.0);
        for col in 1..3 {
            let expected = geometry.starts[col - 1] + geometry.widths[col - 1] + 10.0;
            assert_eq!(geometry.starts[col], expected);
        }
        assert_eq!(geometry.max_offset, geometry.starts[2]);
        assert_eq!(geometry.selected_col, 1);
        assert_eq!(geometry.selected_width, geometry.widths[1]);
    }

    #[test]
    fn resolving_spends_the_intent_and_holds_the_offset_inside_the_strip() {
        let (system, layout) = three_columns();
        let mut state = system.layouts[layout].clone();
        let geometry = geometry_of(&system, &state);

        state.intent = ScrollIntent::Align;
        assert_eq!(state.resolve_scroll(&geometry), geometry.starts[1]);
        assert_eq!(state.intent, ScrollIntent::Keep);

        state.scroll_offset = 1e6;
        assert_eq!(state.resolve_scroll(&geometry), geometry.max_offset);
        state.scroll_offset = -1e6;
        assert_eq!(state.resolve_scroll(&geometry), 0.0);
        assert_eq!(state.scroll_offset, 0.0);
    }

    #[test]
    fn the_frames_move_with_the_offset_and_keep_their_size() {
        let (system, layout) = three_columns();
        let state = &system.layouts[layout];
        let geometry = geometry_of(&system, state);
        let constraints = HashMap::default();
        let borders = tile::Borders::default();
        let at_rest = state.frames(&geometry, 0.0, &constraints, &borders);
        let scrolled = state.frames(&geometry, 100.0, &constraints, &borders);
        assert_eq!(at_rest.len(), 3);
        for ((id, rest), (scrolled_id, moved)) in at_rest.iter().zip(&scrolled) {
            assert_eq!(id, scrolled_id);
            assert_eq!(moved.origin.x, rest.origin.x - 100.0);
            assert_eq!(moved.origin.y, rest.origin.y);
            assert_eq!(moved.size, rest.size);
        }
    }
}
