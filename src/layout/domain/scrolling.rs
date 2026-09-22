use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU64, Ordering};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use serde::{Deserialize, Serialize};

use rini_core::ids::{WindowId, pid_t};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use crate::layout::settings::{
    ScrollingFocusNavigationStyle, ScrollingLayoutSettings, WindowInsertionPoint,
};
use crate::layout::domain::constraints::{AxisConstraints, clamp_to_constraints, solve_axis_lengths};
use crate::layout::domain::strip::{Reveal, anchor_x, column_starts, gap_share, reveal_offset};
use crate::layout::WindowLayoutConstraints;
use crate::layout::domain::area::compute_tiling_area;
use crate::layout::{Direction, LayoutId, ResizeOrientation};

/// Where a maximized window sat in its column stack, so a second press can put it back.
///
/// A neighbour rather than an index: while the window is maximized its old column can move along
/// the strip, gain windows or lose them, and an index would point somewhere else by then.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
struct StackOrigin {
    /// A window that stayed behind in the column.
    anchor: WindowId,
    /// It sat below the anchor rather than above it.
    below: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Column {
    windows: Vec<WindowId>,
    width_offset: f64,
    #[serde(default)]
    width_overridden: bool,
    #[serde(default)]
    height_weights: Vec<f64>,
    /// Whether `height_weights` are DESIRED PIXEL HEIGHTS from a deliberate vertical resize, rather
    /// than the equal ratios a freshly folded column starts at. The two are read differently and
    /// conflating them is what collapsed a folded window to its title bar.
    #[serde(default)]
    height_overridden: bool,
}

impl Column {
    fn ensure_height_weights(&mut self) {
        if self.height_weights.len() != self.windows.len() {
            self.height_weights.resize(self.windows.len(), 1.0);
        }
    }

    /// Equal shares, and no longer a deliberate split.
    fn equalise_heights(&mut self) {
        let count = self.windows.len();
        self.height_weights.clear();
        self.height_weights.resize(count, 1.0);
        self.height_overridden = false;
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct LayoutState {
    columns: Vec<Column>,
    selected: Option<WindowId>,
    column_width_ratio: f64,
    #[serde(skip, default = "default_atomic")]
    scroll_offset_px: AtomicU64,
    #[serde(skip, default = "default_atomic_bool")]
    pending_align: AtomicBool,
    #[serde(skip, default = "default_atomic_bool")]
    pending_center_align: AtomicBool,
    #[serde(skip, default = "default_atomic_i8")]
    pending_reveal_direction: AtomicI8,
    center_override_window: Option<WindowId>,
    #[serde(skip, default = "default_atomic")]
    last_screen_width: AtomicU64,
    #[serde(skip, default = "default_atomic")]
    last_gap_x: AtomicU64,
    #[serde(skip, default = "default_atomic")]
    last_step_px: AtomicU64,
    #[serde(skip, default = "default_atomic")]
    last_center_offset_delta_px: AtomicU64,
    #[serde(skip, default = "default_atomic")]
    overscroll_accumulation: AtomicU64,
    fullscreen_within_gaps: HashSet<WindowId>,
    /// Only for windows pulled out of a stack to be maximized.
    #[serde(default)]
    stack_origins: HashMap<WindowId, StackOrigin>,
}

impl LayoutState {
    fn new(column_width_ratio: f64) -> Self {
        Self {
            columns: Vec::new(),
            selected: None,
            column_width_ratio,
            scroll_offset_px: AtomicU64::new(0.0f64.to_bits()),
            pending_align: AtomicBool::new(false),
            pending_center_align: AtomicBool::new(false),
            pending_reveal_direction: AtomicI8::new(0),
            center_override_window: None,
            last_screen_width: AtomicU64::new(0.0f64.to_bits()),
            last_gap_x: AtomicU64::new(0.0f64.to_bits()),
            last_step_px: AtomicU64::new(0.0f64.to_bits()),
            last_center_offset_delta_px: AtomicU64::new(0.0f64.to_bits()),
            overscroll_accumulation: AtomicU64::new(0.0f64.to_bits()),
            fullscreen_within_gaps: HashSet::default(),
            stack_origins: HashMap::default(),
        }
    }

    fn first_window(&self) -> Option<WindowId> {
        self.columns.first().and_then(|c| c.windows.first()).copied()
    }

    /// Pull `wid` out of a shared column into its own, immediately to the right.
    ///
    /// `None` when it was alone in its column and there was nothing to pull it out of. Otherwise the
    /// neighbour to put it back beside, which the caller keeps until the window is restored.
    /// Take `wid` out of its column into a column of its own, on `side` of the one it left.
    ///
    /// The one way a window leaves a stack. Unfolding, expelling and unstacking were four copies of
    /// this, each with its own idea of where the new column goes and whether the weight travels.
    ///
    /// Returns the neighbour to fold it back beside, or `None` when it was alone and there was no
    /// stack to leave. Nothing is moved in that case.
    fn split_out(&mut self, wid: WindowId, side: Direction) -> Option<StackOrigin> {
        let (col_idx, row_idx) = self.locate(wid)?;
        if self.columns[col_idx].windows.len() <= 1 {
            return None;
        }
        // Read before the removal, while the neighbours are still where they were.
        let origin = if row_idx > 0 {
            StackOrigin { anchor: self.columns[col_idx].windows[row_idx - 1], below: true }
        } else {
            StackOrigin { anchor: self.columns[col_idx].windows[row_idx + 1], below: false }
        };
        self.columns[col_idx].ensure_height_weights();
        self.columns[col_idx].windows.remove(row_idx);
        let weight = self.columns[col_idx].height_weights.remove(row_idx);
        // The column it left keeps its own shape, and it had one row fewer a moment ago, so an
        // even split has to be recomputed rather than inherited.
        if !self.columns[col_idx].height_overridden {
            self.columns[col_idx].equalise_heights();
        }
        let insert_at = match side {
            Direction::Left => col_idx,
            _ => (col_idx + 1).min(self.columns.len()),
        };
        self.columns.insert(
            insert_at,
            Column {
                windows: vec![wid],
                width_offset: 0.0,
                width_overridden: false,
                height_weights: vec![weight],
                height_overridden: false,
            },
        );
        Some(origin)
    }

    /// Put `wid` back beside the neighbour it was pulled away from.
    ///
    /// `false` when the anchor is gone, in which case the window stays the column it became: there
    /// is no stack left to rejoin, and inventing one would put it somewhere the user never had it.
    fn restore_into_stack(&mut self, wid: WindowId, origin: StackOrigin) -> bool {
        let Some((anchor_col, _)) = self.locate(origin.anchor) else {
            return false;
        };
        let Some((col_idx, row_idx)) = self.locate(wid) else {
            return false;
        };
        if col_idx == anchor_col {
            return false;
        }
        self.columns[col_idx].ensure_height_weights();
        self.columns[col_idx].windows.remove(row_idx);
        let weight = self.columns[col_idx].height_weights.remove(row_idx);
        if self.columns[col_idx].windows.is_empty() {
            self.columns.remove(col_idx);
        }
        // Located again: removing the column above may have shifted the anchor's own index.
        let Some((anchor_col, anchor_row)) = self.locate(origin.anchor) else {
            return false;
        };
        let at = if origin.below { anchor_row + 1 } else { anchor_row };
        let column = &mut self.columns[anchor_col];
        column.ensure_height_weights();
        let at = at.min(column.windows.len());
        column.windows.insert(at, wid);
        column.height_weights.insert(at, weight);
        // The weight came from a column of its own, where one row took everything, so it says
        // nothing about a share of this one. Only a column that was deliberately split keeps its
        // numbers; otherwise the rows go back to dividing evenly.
        if !column.height_overridden {
            column.equalise_heights();
        }
        true
    }

    fn locate(&self, wid: WindowId) -> Option<(usize, usize)> {
        for (col_idx, col) in self.columns.iter().enumerate() {
            for (row_idx, w) in col.windows.iter().enumerate() {
                if *w == wid {
                    return Some((col_idx, row_idx));
                }
            }
        }
        None
    }

    fn selected_location(&self) -> Option<(usize, usize)> {
        self.selected.and_then(|wid| self.locate(wid))
    }

    fn selected_or_first(&self) -> Option<WindowId> {
        self.selected.or_else(|| self.first_window())
    }

    fn align_scroll_to_selected(&mut self) {
        // Keep centered alignment only while the same selection remains focused.
        if self.center_override_window.is_some() && self.center_override_window == self.selected {
            self.pending_center_align.store(true, Ordering::Relaxed);
            self.pending_reveal_direction.store(0, Ordering::Relaxed);
            self.pending_align.store(false, Ordering::Relaxed);
            return;
        }
        self.center_override_window = None;
        self.pending_center_align.store(false, Ordering::Relaxed);
        self.pending_reveal_direction.store(0, Ordering::Relaxed);
        let Some((_col_idx, _)) = self.selected_location() else {
            self.scroll_offset_px.store(0.0f64.to_bits(), Ordering::Relaxed);
            return;
        };
        self.pending_align.store(true, Ordering::Relaxed);
    }

    fn request_center_on_selected(&mut self) {
        if self.selected_location().is_none() {
            return;
        }
        if self.center_override_window.is_some() && self.center_override_window == self.selected {
            // Toggle off when already centered on the same selection.
            self.center_override_window = None;
            self.pending_center_align.store(false, Ordering::Relaxed);
            self.pending_align.store(true, Ordering::Relaxed);
            self.pending_reveal_direction.store(0, Ordering::Relaxed);
        } else {
            self.center_override_window = self.selected;
            self.pending_center_align.store(true, Ordering::Relaxed);
            self.pending_align.store(false, Ordering::Relaxed);
            self.pending_reveal_direction.store(0, Ordering::Relaxed);
        }
    }

    fn reveal_selected_in_direction(&mut self, direction: Direction) {
        self.center_override_window = None;
        self.pending_center_align.store(false, Ordering::Relaxed);
        self.pending_align.store(false, Ordering::Relaxed);
        let dir_code = match direction {
            Direction::Left => -1,
            Direction::Right => 1,
            _ => 0,
        };
        self.pending_reveal_direction.store(dir_code, Ordering::Relaxed);
    }

    fn reveal_selected_without_direction(&mut self) {
        self.center_override_window = None;
        self.pending_center_align.store(false, Ordering::Relaxed);
        self.pending_align.store(false, Ordering::Relaxed);
        // 2 = neutral reveal: keep current offset unless selected would be clipped.
        self.pending_reveal_direction.store(2, Ordering::Relaxed);
    }

    fn clamp_scroll_offset(&mut self) {
        if self.columns.is_empty() {
            self.scroll_offset_px.store(0.0f64.to_bits(), Ordering::Relaxed);
            return;
        }
        // Keep the user's current strip position; final bounds clamping happens in
        // `calculate_layout` where full column geometry is available.
        self.pending_align.store(false, Ordering::Relaxed);
    }

    fn remove_window(&mut self, wid: WindowId) -> Option<WindowId> {
        let (col_idx, row_idx) = self.locate(wid)?;
        let col = &mut self.columns[col_idx];
        col.ensure_height_weights();
        col.windows.remove(row_idx);
        col.height_weights.remove(row_idx);
        if col.windows.is_empty() {
            self.columns.remove(col_idx);
        }
        self.fullscreen_within_gaps.remove(&wid);
        self.stack_origins.remove(&wid);
        // An origin anchored on a window that has closed cannot be restored into, and keeping it
        // would send the next un-maximize hunting for a window that is gone.
        self.stack_origins.retain(|_, origin| origin.anchor != wid);

        if self.selected == Some(wid) {
            self.selected = None;
            if col_idx < self.columns.len() {
                let col = &self.columns[col_idx];
                if let Some(new_sel) = col.windows.get(row_idx).copied() {
                    self.selected = Some(new_sel);
                } else if let Some(new_sel) = col.windows.last().copied() {
                    self.selected = Some(new_sel);
                }
            }
            if self.selected.is_none() && col_idx > 0 {
                if let Some(new_sel) = self.columns[col_idx - 1].windows.last().copied() {
                    self.selected = Some(new_sel);
                }
            }
            if self.selected.is_none() {
                self.selected = self.first_window();
            }
        }
        if self.center_override_window == Some(wid) {
            self.center_override_window = None;
        }

        self.clamp_scroll_offset();
        self.selected
    }

    fn insert_column_after(&mut self, index: usize, wid: WindowId) {
        let column = Column {
            windows: vec![wid],
            width_offset: 0.0,
            width_overridden: false,
            height_weights: vec![1.0],
        height_overridden: false,
        };
        let insert_at = (index + 1).min(self.columns.len());
        self.columns.insert(insert_at, column);
        self.selected = Some(wid);
        self.align_scroll_to_selected();
    }

    fn insert_column_at_end(&mut self, wid: WindowId) {
        self.columns.push(Column {
            windows: vec![wid],
            width_offset: 0.0,
            width_overridden: false,
            height_weights: vec![1.0],
        height_overridden: false,
        });
        self.selected = Some(wid);
        self.align_scroll_to_selected();
    }

    fn move_window_to_column_end(&mut self, wid: WindowId, target_col: usize) {
        if let Some((col_idx, row_idx)) = self.locate(wid) {
            if col_idx == target_col {
                return;
            }
            self.columns[col_idx].ensure_height_weights();
            let window = self.columns[col_idx].windows.remove(row_idx);
            let weight = self.columns[col_idx].height_weights.remove(row_idx);
            let removed_column = self.columns[col_idx].windows.is_empty();
            if removed_column {
                self.columns.remove(col_idx);
            }
            let mut target = target_col;
            if removed_column && col_idx < target {
                target = target.saturating_sub(1);
            }
            target = target.min(self.columns.len());
            if target >= self.columns.len() {
                self.columns.push(Column {
                    windows: vec![window],
                    width_offset: 0.0,
                    width_overridden: false,
                    height_weights: vec![1.0],
                height_overridden: false,
                });
            } else {
                self.columns[target].ensure_height_weights();
                self.columns[target].windows.push(window);
                // Equalise the column rather than carrying the old weight over.
                //
                // `weight` came from the column the window just left, where as the only
                // window it held that column's entire share. Pushed into a column of
                // 1.0-weighted windows it dominates: stacking two terminals collapsed the
                // existing one to its title bar while the newcomer took the rest.
                //
                // Weights are relative, so resetting the column to all-1.0 divides the
                // height evenly, which is what stacking should do. A deliberate vertical
                // resize afterwards still sets its own weights.
                let _ = weight;
                self.columns[target].equalise_heights();
            }
            self.selected = Some(window);
            self.align_scroll_to_selected();
        }
    }
}

impl Clone for LayoutState {
    fn clone(&self) -> Self {
        Self {
            columns: self.columns.clone(),
            selected: self.selected,
            column_width_ratio: self.column_width_ratio,
            scroll_offset_px: AtomicU64::new(self.scroll_offset_px.load(Ordering::Relaxed)),
            pending_align: AtomicBool::new(self.pending_align.load(Ordering::Relaxed)),
            pending_center_align: AtomicBool::new(
                self.pending_center_align.load(Ordering::Relaxed),
            ),
            pending_reveal_direction: AtomicI8::new(
                self.pending_reveal_direction.load(Ordering::Relaxed),
            ),
            center_override_window: self.center_override_window,
            last_screen_width: AtomicU64::new(self.last_screen_width.load(Ordering::Relaxed)),
            last_gap_x: AtomicU64::new(self.last_gap_x.load(Ordering::Relaxed)),
            last_step_px: AtomicU64::new(self.last_step_px.load(Ordering::Relaxed)),
            last_center_offset_delta_px: AtomicU64::new(
                self.last_center_offset_delta_px.load(Ordering::Relaxed),
            ),
            overscroll_accumulation: AtomicU64::new(
                self.overscroll_accumulation.load(Ordering::Relaxed),
            ),
            fullscreen_within_gaps: self.fullscreen_within_gaps.clone(),
            stack_origins: self.stack_origins.clone(),
        }
    }
}

fn default_atomic_bool() -> AtomicBool {
    AtomicBool::new(false)
}
fn default_atomic_i8() -> AtomicI8 {
    AtomicI8::new(0)
}
fn default_atomic() -> AtomicU64 {
    AtomicU64::new(0.0f64.to_bits())
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ScrollingLayoutSystem {
    layouts: slotmap::SlotMap<LayoutId, LayoutState>,
    #[serde(skip, default = "default_scrolling_settings")]
    settings: ScrollingLayoutSettings,
}

fn default_scrolling_settings() -> ScrollingLayoutSettings {
    ScrollingLayoutSettings::default()
}

impl Default for ScrollingLayoutSystem {
    fn default() -> Self {
        Self {
            layouts: Default::default(),
            settings: ScrollingLayoutSettings::default(),
        }
    }
}

impl ScrollingLayoutSystem {
    pub fn new(settings: &ScrollingLayoutSettings) -> Self {
        Self {
            layouts: Default::default(),
            settings: settings.clone(),
        }
    }

    pub fn update_settings(&mut self, settings: &ScrollingLayoutSettings) {
        // Rebase per-column width overrides when the DEFAULT ratio changes.
        //
        // Column widths are stored as `width_offset` relative to
        // `column_width_ratio`, so a restored layout keeps reproducing its old
        // absolute width after the default changes in config: the offset is
        // faithfully re-applied against a new base. Moving the ratio from 0.49 to 0.5
        // left every restored window at the old 842pt instead of the new 859pt — a
        // width matching no preset, so ctrl-R could never return a window to it and
        // every window looked subtly wrong.
        //
        // Columns the user deliberately resized (width_overridden) keep their chosen
        // absolute width by re-basing the offset. Everything else returns to the new
        // default.
        let ratio_changed =
            (self.settings.column_width_ratio - settings.column_width_ratio).abs() > 1e-9;
        let old_ratio = self.settings.column_width_ratio;

        self.settings = settings.clone();

        if ratio_changed {
            for state in self.layouts.values_mut() {
                state.column_width_ratio = settings.column_width_ratio;
                for column in &mut state.columns {
                    if column.width_overridden {
                        column.width_offset += old_ratio - settings.column_width_ratio;
                    } else {
                        column.width_offset = 0.0;
                    }
                }
            }
        }
    }

    fn insert_new_column(
        state: &mut LayoutState,
        wid: WindowId,
        insertion_point: WindowInsertionPoint,
    ) {
        if insertion_point == WindowInsertionPoint::EndOfTree {
            state.insert_column_at_end(wid);
        } else if let Some((col_idx, _)) = state.selected_location() {
            state.insert_column_after(col_idx, wid);
        } else if !state.columns.is_empty() {
            state.insert_column_after(0, wid);
        } else {
            state.insert_column_at_end(wid);
        }
    }

    fn clamp_ratio(&self, ratio: f64) -> f64 {
        ratio
            .clamp(
                self.settings.min_column_width_ratio,
                self.settings.max_column_width_ratio,
            )
            .max(0.05)
    }

    fn clamp_ratio_with_bounds(ratio: f64, min_ratio: f64, max_ratio: f64) -> f64 {
        ratio.clamp(min_ratio, max_ratio).max(0.05)
    }

    fn column_widths_and_starts(
        state: &LayoutState,
        screen_width: f64,
        gap_x: f64,
        min_ratio: f64,
        max_ratio: f64,
    ) -> (Vec<f64>, Vec<f64>) {
        let base_ratio =
            Self::clamp_ratio_with_bounds(state.column_width_ratio, min_ratio, max_ratio);
        let mut widths = Vec::with_capacity(state.columns.len());
        let mut starts = Vec::with_capacity(state.columns.len());
        let mut cursor = 0.0;
        for col in &state.columns {
            starts.push(cursor);
            let ratio =
                Self::clamp_ratio_with_bounds(base_ratio + col.width_offset, min_ratio, max_ratio);
            let width = (screen_width * ratio).max(1.0);
            widths.push(width);
            cursor += width + gap_x;
        }
        (widths, starts)
    }

    pub fn scroll_by_delta(&mut self, layout: LayoutId, delta: f64) -> Option<Direction> {
        let min_ratio = self.settings.min_column_width_ratio;
        let max_ratio = self.settings.max_column_width_ratio;
        let threshold = self.settings.gestures.workspace_switch_threshold;
        let Some(state) = self.layout_state_mut(layout) else {
            return None;
        };
        let screen_width = f64::from_bits(state.last_screen_width.load(Ordering::Relaxed));
        let gap_x = f64::from_bits(state.last_gap_x.load(Ordering::Relaxed));
        if screen_width <= 0.0 {
            return None;
        }
        let (widths, starts) =
            Self::column_widths_and_starts(state, screen_width, gap_x, min_ratio, max_ratio);
        if starts.is_empty() {
            return None;
        }
        let selected_idx = state.selected_location().map(|(idx, _)| idx).unwrap_or(0);
        let step = widths.get(selected_idx).copied().unwrap_or(1.0) + gap_x;
        if step <= 0.0 {
            return None;
        }
        let base_max_offset = starts.last().copied().unwrap_or(0.0);
        let center_offset_delta =
            f64::from_bits(state.last_center_offset_delta_px.load(Ordering::Relaxed));
        let (min_offset, max_offset) = if state.center_override_window.is_some() {
            (center_offset_delta, base_max_offset + center_offset_delta)
        } else {
            (0.0, base_max_offset)
        };
        let current = f64::from_bits(state.scroll_offset_px.load(Ordering::Relaxed));
        let next_raw = current + delta * step;
        let next = next_raw.clamp(min_offset, max_offset);
        state.scroll_offset_px.store(next.to_bits(), Ordering::Relaxed);

        if next_raw < min_offset && delta < 0.0 {
            let overscroll = (min_offset - next_raw) / step;
            let accum =
                f64::from_bits(state.overscroll_accumulation.load(Ordering::Relaxed)) + overscroll;
            if accum >= threshold {
                state.overscroll_accumulation.store(0.0f64.to_bits(), Ordering::Relaxed);
                Some(Direction::Left)
            } else {
                state.overscroll_accumulation.store(accum.to_bits(), Ordering::Relaxed);
                None
            }
        } else if next_raw > max_offset && delta > 0.0 {
            let overscroll = (next_raw - max_offset) / step;
            let accum =
                f64::from_bits(state.overscroll_accumulation.load(Ordering::Relaxed)) + overscroll;
            if accum >= threshold {
                state.overscroll_accumulation.store(0.0f64.to_bits(), Ordering::Relaxed);
                Some(Direction::Right)
            } else {
                state.overscroll_accumulation.store(accum.to_bits(), Ordering::Relaxed);
                None
            }
        } else {
            state.overscroll_accumulation.store(0.0f64.to_bits(), Ordering::Relaxed);
            None
        }
    }

    pub fn snap_to_nearest_column(&mut self, layout: LayoutId) {
        let min_ratio = self.settings.min_column_width_ratio;
        let max_ratio = self.settings.max_column_width_ratio;
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        let screen_width = f64::from_bits(state.last_screen_width.load(Ordering::Relaxed));
        let gap_x = f64::from_bits(state.last_gap_x.load(Ordering::Relaxed));
        if screen_width <= 0.0 {
            return;
        }
        let (_widths, starts) =
            Self::column_widths_and_starts(state, screen_width, gap_x, min_ratio, max_ratio);
        if starts.is_empty() {
            return;
        }
        let base_max_offset = starts.last().copied().unwrap_or(0.0);
        let center_offset_delta =
            f64::from_bits(state.last_center_offset_delta_px.load(Ordering::Relaxed));
        let (min_offset, max_offset, baseline) = if state.center_override_window.is_some() {
            (
                center_offset_delta,
                base_max_offset + center_offset_delta,
                center_offset_delta,
            )
        } else {
            (0.0, base_max_offset, 0.0)
        };
        let current = f64::from_bits(state.scroll_offset_px.load(Ordering::Relaxed));
        let strip_offset = current - baseline;
        let target = starts
            .iter()
            .min_by(|a, b| {
                let da = (*a - strip_offset).abs();
                let db = (*b - strip_offset).abs();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
            .unwrap_or(0.0);
        let next = (baseline + target).clamp(min_offset, max_offset);
        state.scroll_offset_px.store(next.to_bits(), Ordering::Relaxed);
    }

    pub fn center_selected_column(&mut self, layout: LayoutId) {
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        state.request_center_on_selected();
    }

    /// How far along the strip the viewport currently sits, in points.
    ///
    /// A window at strip position p is drawn at p minus this. The animation path uses the CHANGE in this
    /// number as the distance the strip has to travel, because it is the only description of the movement
    /// that does not depend on where each window really is: macOS clamps windows it will not place off
    /// screen, and the layout is recomputed several times per keystroke.
    pub fn scroll_offset(&self, layout: LayoutId) -> Option<f64> {
        self.layout_state(layout)
            .map(|state| f64::from_bits(state.scroll_offset_px.load(Ordering::Relaxed)))
    }

    fn layout_state(&self, layout: LayoutId) -> Option<&LayoutState> {
        self.layouts.get(layout)
    }

    fn layout_state_mut(&mut self, layout: LayoutId) -> Option<&mut LayoutState> {
        self.layouts.get_mut(layout)
    }

    fn move_focus_vertical(state: &mut LayoutState, dir: Direction) -> Option<WindowId> {
        let (col_idx, row_idx) = state.selected_location()?;
        let column = &state.columns[col_idx];
        if column.windows.is_empty() {
            return None;
        }
        let new_idx = match dir {
            Direction::Up => row_idx.checked_sub(1)?,
            Direction::Down => (row_idx + 1 < column.windows.len()).then_some(row_idx + 1)?,
            _ => return None,
        };
        let new_sel = column.windows[new_idx];
        state.selected = Some(new_sel);
        Some(new_sel)
    }

    fn move_focus_horizontal(state: &mut LayoutState, dir: Direction) -> Option<WindowId> {
        let (col_idx, row_idx) = state.selected_location()?;
        let target_col = match dir {
            Direction::Left => col_idx.checked_sub(1)?,
            Direction::Right => (col_idx + 1 < state.columns.len()).then_some(col_idx + 1)?,
            _ => return None,
        };
        let target_column = &state.columns[target_col];
        if target_column.windows.is_empty() {
            return None;
        }
        let target_row = row_idx.min(target_column.windows.len() - 1);
        let new_sel = target_column.windows[target_row];
        state.selected = Some(new_sel);
        Some(new_sel)
    }

    fn move_selected_window_vertical(state: &mut LayoutState, dir: Direction) -> bool {
        let (col_idx, row_idx) = match state.selected_location() {
            Some(loc) => loc,
            None => return false,
        };
        let column = &mut state.columns[col_idx];
        let target_idx = match dir {
            Direction::Up => row_idx.checked_sub(1),
            Direction::Down => (row_idx + 1 < column.windows.len()).then_some(row_idx + 1),
            _ => None,
        };
        let Some(target_idx) = target_idx else { return false };
        column.ensure_height_weights();
        column.windows.swap(row_idx, target_idx);
        column.height_weights.swap(row_idx, target_idx);
        state.selected = Some(column.windows[target_idx]);
        true
    }

    fn move_selected_window_horizontal(state: &mut LayoutState, dir: Direction) -> bool {
        let (col_idx, row_idx) = match state.selected_location() {
            Some(loc) => loc,
            None => return false,
        };
        // If the current column is stacked, horizontal move should extract the selected
        // window into its own neighbor column. This is a faster way to undo accidental stacks.
        if state.columns[col_idx].windows.len() > 1 {
            state.columns[col_idx].ensure_height_weights();
            let wid = state.columns[col_idx].windows.remove(row_idx);
            let weight = state.columns[col_idx].height_weights.remove(row_idx);
            let insert_at = match dir {
                Direction::Left => col_idx,
                Direction::Right => (col_idx + 1).min(state.columns.len()),
                _ => return false,
            };
            state.columns.insert(
                insert_at,
                Column {
                    windows: vec![wid],
                    width_offset: 0.0,
                    width_overridden: false,
                    height_weights: vec![weight],
                height_overridden: false,
                },
            );
            state.selected = Some(wid);
            return true;
        }

        let target_col = match dir {
            Direction::Left => col_idx.checked_sub(1),
            Direction::Right => (col_idx + 1 < state.columns.len()).then_some(col_idx + 1),
            _ => None,
        };
        let Some(target_col) = target_col else { return false };
        state.columns.swap(col_idx, target_col);
        let Some(selected) = state.selected else { return false };
        state.selected = Some(selected);
        true
    }

    fn all_windows(state: &LayoutState) -> Vec<WindowId> {
        state.columns.iter().flat_map(|c| c.windows.iter().copied()).collect()
    }
}

#[cfg(test)]
impl ScrollingLayoutSystem {
    /// Drop the selection, to check that commands which MOVE a window refuse to guess one.
    fn clear_selection_for_test(&mut self, layout: LayoutId) {
        if let Some(state) = self.layout_state_mut(layout) {
            state.selected = None;
        }
    }
}

impl ScrollingLayoutSystem {
    pub fn create_layout(&mut self) -> LayoutId {
        self.layouts.insert(LayoutState::new(self.settings.column_width_ratio))
    }

    pub fn contains_layout(&self, layout: LayoutId) -> bool {
        self.layouts.contains_key(layout)
    }

    pub fn clone_layout(&mut self, layout: LayoutId) -> LayoutId {
        let cloned = self
            .layouts
            .get(layout)
            .cloned()
            .unwrap_or_else(|| LayoutState::new(self.settings.column_width_ratio));
        self.layouts.insert(cloned)
    }

    pub fn remove_layout(&mut self, layout: LayoutId) {
        self.layouts.remove(layout);
    }

    pub fn draw_tree(&self, layout: LayoutId) -> String {
        let Some(state) = self.layouts.get(layout) else {
            return String::new();
        };
        let mut out = String::new();
        for (idx, col) in state.columns.iter().enumerate() {
            out.push_str(&format!("Column {idx}:"));
            for wid in &col.windows {
                if Some(*wid) == state.selected {
                    out.push_str(&format!(" [*{:?}]", wid));
                } else {
                    out.push_str(&format!(" [{:?}]", wid));
                }
            }
            out.push('\n');
        }
        out
    }

    /// Return a stable, platform-neutral view of the layout topology for IPC consumers.
    pub fn container_tree(&self, layout: LayoutId) -> rini_ipc::protocol::ContainerTreeNode {
        let state = self.layouts.get(layout).expect("unknown scrolling layout");
        let children = state
            .columns
            .iter()
            .map(|column| {
                let windows = column
                    .windows
                    .iter()
                    .enumerate()
                    .map(|(index, &window)| rini_ipc::protocol::ContainerTreeNode {
                        node_type: rini_ipc::protocol::ContainerNodeType::Window,
                        layout_kind: None,
                        weight: Some(column.height_weights.get(index).copied().unwrap_or(1.0)),
                        window_id: Some(window.into()),
                        is_selected: state.selected == Some(window),
                        is_fullscreen_within_gaps: state.fullscreen_within_gaps.contains(&window),
                        children: Vec::new(),
                    })
                    .collect();
                rini_ipc::protocol::ContainerTreeNode {
                    node_type: rini_ipc::protocol::ContainerNodeType::Container,
                    layout_kind: Some(rini_ipc::protocol::LayoutKind::Vertical),
                    weight: Some((state.column_width_ratio + column.width_offset).max(0.0)),
                    window_id: None,
                    is_selected: false,
                    is_fullscreen_within_gaps: false,
                    children: windows,
                }
            })
            .collect();

        rini_ipc::protocol::ContainerTreeNode {
            node_type: rini_ipc::protocol::ContainerNodeType::Container,
            layout_kind: Some(rini_ipc::protocol::LayoutKind::Horizontal),
            weight: None,
            window_id: None,
            is_selected: false,
            is_fullscreen_within_gaps: false,
            children,
        }
    }

    pub fn calculate_layout(
        &self,
        layout: LayoutId,
        screen: CGRect,
        constraints: &HashMap<WindowId, WindowLayoutConstraints>,
        gaps: &crate::layout::settings::GapSettings,
    ) -> Vec<(WindowId, CGRect)> {
        let Some(state) = self.layouts.get(layout) else {
            return Vec::new();
        };
        let tiling = compute_tiling_area(screen, gaps);
        let gap_x = gaps.inner.horizontal;
        let gap_y = gaps.inner.vertical;
        let base_ratio = self.clamp_ratio(state.column_width_ratio);

        let mut column_widths = Vec::with_capacity(state.columns.len());
        let mut column_ratios = Vec::with_capacity(state.columns.len());
        for col in state.columns.iter() {
            // A column holding a full-width ("within gaps") window occupies the
            // whole viewport, so it must be WIDTH 1.0 here as well as in the frame
            // assignment below. column_widths feeds column_starts, which is what
            // reserves horizontal space in the strip — without this the strip only
            // reserves the normal column width and the following column is laid
            // out on top of the full-width one.
            let holds_full_width =
                col.windows.iter().any(|wid| state.fullscreen_within_gaps.contains(wid));
            // A lone column keeps its width; expanding it tied a window's size to its neighbours.
            // See "Column width" in `src/layout/docs/strip.md`.
            let ratio = if holds_full_width {
                1.0
            } else {
                self.clamp_ratio(base_ratio + col.width_offset)
            };
            let base_width = (tiling.size.width * ratio).max(1.0);
            let mut min_w: f64 = 1.0;
            let mut fixed_w: Option<f64> = None;
            let mut max_w: Option<f64> = None;
            for wid in &col.windows {
                if let Some(c) = constraints.get(wid).copied() {
                    let c = c.normalized();
                    min_w = min_w.max(c.min_for_axis(true));
                    if let Some(locked) = c.fixed_for_axis(true) {
                        fixed_w = Some(match fixed_w {
                            Some(current) => current.max(locked),
                            None => locked,
                        });
                    }
                    if c.max_for_axis(true) > 0.0 {
                        max_w = Some(match max_w {
                            Some(current) => current.min(c.max_for_axis(true)),
                            None => c.max_for_axis(true),
                        });
                    }
                }
            }
            let required_w = fixed_w.unwrap_or(min_w).max(min_w);
            let mut width = base_width.max(required_w);
            if let Some(max_w) = max_w {
                width = width.min(max_w).max(required_w);
            }
            // Keep scrolling columns bounded to the tiling viewport. This layout
            // scrolls between column starts; it does not pan within a single
            // oversized column.
            width = width.min(tiling.size.width.max(1.0));

            let shrunk = width - gap_share(ratio, gap_x);
            if shrunk >= 1.0 {
                width = shrunk;
            }
            column_widths.push(width);
            column_ratios.push(if tiling.size.width > 0.0 {
                (width / tiling.size.width).max(0.0)
            } else {
                0.0
            });
        }

        let (column_starts, strip_max_offset) = column_starts(&column_widths, gap_x);
        let selected_col_idx = state.selected_location().map(|(idx, _)| idx).unwrap_or(0);
        let selected_width = column_widths
            .get(selected_col_idx)
            .copied()
            .unwrap_or((tiling.size.width * base_ratio).max(1.0));
        let step = selected_width + gap_x;
        state.last_screen_width.store(tiling.size.width.to_bits(), Ordering::Relaxed);
        state.last_gap_x.store(gap_x.to_bits(), Ordering::Relaxed);
        state.last_step_px.store(step.to_bits(), Ordering::Relaxed);

        let anchor_x = anchor_x(
            tiling,
            selected_width,
            selected_col_idx,
            state.columns.len(),
            self.settings.alignment,
            self.settings.focus_navigation_style,
            state.center_override_window.is_some(),
        );
        let center_anchor_x = tiling.origin.x + (tiling.size.width - selected_width) / 2.0;
        let center_offset_delta = anchor_x - center_anchor_x;
        state
            .last_center_offset_delta_px
            .store(center_offset_delta.to_bits(), Ordering::Relaxed);

        if state.pending_center_align.load(Ordering::Relaxed) {
            let offset = state
                .selected_location()
                .map(|(col_idx, _)| {
                    center_offset_delta + column_starts.get(col_idx).copied().unwrap_or(0.0)
                })
                .unwrap_or(0.0);
            state.scroll_offset_px.store(offset.to_bits(), Ordering::Relaxed);
            state.pending_center_align.store(false, Ordering::Relaxed);
            state.pending_align.store(false, Ordering::Relaxed);
        } else if state.pending_align.load(Ordering::Relaxed) {
            let offset = state
                .selected_location()
                .map(|(col_idx, _)| column_starts.get(col_idx).copied().unwrap_or(0.0))
                .unwrap_or(0.0);
            state.scroll_offset_px.store(offset.to_bits(), Ordering::Relaxed);
            state.pending_align.store(false, Ordering::Relaxed);
        }
        let reveal_direction = state.pending_reveal_direction.swap(0, Ordering::Relaxed);
        if reveal_direction != 0 {
            if let Some((selected_col_idx, _)) = state.selected_location() {
                let selected_width = column_widths
                    .get(selected_col_idx)
                    .copied()
                    .unwrap_or((tiling.size.width * base_ratio).max(1.0));
                let mut offset = f64::from_bits(state.scroll_offset_px.load(Ordering::Relaxed));
                let selected_start = column_starts.get(selected_col_idx).copied().unwrap_or(0.0);

                let reveal = match reveal_direction {
                    -1 => Some(Reveal::FromRight),
                    1 => Some(Reveal::FromLeft),
                    2 => Some(Reveal::Either),
                    _ => None,
                };
                if let Some(reveal) = reveal
                    && let Some(corrected) =
                        reveal_offset(reveal, tiling, anchor_x, selected_start, selected_width, offset)
                {
                    offset = corrected;
                }
                state.scroll_offset_px.store(offset.to_bits(), Ordering::Relaxed);
            }
        }
        let current = f64::from_bits(state.scroll_offset_px.load(Ordering::Relaxed));
        let base_max_offset = strip_max_offset;
        let (min_offset, max_offset) = if state.center_override_window.is_some() {
            (center_offset_delta, base_max_offset + center_offset_delta)
        } else {
            (0.0, base_max_offset)
        };
        let clamped = current.clamp(min_offset, max_offset);
        state.scroll_offset_px.store(clamped.to_bits(), Ordering::Relaxed);

        let mut out = Vec::new();
        for (col_idx, col) in state.columns.iter().enumerate() {
            let offset = f64::from_bits(state.scroll_offset_px.load(Ordering::Relaxed));
            let ratio = column_ratios.get(col_idx).copied().unwrap_or(base_ratio);
            let column_width = column_widths
                .get(col_idx)
                .copied()
                .unwrap_or((tiling.size.width * ratio).max(1.0));
            let start = column_starts.get(col_idx).copied().unwrap_or(0.0);
            let x = anchor_x + start - offset;
            if col.windows.is_empty() {
                continue;
            }
            let total_gap = gap_y * (col.windows.len().saturating_sub(1) as f64);
            let available_height = (tiling.size.height - total_gap).max(0.0);
            let row_constraints: Vec<AxisConstraints> = col
                .windows
                .iter()
                .enumerate()
                .map(|(row_idx, wid)| {
                    let (min, fixed, max, can_grow) = constraints
                        .get(wid)
                        .copied()
                        .map(|c| {
                            let c = c.normalized();
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
                    // minima are pixels, so `1.0 - min` floors at 0.001 for any window macOS
                    // reported a minimum height for, and 1.0 for any it did not. A folded pair with
                    // one reported minimum split 700/100 instead of 400/400: the window with the
                    // minimum was left at it, which on screen is its title bar. It only happened
                    // when the two windows disagreed about having a minimum, which is why it came
                    // and went.
                    //
                    // After a deliberate vertical resize the weights ARE desired pixel heights, and
                    // then the subtraction is right: the solver reserves every minimum first and
                    // shares out the remainder, so weights of [900, 100] against minima of
                    // [100, 100] have to become [800, 0] to land on 900/100.
                    let weight = if col.height_overridden {
                        let raw_weight = col.height_weights.get(row_idx).copied().unwrap_or(1.0);
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
            let solved_row_heights = solve_axis_lengths(&row_constraints, available_height);
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
                if state.fullscreen_within_gaps.contains(wid) {
                    // The tiling rect's SIZE at the column's own x: keeping the strip-relative x is
                    // what lets the column keep scrolling with the strip (`src/layout/docs/strip.md`).
                    frame = CGRect::new(
                        CGPoint::new(x.round(), tiling.origin.y.round()),
                        CGSize::new(tiling.size.width.round(), tiling.size.height.round()),
                    );
                }
                if let Some(c) = constraints.get(wid).copied() {
                    frame.size = clamp_to_constraints(frame.size, c);
                }
                out.push((*wid, frame));
                y_cursor += row_height;
                if row_idx < col.windows.len() - 1 {
                    y_cursor += gap_y;
                }
            }
        }
        out
    }

    pub fn selected_window(&self, layout: LayoutId) -> Option<WindowId> {
        self.layout_state(layout).and_then(|state| state.selected_or_first())
    }

    /// Return every window stored in this layout, including members hidden by a stack.
    /// Persistence validation must not confuse "currently visible" with "serialized" or an
    /// unmatchable hidden member can survive forever as a ghost.
    pub fn all_windows_in_layout(&self, layout: LayoutId) -> Vec<WindowId> {
        self.layout_state(layout).map(Self::all_windows).unwrap_or_default()
    }

    pub fn visible_windows_in_layout(&self, layout: LayoutId) -> Vec<WindowId> {
        self.layout_state(layout).map(Self::all_windows).unwrap_or_default()
    }

    pub fn visible_windows_under_selection(&self, layout: LayoutId) -> Vec<WindowId> {
        let Some(state) = self.layout_state(layout) else {
            return Vec::new();
        };
        let Some((col_idx, _)) = state.selected_location() else {
            return Vec::new();
        };
        state.columns[col_idx].windows.clone()
    }


    pub fn move_focus(
        &mut self,
        layout: LayoutId,
        direction: Direction,
    ) -> (Option<WindowId>, Vec<WindowId>) {
        let niri_navigation = matches!(
            self.settings.focus_navigation_style,
            ScrollingFocusNavigationStyle::Niri
        );
        let Some(state) = self.layout_state_mut(layout) else {
            return (None, vec![]);
        };
        let new_sel = match direction {
            Direction::Left | Direction::Right => Self::move_focus_horizontal(state, direction),
            Direction::Up | Direction::Down => Self::move_focus_vertical(state, direction),
        };
        if new_sel.is_some() && niri_navigation {
            if matches!(direction, Direction::Left | Direction::Right) {
                state.reveal_selected_in_direction(direction);
            } else {
                state.reveal_selected_without_direction();
            }
        } else {
            state.align_scroll_to_selected();
        }
        let raise = state
            .selected_location()
            .map(|(col_idx, _)| state.columns[col_idx].windows.clone())
            .unwrap_or_default();
        (new_sel, raise)
    }

    pub fn window_in_direction(&self, layout: LayoutId, direction: Direction) -> Option<WindowId> {
        let state = self.layout_state(layout)?;
        let (col_idx, row_idx) = state.selected_location()?;
        match direction {
            Direction::Left => {
                let target = col_idx.checked_sub(1)?;
                state.columns.get(target).and_then(|col| {
                    col.windows.get(row_idx.min(col.windows.len().saturating_sub(1))).copied()
                })
            }
            Direction::Right => {
                let target = col_idx + 1;
                state.columns.get(target).and_then(|col| {
                    col.windows.get(row_idx.min(col.windows.len().saturating_sub(1))).copied()
                })
            }
            Direction::Up => {
                state.columns.get(col_idx)?.windows.get(row_idx.checked_sub(1)?).copied()
            }
            Direction::Down => state.columns.get(col_idx)?.windows.get(row_idx + 1).copied(),
        }
    }

    pub fn add_window_after_selection(&mut self, layout: LayoutId, wid: WindowId) {
        let niri_navigation = matches!(
            self.settings.focus_navigation_style,
            ScrollingFocusNavigationStyle::Niri
        );
        let insertion_point = self.settings.base.window_insertion_point.unwrap_or_default();
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        Self::insert_new_column(state, wid, insertion_point);
        if niri_navigation {
            state.reveal_selected_without_direction();
        }
    }

    /// Replace a window identity in-place without changing its layout position.
    pub fn replace_window(&mut self, from: WindowId, to: WindowId) {
        if from == to {
            return;
        }
        for state in self.layouts.values_mut() {
            for column in &mut state.columns {
                for window in &mut column.windows {
                    if *window == from {
                        *window = to;
                    }
                }
            }
            if state.selected == Some(from) {
                state.selected = Some(to);
            }
            if state.center_override_window == Some(from) {
                state.center_override_window = Some(to);
            }
            if state.fullscreen_within_gaps.remove(&from) {
                state.fullscreen_within_gaps.insert(to);
            }
            if let Some(origin) = state.stack_origins.remove(&from) {
                state.stack_origins.insert(to, origin);
            }
            for origin in state.stack_origins.values_mut() {
                if origin.anchor == from {
                    origin.anchor = to;
                }
            }
        }
    }

    pub fn remove_window(&mut self, wid: WindowId) {
        for state in self.layouts.values_mut() {
            let _ = state.remove_window(wid);
        }
    }

    /// The width a window's column carries, if it has been sized away from the default.
    ///
    /// Needed so moving a window between workspaces or displays keeps the width the user gave
    /// it; a fresh column otherwise starts at the default ratio and the window appeared to
    /// reset to 50%.
    pub fn column_width_offset(&self, layout: LayoutId, wid: WindowId) -> Option<f64> {
        let state = self.layout_state(layout)?;
        let (col_idx, _) = state.locate(wid)?;
        let column = state.columns.get(col_idx)?;
        // Only a deliberately sized column is worth carrying. An untouched one should adopt
        // the destination's default, which may differ (a wider display, a different config).
        column.width_overridden.then_some(column.width_offset)
    }

    /// Re-apply a width carried from another column.
    pub fn set_column_width_offset(&mut self, layout: LayoutId, wid: WindowId, offset: f64) {
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        let Some((col_idx, _)) = state.locate(wid) else {
            return;
        };
        if let Some(column) = state.columns.get_mut(col_idx) {
            column.width_offset = offset;
            column.width_overridden = true;
        }
    }

    /// Whether this window's column occupies the whole viewport width.
    ///
    /// Separate from `column_width_offset` because full width is a MODE rather than a ratio:
    /// it stays full on a display of any size, so it cannot be represented as an offset from
    /// the configured default and survive a move between displays.
    pub fn is_window_full_width(&self, layout: LayoutId, wid: WindowId) -> bool {
        self.layout_state(layout)
            .is_some_and(|state| state.fullscreen_within_gaps.contains(&wid))
    }

    /// Set or clear the full-viewport-width mode for a window's column.
    pub fn set_window_full_width(&mut self, layout: LayoutId, wid: WindowId, full: bool) {
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        if state.locate(wid).is_none() {
            return;
        }
        if full {
            state.fullscreen_within_gaps.insert(wid);
        } else {
            state.fullscreen_within_gaps.remove(&wid);
        }
    }

    /// Remove a window from ONE layout only.
    ///
    /// `remove_window` spans every layout the system owns. A workspace now owns one layout
    /// (strip) per display, so normalizing a single display's strip must not reach into the
    /// others — doing so deleted windows from the display they legitimately sat on.
    pub fn remove_window_from_layout(&mut self, layout: LayoutId, wid: WindowId) {
        if let Some(state) = self.layouts.get_mut(layout) {
            let _ = state.remove_window(wid);
        }
    }

    pub fn remove_windows_for_app(&mut self, pid: pid_t) {
        for state in self.layouts.values_mut() {
            let windows: Vec<_> = state
                .columns
                .iter()
                .flat_map(|c| c.windows.iter().copied())
                .filter(|w| w.pid == pid)
                .collect();
            for wid in windows {
                let _ = state.remove_window(wid);
            }
        }
    }

    pub fn windows_for_app(&self, layout: LayoutId, pid: pid_t) -> Vec<WindowId> {
        self.layout_state(layout)
            .map(|state| {
                state
                    .columns
                    .iter()
                    .flat_map(|c| c.windows.iter().copied())
                    .filter(|w| w.pid == pid)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn set_windows_for_app(&mut self, layout: LayoutId, pid: pid_t, desired: Vec<WindowId>) {
        let niri_navigation = matches!(
            self.settings.focus_navigation_style,
            ScrollingFocusNavigationStyle::Niri
        );
        let insertion_point = self.settings.base.window_insertion_point.unwrap_or_default();
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        let mut desired = desired;
        desired.sort_unstable();
        let current: Vec<_> = state
            .columns
            .iter()
            .flat_map(|c| c.windows.iter().copied())
            .filter(|w| w.pid == pid)
            .collect();
        let mut current = current;
        current.sort_unstable();
        let mut desired_iter = desired.iter().peekable();
        let mut current_iter = current.iter().peekable();
        loop {
            match (desired_iter.peek(), current_iter.peek()) {
                (Some(des), Some(cur)) if des == cur => {
                    desired_iter.next();
                    current_iter.next();
                }
                (Some(des), None) => {
                    Self::insert_new_column(state, **des, insertion_point);
                    desired_iter.next();
                }
                (Some(des), Some(cur)) if des < cur => {
                    Self::insert_new_column(state, **des, insertion_point);
                    desired_iter.next();
                }
                (_, Some(cur)) => {
                    let _ = state.remove_window(**cur);
                    current_iter.next();
                }
                (None, None) => break,
            }
        }
        if niri_navigation {
            state.reveal_selected_without_direction();
        }
    }

    pub fn has_windows_for_app(&self, layout: LayoutId, pid: pid_t) -> bool {
        self.layout_state(layout)
            .map(|state| state.columns.iter().flat_map(|c| c.windows.iter()).any(|w| w.pid == pid))
            .unwrap_or(false)
    }

    pub fn contains_window(&self, layout: LayoutId, wid: WindowId) -> bool {
        self.layout_state(layout)
            .map(|state| state.locate(wid).is_some())
            .unwrap_or(false)
    }

    pub fn select_window(&mut self, layout: LayoutId, wid: WindowId) -> bool {
        let niri_navigation = matches!(
            self.settings.focus_navigation_style,
            ScrollingFocusNavigationStyle::Niri
        );
        let Some(state) = self.layout_state_mut(layout) else {
            return false;
        };
        if state.locate(wid).is_some() {
            // refocusing the same centered window should keep the center override
            if state.selected == Some(wid) && state.center_override_window == Some(wid) {
                return true;
            }
            state.selected = Some(wid);
            if niri_navigation {
                state.reveal_selected_without_direction();
            } else {
                state.align_scroll_to_selected();
            }
            true
        } else {
            false
        }
    }

    pub fn on_window_resized(
        &mut self,
        layout: LayoutId,
        wid: WindowId,
        _old_frame: CGRect,
        new_frame: CGRect,
        screen: CGRect,
        gaps: &crate::layout::settings::GapSettings,
    ) {
        let min_ratio = self.settings.min_column_width_ratio;
        let max_ratio = self.settings.max_column_width_ratio;
        let niri_navigation = matches!(
            self.settings.focus_navigation_style,
            ScrollingFocusNavigationStyle::Niri
        );

        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        if state.selected != Some(wid) {
            return;
        }
        let tiling = compute_tiling_area(screen, gaps);
        if tiling.size.width <= 0.0 {
            return;
        }
        let ratio = new_frame.size.width / tiling.size.width;
        let clamped = ratio.clamp(min_ratio, max_ratio).max(0.05);

        let base_ratio = state.column_width_ratio;
        let Some((col_idx, row_idx)) = state.locate(wid) else {
            return;
        };
        state.columns[col_idx].width_offset = clamped - base_ratio;
        state.columns[col_idx].width_overridden = true;

        // Handle vertical resizing within columns
        let col = &mut state.columns[col_idx];
        if col.windows.len() > 1 && tiling.size.height > 0.0 {
            col.ensure_height_weights();
            let total_gap = gaps.inner.vertical * (col.windows.len().saturating_sub(1) as f64);
            let available_height = (tiling.size.height - total_gap).max(0.0);

            if available_height > 0.0 {
                col.height_overridden = true;
                // Set weights directly to desired pixel heights. The constraint
                // solver (`solve_axis_lengths`) first reserves each window's
                // minimum height, then distributes the remainder proportionally
                // by weight.  Using actual pixel heights as weights means the
                // solver will naturally produce the user's intended split,
                // subject only to the macOS-reported min/max constraints.
                let new_resized_height = new_frame.size.height.max(1.0);
                let new_other_total = (available_height - new_resized_height).max(1.0);

                // Distribute the "other" portion among non-resized windows,
                // preserving their relative proportions.
                let other_weight_sum: f64 = col
                    .height_weights
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| i != row_idx)
                    .map(|(_, &w)| w)
                    .sum();

                if other_weight_sum > 0.0 {
                    for i in 0..col.windows.len() {
                        if i == row_idx {
                            col.height_weights[i] = new_resized_height;
                        } else {
                            // Scale each other window's weight so their sum equals new_other_total.
                            col.height_weights[i] =
                                new_other_total * (col.height_weights[i] / other_weight_sum);
                        }
                    }
                } else {
                    // Fallback: no prior weights for other windows.
                    let other_count = (col.windows.len() - 1).max(1) as f64;
                    let share = new_other_total / other_count;
                    for i in 0..col.windows.len() {
                        if i == row_idx {
                            col.height_weights[i] = new_resized_height;
                        } else {
                            col.height_weights[i] = share;
                        }
                    }
                }
            }
        }

        if niri_navigation && state.selected == Some(wid) {
            state.reveal_selected_without_direction();
        } else if state.selected == Some(wid) {
            state.align_scroll_to_selected();
        }
    }

    pub fn swap_windows(&mut self, layout: LayoutId, a: WindowId, b: WindowId) -> bool {
        let Some(state) = self.layout_state_mut(layout) else {
            return false;
        };
        let (a_col, a_row) = match state.locate(a) {
            Some(loc) => loc,
            None => return false,
        };
        let (b_col, b_row) = match state.locate(b) {
            Some(loc) => loc,
            None => return false,
        };
        if a_col == b_col {
            state.columns[a_col].ensure_height_weights();
            state.columns[a_col].windows.swap(a_row, b_row);
            state.columns[a_col].height_weights.swap(a_row, b_row);
        } else {
            state.columns[a_col].ensure_height_weights();
            state.columns[b_col].ensure_height_weights();
            let a_window = state.columns[a_col].windows[a_row];
            let b_window = state.columns[b_col].windows[b_row];
            state.columns[a_col].windows[a_row] = b_window;
            state.columns[b_col].windows[b_row] = a_window;

            let a_weight = state.columns[a_col].height_weights[a_row];
            let b_weight = state.columns[b_col].height_weights[b_row];
            state.columns[a_col].height_weights[a_row] = b_weight;
            state.columns[b_col].height_weights[b_row] = a_weight;
        }
        true
    }

    pub fn move_selection(&mut self, layout: LayoutId, direction: Direction) -> bool {
        let niri_navigation = matches!(
            self.settings.focus_navigation_style,
            ScrollingFocusNavigationStyle::Niri
        );
        let Some(state) = self.layout_state_mut(layout) else {
            return false;
        };
        let moved = match direction {
            Direction::Left | Direction::Right => {
                Self::move_selected_window_horizontal(state, direction)
            }
            Direction::Up | Direction::Down => {
                Self::move_selected_window_vertical(state, direction)
            }
        };
        if moved {
            if niri_navigation {
                state.reveal_selected_without_direction();
            } else {
                state.align_scroll_to_selected();
            }
        }
        moved
    }


    /// Fold the selection into the column on `side`, or back out of the column it is in.
    ///
    /// `side` names the column this key works with, so one binding per side gives symmetric control.
    /// There is no falling back to the other side: with both keys bound that would make them agree
    /// at the ends of the strip.
    ///
    /// Pressing the same key twice puts the window back, which takes two different mechanisms.
    /// Folding OUT lands the new column on the side AWAY from `side`, because that is where the
    /// window was before it folded into that column. Folding IN prefers the row it was folded out
    /// of (`StackOrigin`) over appending to the end, so the rows keep their order too.
    pub fn toggle_fold_of_selection(&mut self, layout: LayoutId, side: Direction) -> Vec<WindowId> {
        let away = match side {
            Direction::Left => Direction::Right,
            Direction::Right => Direction::Left,
            _ => return Vec::new(),
        };
        let niri_navigation = matches!(
            self.settings.focus_navigation_style,
            ScrollingFocusNavigationStyle::Niri
        );
        let Some(state) = self.layout_state_mut(layout) else {
            return Vec::new();
        };
        // The real selection, with no fall back to the first window. `selected_or_first` is fine
        // for a query, but this MOVES a window: acting on the top of the strip because the
        // selection was momentarily unset would fold a window the user is not looking at.
        // `consume_or_expel_selection` does nothing in that state and so does this.
        let Some((col_idx, row_idx)) = state.selected_location() else {
            return Vec::new();
        };
        let selected = state.columns[col_idx].windows[row_idx];

        if state.columns[col_idx].windows.len() > 1 {
            if let Some(origin) = state.split_out(selected, away) {
                state.stack_origins.insert(selected, origin);
            }
        } else {
            let restored = match state.stack_origins.remove(&selected) {
                Some(origin) => state.restore_into_stack(selected, origin),
                None => false,
            };
            if !restored {
                let target = match side {
                    Direction::Left => col_idx.checked_sub(1),
                    _ => (col_idx + 1 < state.columns.len()).then_some(col_idx + 1),
                };
                let Some(target) = target else {
                    return Vec::new();
                };
                state.move_window_to_column_end(selected, target);
            }
        }

        state.selected = Some(selected);
        if niri_navigation {
            state.reveal_selected_without_direction();
        } else {
            state.align_scroll_to_selected();
        }
        vec![selected]
    }

    pub fn toggle_fullscreen_within_gaps_of_selection(&mut self, layout: LayoutId) -> Vec<WindowId> {
        let niri_navigation = matches!(
            self.settings.focus_navigation_style,
            ScrollingFocusNavigationStyle::Niri
        );
        let Some(state) = self.layout_state_mut(layout) else {
            return Vec::new();
        };
        let Some(selected) = state.selected_or_first() else {
            return Vec::new();
        };

        if state.fullscreen_within_gaps.remove(&selected) {
            // Back where it came from, if that place still exists. A window that was alone in its
            // column has no origin and simply stops being maximized.
            if let Some(origin) = state.stack_origins.remove(&selected) {
                state.restore_into_stack(selected, origin);
            }
        } else {
            // A maximized window fills the tiling area, which would cover the siblings sharing its
            // column. Pull it out first, so what is on screen matches what the tree says.
            if let Some(origin) = state.split_out(selected, Direction::Right) {
                state.stack_origins.insert(selected, origin);
            }
            state.fullscreen_within_gaps.insert(selected);
        }
        state.selected = Some(selected);

        // Rescroll so the resized column stays visible, as the resize path does.
        if niri_navigation {
            state.reveal_selected_without_direction();
        } else {
            state.align_scroll_to_selected();
        }

        vec![selected]
    }

    /// Cycle the selected column through `preset_column_widths`.
    ///
    /// niri's switch-preset-column-width. Unlike ResizeWindowGrow/Shrink, which
    /// step by a fixed ~5% and leave columns at arbitrary in-between widths, this
    /// snaps to a known set — so every column ends up at one of a few predictable
    /// sizes instead of drifting.
    ///
    /// Widths are stored as `width_offset` relative to `column_width_ratio`, the
    /// same representation the resize path uses, so nothing else needs to know
    /// these came from a preset.
    pub fn cycle_preset_column_width(&mut self, layout: LayoutId) -> Vec<WindowId> {
        let presets: Vec<f64> = self
            .settings
            .preset_column_widths
            .iter()
            .copied()
            .filter(|r| *r > 0.0 && *r <= 1.0)
            .collect();
        if presets.is_empty() {
            return Vec::new();
        }
        let niri_navigation = matches!(
            self.settings.focus_navigation_style,
            ScrollingFocusNavigationStyle::Niri
        );

        let Some(state) = self.layout_state_mut(layout) else {
            return Vec::new();
        };
        let Some(selected) = state.selected_or_first() else {
            return Vec::new();
        };
        let Some((col_idx, _)) = state.locate(selected) else {
            return Vec::new();
        };

        let base_ratio = state.column_width_ratio;
        let current = base_ratio + state.columns[col_idx].width_offset;

        // Advance to the first preset meaningfully wider than the current width,
        // wrapping to the narrowest. The 1% epsilon stops floating-point noise
        // (and the rounding applied when frames are written) from making the
        // current width look like it is already just past a preset, which would
        // skip an entry.
        let next = presets.iter().copied().find(|p| *p > current + 0.01).unwrap_or(presets[0]);

        state.columns[col_idx].width_offset = next - base_ratio;
        state.columns[col_idx].width_overridden = true;

        // A width change moves every column start after it, so the strip has to be
        // rescrolled or a column at the viewport edge grows off-screen. Same
        // reasoning as the fullscreen toggle above.
        if niri_navigation {
            state.reveal_selected_without_direction();
        } else {
            state.align_scroll_to_selected();
        }

        vec![selected]
    }


    pub fn apply_stacking_to_parent_of_selection(&mut self, layout: LayoutId) -> Vec<WindowId> {
        let Some(state) = self.layout_state_mut(layout) else {
            return Vec::new();
        };
        let (col_idx, _) = match state.selected_location() {
            Some(loc) => loc,
            None => return Vec::new(),
        };
        let Some(selected) = state.selected else {
            return Vec::new();
        };

        // Move the SELECTED window into the PREVIOUS column.
        //
        // Was the other way round: it took the NEXT column's windows
        // (col_idx + 1) and pulled them into the current one, so pressing the
        // stack key stacked a window you had not chosen underneath the one you
        // were looking at, and the selection stayed put. Confusing, and the
        // opposite of niri's consume-window-into-column, which moves the current
        // window into the column to its left.
        //
        // Only fall back to the next column when the selection is already in the
        // first column, so the key still does something useful at the left edge.
        let target_col = if col_idx > 0 {
            col_idx - 1
        } else if col_idx + 1 < state.columns.len() {
            col_idx + 1
        } else {
            return Vec::new();
        };

        // Nothing to do if the selected window is the only one in its column and
        // that column IS the target (cannot stack a window onto itself).
        if state.columns[col_idx].windows.len() == 1 && target_col == col_idx {
            return Vec::new();
        }

        state.move_window_to_column_end(selected, target_col);
        vec![selected]
    }

    pub fn unstack_parent_of_selection(&mut self, layout: LayoutId) -> Vec<WindowId> {
        let Some(state) = self.layout_state_mut(layout) else {
            return Vec::new();
        };
        let Some((col_idx, row_idx)) = state.selected_location() else {
            return Vec::new();
        };
        if state.columns[col_idx].windows.len() <= 1 {
            return Vec::new();
        }
        let selected = state.columns[col_idx].windows[row_idx];
        // Every window EXCEPT the selected one leaves, each to its own column, keeping their order.
        // Splitting them right to left means each lands immediately after the column being emptied,
        // so the row order becomes the column order.
        let others: Vec<WindowId> = state.columns[col_idx]
            .windows
            .iter()
            .copied()
            .filter(|wid| *wid != selected)
            .collect();
        for wid in others.iter().rev() {
            state.split_out(*wid, Direction::Right);
        }
        others
    }

    pub fn parent_of_selection_is_stacked(&self, layout: LayoutId) -> bool {
        let Some(state) = self.layout_state(layout) else {
            return false;
        };
        let Some((col_idx, _)) = state.selected_location() else {
            return false;
        };
        state.columns[col_idx].windows.len() > 1
    }

    pub fn resize_selection_by(
        &mut self,
        layout: LayoutId,
        amount: f64,
        orientation: ResizeOrientation,
    ) {
        let min_ratio = self.settings.min_column_width_ratio;
        let max_ratio = self.settings.max_column_width_ratio;
        let niri_navigation = matches!(
            self.settings.focus_navigation_style,
            ScrollingFocusNavigationStyle::Niri
        );
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        let base_ratio = state.column_width_ratio;

        let Some((col_idx, row_idx)) = state.selected_location() else {
            if orientation == ResizeOrientation::Vertical {
                return;
            }
            let ratio = base_ratio + amount;
            state.column_width_ratio = ratio.clamp(min_ratio, max_ratio).max(0.05);
            return;
        };

        let resize_vertically = orientation == ResizeOrientation::Vertical
            || (orientation == ResizeOrientation::Smart
                && state.columns[col_idx].windows.len() > 1);
        if resize_vertically {
            let column = &mut state.columns[col_idx];
            if column.windows.len() < 2 {
                return;
            }
            column.ensure_height_weights();
            let total: f64 = column.height_weights.iter().sum();
            if total <= f64::EPSILON {
                return;
            }
            // A deliberate split from here on: the weights below are pixel-ish shares, not the
            // equal ratios a folded column starts at.
            column.height_overridden = true;
            let current_share = column.height_weights[row_idx] / total;
            let next_share = (current_share + amount).clamp(0.05, 0.95);
            let other_total = (total - column.height_weights[row_idx]).max(f64::EPSILON);
            let scale = total.max(10_000.0);
            for (idx, weight) in column.height_weights.iter_mut().enumerate() {
                if idx == row_idx {
                    *weight = next_share * scale;
                } else {
                    *weight = (1.0 - next_share) * scale * (*weight / other_total);
                }
            }
            return;
        }

        let current = base_ratio + state.columns[col_idx].width_offset;
        let next = current + amount;
        let clamped = next.clamp(min_ratio, max_ratio).max(0.05);
        state.columns[col_idx].width_offset = clamped - base_ratio;
        state.columns[col_idx].width_overridden = true;
        if niri_navigation {
            state.reveal_selected_without_direction();
        } else {
            state.align_scroll_to_selected();
        }
    }

}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use objc2_core_foundation::{CGPoint, CGRect, CGSize};

    use super::{Column, ScrollingLayoutSystem};
    use rini_core::ids::{WindowId, pid_t};
    use rustc_hash::FxHashMap as HashMap;
    use crate::layout::settings::{GapSettings, ScrollingLayoutSettings, WindowInsertionPoint};
    use crate::layout::WindowLayoutConstraints;
    use crate::layout::domain::area::compute_tiling_area;
    use crate::layout::{Direction, LayoutId, ResizeOrientation};

    fn wid(pid: pid_t, idx: u32) -> WindowId {
        WindowId {
            pid,
            idx: std::num::NonZeroU32::new(idx).unwrap(),
        }
    }

    fn screen(width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width, height))
    }

    fn render(
        system: &ScrollingLayoutSystem,
        layout: LayoutId,
        screen: CGRect,
        gaps: &GapSettings,
    ) -> Vec<(WindowId, CGRect)> {
        let constraints = HashMap::default();
        system.calculate_layout(
            layout,
            screen,
            &constraints,
            gaps,
        )
    }

    fn constraints_none() -> HashMap<WindowId, WindowLayoutConstraints> {
        HashMap::default()
    }

    fn frame_for(frames: &[(WindowId, CGRect)], wid: WindowId) -> CGRect {
        frames
            .iter()
            .find(|(id, _)| *id == wid)
            .map(|(_, frame)| *frame)
            .expect("missing frame")
    }

    fn scroll_offset(system: &ScrollingLayoutSystem, layout: LayoutId) -> f64 {
        f64::from_bits(
            system
                .layouts
                .get(layout)
                .expect("layout state missing")
                .scroll_offset_px
                .load(Ordering::Relaxed),
        )
    }

    fn setup_two_windows(
        settings: ScrollingLayoutSettings,
    ) -> (ScrollingLayoutSystem, LayoutId, WindowId, WindowId) {
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);
        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);
        (system, layout, w1, w2)
    }

    #[test]
    fn respects_min_width_and_min_height_independently() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let window = wid(10, 1);
        system.add_window_after_selection(layout, window);

        let mut constraints = HashMap::default();
        constraints.insert(
            window,
            WindowLayoutConstraints {
                is_resizable: true,
                locked_width: 0.0,
                locked_height: 0.0,
                min_width: 700.0,
                min_height: 500.0,
                max_width: 0.0,
                max_height: 0.0,
            }
            .normalized(),
        );

        let frames = system.calculate_layout(
            layout,
            screen(800.0, 600.0),
            &constraints,
            &GapSettings::default(),
        );
        let frame = frame_for(&frames, window);
        assert!(frame.size.width >= 699.0);
        assert!(frame.size.height >= 499.0);
    }

    #[test]
    fn row_constraints_apply_vertical_min_independent_of_column_width() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let w1 = wid(20, 1);
        let w2 = wid(20, 2);
        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);

        let state = system.layouts.get_mut(layout).expect("layout state missing");
        state.columns = vec![Column {
            windows: vec![w1, w2],
            width_offset: 0.0,
            width_overridden: false,
            height_weights: vec![1.0, 1.0],
        height_overridden: false,
        }];
        state.selected = Some(w1);

        let mut constraints = HashMap::default();
        constraints.insert(
            w1,
            WindowLayoutConstraints {
                is_resizable: true,
                locked_width: 0.0,
                locked_height: 0.0,
                min_width: 500.0,
                min_height: 350.0,
                max_width: 0.0,
                max_height: 0.0,
            }
            .normalized(),
        );

        let frames = system.calculate_layout(
            layout,
            screen(700.0, 600.0),
            &constraints,
            &GapSettings::default(),
        );
        let frame = frame_for(&frames, w1);
        assert!(frame.size.width >= 499.0);
        assert!(frame.size.height >= 349.0);
    }

    #[test]
    fn respects_positive_max_width_for_resizable_window() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let window = wid(30, 1);
        system.add_window_after_selection(layout, window);

        let mut constraints = HashMap::default();
        constraints.insert(
            window,
            WindowLayoutConstraints {
                is_resizable: true,
                locked_width: 0.0,
                locked_height: 0.0,
                min_width: 0.0,
                min_height: 0.0,
                max_width: 600.0,
                max_height: 0.0,
            }
            .normalized(),
        );

        let frames = system.calculate_layout(
            layout,
            screen(1200.0, 700.0),
            &constraints,
            &GapSettings::default(),
        );
        let frame = frame_for(&frames, window);
        assert!(frame.size.width <= 600.0);
    }

    #[test]
    fn locked_window_width_wins_over_smaller_sibling_max_width_in_same_column() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let locked = wid(31, 1);
        let capped = wid(31, 2);
        system.add_window_after_selection(layout, locked);
        system.add_window_after_selection(layout, capped);

        let state = system.layouts.get_mut(layout).expect("layout state missing");
        state.columns = vec![Column {
            windows: vec![locked, capped],
            width_offset: 0.0,
            width_overridden: false,
            height_weights: vec![1.0, 1.0],
        height_overridden: false,
        }];
        state.selected = Some(locked);

        let mut constraints = HashMap::default();
        constraints.insert(
            locked,
            WindowLayoutConstraints {
                is_resizable: false,
                locked_width: 700.0,
                locked_height: 400.0,
                min_width: 700.0,
                min_height: 0.0,
                max_width: 700.0,
                max_height: 0.0,
            }
            .normalized(),
        );
        constraints.insert(
            capped,
            WindowLayoutConstraints {
                is_resizable: true,
                locked_width: 0.0,
                locked_height: 0.0,
                min_width: 0.0,
                min_height: 0.0,
                max_width: 500.0,
                max_height: 0.0,
            }
            .normalized(),
        );

        let frames = system.calculate_layout(
            layout,
            screen(1200.0, 700.0),
            &constraints,
            &GapSettings::default(),
        );
        let locked_frame = frame_for(&frames, locked);
        let capped_frame = frame_for(&frames, capped);

        assert!(locked_frame.size.width >= 699.0);
        assert!(capped_frame.size.width <= 501.0);
    }

    #[test]
    fn impossible_min_width_does_not_expand_column_beyond_tiling_width() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let window = wid(40, 1);
        system.add_window_after_selection(layout, window);

        let mut constraints = HashMap::default();
        constraints.insert(
            window,
            WindowLayoutConstraints {
                is_resizable: true,
                locked_width: 0.0,
                locked_height: 0.0,
                min_width: 1600.0,
                min_height: 0.0,
                max_width: 0.0,
                max_height: 0.0,
            }
            .normalized(),
        );

        let screen = screen(1200.0, 700.0);
        let gaps = GapSettings::default();
        let tiling = compute_tiling_area(screen, &gaps);
        let frames = system.calculate_layout(
            layout,
            screen,
            &constraints,
            &gaps,
        );
        let frame = frame_for(&frames, window);
        assert!(frame.size.width <= tiling.size.width);
        assert!(frame.origin.x >= tiling.origin.x);
        assert!(frame.origin.x + frame.size.width <= tiling.origin.x + tiling.size.width);
    }

    #[test]
    fn creates_columns_and_moves_focus() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);
        let w3 = wid(1, 3);

        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);
        system.add_window_after_selection(layout, w3);

        assert_eq!(system.visible_windows_in_layout(layout).len(), 3);
        assert_eq!(system.selected_window(layout), Some(w3));

        let (focus, _) = system.move_focus(layout, Direction::Left);
        assert_eq!(focus, Some(w2));
    }

    #[test]
    fn move_selection_swaps_columns_horizontally() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);
        let w3 = wid(1, 3);

        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);
        system.add_window_after_selection(layout, w3);

        assert!(system.move_selection(layout, Direction::Left));

        let state = system.layouts.get(layout).expect("layout state missing");
        assert_eq!(state.columns.len(), 3);
        assert_eq!(state.columns[1].windows, vec![w3]);
        assert_eq!(state.columns[2].windows, vec![w2]);
    }

    #[test]
    fn calculates_centered_columns() {
        let (system, layout, _, _) = setup_two_windows(ScrollingLayoutSettings::default());
        let frames = render(&system, layout, screen(1000.0, 800.0), &GapSettings::default());

        assert_eq!(frames.len(), 2);
        let width0 = frames[0].1.size.width;
        let width1 = frames[1].1.size.width;
        assert!(
            width0 > 1.0 && width1 > 1.0 && (width0 - width1).abs() < 1.0,
            "expected equal non-zero widths, got w0={}, w1={}",
            width0,
            width1
        );
    }

    #[test]
    fn centers_selected_column_without_changing_alignment() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.alignment = crate::layout::settings::ScrollingAlignment::Left;
        let (mut system, layout, _, w2) = setup_two_windows(settings);
        system.center_selected_column(layout);

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let frames = render(&system, layout, screen, &gaps);

        let tiling = compute_tiling_area(screen, &gaps);
        let selected_frame = frame_for(&frames, w2);

        let column_width = selected_frame.size.width;
        let expected_x = tiling.origin.x + (tiling.size.width - column_width) / 2.0;

        assert!(
            (selected_frame.origin.x - expected_x.round()).abs() < 1.0,
            "expected centered x={}, got x={}",
            expected_x.round(),
            selected_frame.origin.x
        );
    }

    #[test]
    fn center_selection_clears_when_focus_moves() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.alignment = crate::layout::settings::ScrollingAlignment::Left;
        let (mut system, layout, _, _) = setup_two_windows(settings);
        system.center_selected_column(layout);
        let _ = system.move_focus(layout, Direction::Left);

        let state = system.layouts.get(layout).expect("layout state missing");
        assert_eq!(state.center_override_window, None);
    }

    #[test]
    fn center_selection_toggles_back_to_layout_alignment() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.alignment = crate::layout::settings::ScrollingAlignment::Left;
        let (mut system, layout, _, w2) = setup_two_windows(settings);

        // First call centers the current selection.
        system.center_selected_column(layout);
        // Second call on the same selection toggles centering off.
        system.center_selected_column(layout);

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let frames = render(&system, layout, screen, &gaps);
        let tiling = compute_tiling_area(screen, &gaps);
        let selected_frame = frame_for(&frames, w2);
        assert!(
            (selected_frame.origin.x - tiling.origin.x.round()).abs() < 1.0,
            "expected left-aligned x={}, got x={}",
            tiling.origin.x.round(),
            selected_frame.origin.x
        );
    }

    #[test]
    fn horizontal_focus_keeps_side_by_side_columns_visible_without_anchor_snapping() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.alignment = crate::layout::settings::ScrollingAlignment::Left;
        settings.focus_navigation_style =
            crate::layout::settings::ScrollingFocusNavigationStyle::Niri;
        settings.column_width_ratio = 0.45;
        settings.min_column_width_ratio = 0.2;
        settings.max_column_width_ratio = 0.9;
        let (mut system, layout, w1, w2) = setup_two_windows(settings);

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();

        // Apply the default initial alignment (selected = w2) so w1 starts off-screen.
        let _ = render(&system, layout, screen, &gaps);

        let _ = system.move_focus(layout, Direction::Left);
        let left_frames = render(&system, layout, screen, &gaps);
        let offset_after_left = scroll_offset(&system, layout);

        let _ = system.move_focus(layout, Direction::Right);
        let right_frames = render(&system, layout, screen, &gaps);
        let offset_after_right = scroll_offset(&system, layout);

        let w1_x_after_left = frame_for(&left_frames, w1).origin.x;
        let w2_x_after_right = frame_for(&right_frames, w2).origin.x;

        assert!(
            (offset_after_left - offset_after_right).abs() < 1.0,
            "expected no snap when toggling focus between visible columns, got offsets {} -> {}",
            offset_after_left,
            offset_after_right
        );
        assert!(
            w1_x_after_left >= -1.0 && w2_x_after_right >= -1.0,
            "expected side-by-side visibility, got x positions w1={}, w2={}",
            w1_x_after_left,
            w2_x_after_right
        );
    }

    #[test]
    fn horizontal_focus_anchored_snaps_to_alignment() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.alignment = crate::layout::settings::ScrollingAlignment::Left;
        settings.focus_navigation_style =
            crate::layout::settings::ScrollingFocusNavigationStyle::Anchored;
        settings.column_width_ratio = 0.45;
        settings.min_column_width_ratio = 0.2;
        settings.max_column_width_ratio = 0.9;
        let (mut system, layout, _, _) = setup_two_windows(settings);

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let _ = render(&system, layout, screen, &gaps);

        let _ = system.move_focus(layout, Direction::Left);
        let _ = render(&system, layout, screen, &gaps);
        let offset_after_left = scroll_offset(&system, layout);

        let _ = system.move_focus(layout, Direction::Right);
        let _ = render(&system, layout, screen, &gaps);
        let offset_after_right = scroll_offset(&system, layout);

        assert!(
            (offset_after_left - offset_after_right).abs() > 1.0,
            "expected anchored mode to snap offset on focus changes, got offsets {} -> {}",
            offset_after_left,
            offset_after_right
        );
    }

    #[test]
    fn resized_columns_remain_contiguous_without_horizontal_holes() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.alignment = crate::layout::settings::ScrollingAlignment::Left;
        settings.focus_navigation_style =
            crate::layout::settings::ScrollingFocusNavigationStyle::Anchored;
        let (mut system, layout, w1, w2) = setup_two_windows(settings);
        let _ = system.move_focus(layout, Direction::Left);
        system.resize_selection_by(layout, 0.12, ResizeOrientation::Horizontal);

        let gaps = GapSettings::default();
        let frames = render(&system, layout, screen(1000.0, 800.0), &gaps);

        let w1_frame = frame_for(&frames, w1);
        let w2_frame = frame_for(&frames, w2);

        let expected_w2_x = w1_frame.origin.x + w1_frame.size.width + gaps.inner.horizontal;
        assert!(
            (w2_frame.origin.x - expected_w2_x).abs() < 1.0,
            "expected contiguous columns, got w1 right+gap={} and w2 x={}",
            expected_w2_x,
            w2_frame.origin.x
        );
    }

    #[test]
    fn selecting_column_in_niri_mode_reveals_without_centering() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.alignment = crate::layout::settings::ScrollingAlignment::Center;
        settings.focus_navigation_style =
            crate::layout::settings::ScrollingFocusNavigationStyle::Niri;
        settings.column_width_ratio = 0.45;
        settings.min_column_width_ratio = 0.2;
        settings.max_column_width_ratio = 0.9;
        let (mut system, layout, w1, _) = setup_two_windows(settings);

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let _ = render(&system, layout, screen, &gaps);

        assert!(system.select_window(layout, w1));
        let frames = render(&system, layout, screen, &gaps);
        let w1_frame = frame_for(&frames, w1);
        let center_x = (screen.size.width - w1_frame.size.width) / 2.0;
        assert!(
            (w1_frame.origin.x - center_x).abs() > 5.0,
            "expected niri mode select to avoid centering, got centered x={} (center x={})",
            w1_frame.origin.x,
            center_x
        );
    }

    #[test]
    fn niri_focus_between_different_width_columns_keeps_strip_stable() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.alignment = crate::layout::settings::ScrollingAlignment::Center;
        settings.focus_navigation_style =
            crate::layout::settings::ScrollingFocusNavigationStyle::Niri;
        settings.column_width_ratio = 0.42;
        settings.min_column_width_ratio = 0.2;
        settings.max_column_width_ratio = 0.9;
        let (mut system, layout, w1, _) = setup_two_windows(settings);

        // Make focused-left column wider so selected widths differ across focus moves.
        let _ = system.move_focus(layout, Direction::Left);
        system.resize_selection_by(layout, 0.15, ResizeOrientation::Horizontal);

        let screen = screen(1200.0, 800.0);
        let gaps = GapSettings::default();

        let frames_left = render(&system, layout, screen, &gaps);
        let w1_x_left = frame_for(&frames_left, w1).origin.x;

        let _ = system.move_focus(layout, Direction::Right);
        let frames_right = render(&system, layout, screen, &gaps);
        let w1_x_right = frame_for(&frames_right, w1).origin.x;

        assert!(
            (w1_x_left - w1_x_right).abs() < 1.0,
            "expected stable strip position in niri mode, got x shift {} -> {}",
            w1_x_left,
            w1_x_right
        );
    }

    #[test]
    fn move_selection_right_extracts_selected_from_stacked_column() {
        let (mut system, layout, w1, w2) = setup_two_windows(ScrollingLayoutSettings::default());
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert!(system.move_selection(layout, Direction::Right));
        let state = system.layouts.get(layout).expect("layout state missing");
        assert_eq!(state.columns.len(), 2);
        assert_eq!(state.columns[0].windows, vec![w1]);
        assert_eq!(state.columns[1].windows, vec![w2]);
        assert_eq!(state.selected, Some(w2));
    }

    #[test]
    fn move_selection_left_extracts_selected_from_stacked_column_at_edge() {
        let (mut system, layout, w1, w2) = setup_two_windows(ScrollingLayoutSettings::default());
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert!(system.move_selection(layout, Direction::Left));
        let state = system.layouts.get(layout).expect("layout state missing");
        assert_eq!(state.columns.len(), 2);
        assert_eq!(state.columns[0].windows, vec![w2]);
        assert_eq!(state.columns[1].windows, vec![w1]);
        assert_eq!(state.selected, Some(w2));
    }

    #[test]
    fn niri_rightmost_resize_grow_increases_visible_width() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.alignment = crate::layout::settings::ScrollingAlignment::Center;
        settings.focus_navigation_style =
            crate::layout::settings::ScrollingFocusNavigationStyle::Niri;
        settings.column_width_ratio = 0.45;
        settings.min_column_width_ratio = 0.2;
        settings.max_column_width_ratio = 0.95;
        let (mut system, layout, _, w2) = setup_two_windows(settings); // selected rightmost

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();

        let before = render(&system, layout, screen, &gaps);
        let before_frame = frame_for(&before, w2);

        system.resize_selection_by(layout, 0.08, ResizeOrientation::Horizontal);

        let after = render(&system, layout, screen, &gaps);
        let after_frame = frame_for(&after, w2);

        let visible_width = |frame: CGRect| {
            let left = frame.origin.x.max(screen.origin.x);
            let right =
                (frame.origin.x + frame.size.width).min(screen.origin.x + screen.size.width);
            (right - left).max(0.0)
        };
        let before_visible = visible_width(before_frame);
        let after_visible = visible_width(after_frame);
        assert!(
            after_visible > before_visible + 1.0,
            "expected visible width to grow, before={} after={}",
            before_visible,
            after_visible
        );
    }

    #[test]
    fn center_override_persists_on_refocus_of_same_window_in_niri_mode() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.alignment = crate::layout::settings::ScrollingAlignment::Left;
        settings.focus_navigation_style =
            crate::layout::settings::ScrollingFocusNavigationStyle::Niri;
        let (mut system, layout, _, w2) = setup_two_windows(settings);

        system.center_selected_column(layout);

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let before = frame_for(&render(&system, layout, screen, &gaps), w2);

        assert!(system.select_window(layout, w2));
        assert!(system.select_window(layout, w2));

        let after = frame_for(&render(&system, layout, screen, &gaps), w2);

        assert!(
            (before.origin.x - after.origin.x).abs() < 1.0,
            "expected centered x to persist, got {} -> {}",
            before.origin.x,
            after.origin.x
        );
    }

    #[test]
    fn vertical_resize_adjusts_height_weights_and_calculates_correctly() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);
        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);

        // Join them to the same column
        system.toggle_fold_of_selection(layout, Direction::Left);

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();

        // 1. Initial layout calculation (should be split equally)
        let frames_before = render(&system, layout, screen, &gaps);
        let f1_before = frame_for(&frames_before, w1);
        let f2_before = frame_for(&frames_before, w2);
        assert!((f1_before.size.height - f2_before.size.height).abs() < 1.0);

        // 2. Select w1 and resize it
        assert!(system.select_window(layout, w1));
        let mut new_f1 = f1_before;
        new_f1.size.height = f1_before.size.height + 100.0;

        system.on_window_resized(layout, w1, f1_before, new_f1, screen, &gaps);

        // 3. Re-calculate layout and verify resized heights are preserved
        let frames_after = render(&system, layout, screen, &gaps);
        let f1_after = frame_for(&frames_after, w1);
        let f2_after = frame_for(&frames_after, w2);

        assert!((f1_after.size.height - (f1_before.size.height + 100.0)).abs() < 2.0);
        assert!((f2_after.size.height - (f2_before.size.height - 100.0)).abs() < 2.0);
        assert!(
            (f1_after.size.height + f2_after.size.height
                - (f1_before.size.height + f2_before.size.height))
                .abs()
                < 2.0
        );
    }

    #[test]
    fn vertical_resize_command_changes_row_height_without_changing_column_width() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);
        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert!(system.select_window(layout, w1));

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let before = frame_for(&render(&system, layout, screen, &gaps), w1);

        system.resize_selection_by(layout, 0.05, ResizeOrientation::Vertical);

        let after = frame_for(&render(&system, layout, screen, &gaps), w1);
        assert!(after.size.height > before.size.height);
        assert!((after.size.width - before.size.width).abs() < 1.0);
    }

    #[test]
    fn smart_resize_uses_row_height_for_a_stacked_column() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);
        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert!(system.select_window(layout, w1));

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let before = frame_for(&render(&system, layout, screen, &gaps), w1);
        system.resize_selection_by(layout, 0.05, ResizeOrientation::Smart);
        let after = frame_for(&render(&system, layout, screen, &gaps), w1);

        assert!(after.size.height > before.size.height);
        assert!((after.size.width - before.size.width).abs() < 1.0);
    }

    #[test]
    fn niri_new_window_reveal_does_not_unnecessarily_push_offscreen() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.alignment = crate::layout::settings::ScrollingAlignment::Center;
        settings.focus_navigation_style =
            crate::layout::settings::ScrollingFocusNavigationStyle::Niri;
        settings.column_width_ratio = 0.4;
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);

        // Add first window
        system.add_window_after_selection(layout, w1);
        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let _ = render(&system, layout, screen, &gaps);
        assert_eq!(scroll_offset(&system, layout), 0.0);

        // Add second window
        system.add_window_after_selection(layout, w2);
        let frames = render(&system, layout, screen, &gaps);
        let offset = scroll_offset(&system, layout);

        // Both windows fit on screen (0.4 * 1000 = 400 width each, total 800 width < 1000 screen width).
        // Since we are in Niri mode, adding a new window w2 to the right of w1
        // should reveal it, but since w2 already fits fully on screen at offset 0.0
        // (starts at 400.0, ends at 800.0), the scroll offset should remain 0.0,
        // keeping both windows on screen!
        assert_eq!(offset, 0.0);

        let w1_frame = frame_for(&frames, w1);
        let w2_frame = frame_for(&frames, w2);
        assert!(w1_frame.origin.x >= 0.0);
        assert!(w2_frame.origin.x + w2_frame.size.width <= 1000.0);
    }

    #[test]
    fn anchored_alignments_adjust_outer_columns() {
        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();

        // Test Left Alignment: last column is anchored to the right.
        {
            let mut settings = ScrollingLayoutSettings::default();
            settings.alignment = crate::layout::settings::ScrollingAlignment::Left;
            settings.focus_navigation_style =
                crate::layout::settings::ScrollingFocusNavigationStyle::Anchored;
            settings.column_width_ratio = 0.4;
            let mut system = ScrollingLayoutSystem::new(&settings);
            let layout = system.create_layout();
            let w1 = wid(1, 1);
            let w2 = wid(1, 2);
            let w3 = wid(1, 3);
            system.add_window_after_selection(layout, w1);
            system.add_window_after_selection(layout, w2);
            system.add_window_after_selection(layout, w3);

            assert!(system.select_window(layout, w1));
            let w1_frame = frame_for(&render(&system, layout, screen, &gaps), w1);
            assert!((w1_frame.origin.x - 0.0).abs() < 1.0); // left-aligned

            assert!(system.select_window(layout, w2));
            let w2_frame = frame_for(&render(&system, layout, screen, &gaps), w2);
            assert!((w2_frame.origin.x - 0.0).abs() < 1.0); // left-aligned

            assert!(system.select_window(layout, w3));
            let w3_frame = frame_for(&render(&system, layout, screen, &gaps), w3);
            assert!((w3_frame.origin.x - 600.0).abs() < 1.0); // right-aligned
        }

        // Test Right Alignment: first column is anchored to the left.
        {
            let mut settings = ScrollingLayoutSettings::default();
            settings.alignment = crate::layout::settings::ScrollingAlignment::Right;
            settings.focus_navigation_style =
                crate::layout::settings::ScrollingFocusNavigationStyle::Anchored;
            settings.column_width_ratio = 0.4;
            let mut system = ScrollingLayoutSystem::new(&settings);
            let layout = system.create_layout();
            let w1 = wid(1, 1);
            let w2 = wid(1, 2);
            let w3 = wid(1, 3);
            system.add_window_after_selection(layout, w1);
            system.add_window_after_selection(layout, w2);
            system.add_window_after_selection(layout, w3);

            assert!(system.select_window(layout, w1));
            let w1_frame = frame_for(&render(&system, layout, screen, &gaps), w1);
            assert!((w1_frame.origin.x - 0.0).abs() < 1.0); // left-aligned

            assert!(system.select_window(layout, w2));
            let w2_frame = frame_for(&render(&system, layout, screen, &gaps), w2);
            assert!((w2_frame.origin.x - 600.0).abs() < 1.0); // right-aligned

            assert!(system.select_window(layout, w3));
            let w3_frame = frame_for(&render(&system, layout, screen, &gaps), w3);
            assert!((w3_frame.origin.x - 600.0).abs() < 1.0); // right-aligned
        }

        // Test Center Alignment: first column is left-anchored, last is right-anchored, middle is centered.
        {
            let mut settings = ScrollingLayoutSettings::default();
            settings.alignment = crate::layout::settings::ScrollingAlignment::Center;
            settings.focus_navigation_style =
                crate::layout::settings::ScrollingFocusNavigationStyle::Anchored;
            settings.column_width_ratio = 0.4;
            let mut system = ScrollingLayoutSystem::new(&settings);
            let layout = system.create_layout();
            let w1 = wid(1, 1);
            let w2 = wid(1, 2);
            let w3 = wid(1, 3);
            system.add_window_after_selection(layout, w1);
            system.add_window_after_selection(layout, w2);
            system.add_window_after_selection(layout, w3);

            assert!(system.select_window(layout, w1));
            let w1_frame = frame_for(&render(&system, layout, screen, &gaps), w1);
            assert!((w1_frame.origin.x - 0.0).abs() < 1.0); // left-aligned

            assert!(system.select_window(layout, w2));
            let w2_frame = frame_for(&render(&system, layout, screen, &gaps), w2);
            assert!((w2_frame.origin.x - 300.0).abs() < 1.0); // centered

            assert!(system.select_window(layout, w3));
            let w3_frame = frame_for(&render(&system, layout, screen, &gaps), w3);
            assert!((w3_frame.origin.x - 600.0).abs() < 1.0); // right-aligned
        }
    }

    /// A column's width must not depend on how many OTHER columns share its workspace.
    ///
    /// This inverts the former `single_column_fills_full_width`, which asserted that a lone
    /// column expands to the whole viewport. That rule made a window's size a function of its
    /// neighbours, so a full-size window moved from a populated workspace to an empty one
    /// changed size with nothing having been done to it — reported as an Outlook window going
    /// half-size on workspace 2 and full again on 3.
    ///
    /// niri behaves as asserted here: one column keeps its preset width and leaves the rest
    /// of the strip empty. Full width is `maximize-column`
    /// (`toggle_fullscreen_within_gaps`), which is explicit and remembered per display.
    #[test]
    fn a_lone_column_keeps_its_configured_width() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.column_width_ratio = 0.4;
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w1 = wid(1, 1);

        system.add_window_after_selection(layout, w1);

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let frames1 = render(&system, layout, screen, &gaps);
        let w1_frame1 = frame_for(&frames1, w1);

        // Alone in the strip, and still the configured 0.4 * 1000.
        assert!(
            (w1_frame1.size.width - 400.0).abs() < 1.0,
            "a lone column must keep its configured width, got {w1_frame1:?}"
        );
        assert!((w1_frame1.origin.x - 0.0).abs() < 1.0);

        // Gaining a neighbour must not resize it. This is the property that failed before.
        let w2 = wid(1, 2);
        system.add_window_after_selection(layout, w2);

        let frames2 = render(&system, layout, screen, &gaps);
        let w1_frame2 = frame_for(&frames2, w1);
        let w2_frame2 = frame_for(&frames2, w2);

        assert!(
            (w1_frame2.size.width - w1_frame1.size.width).abs() < 1.0,
            "gaining a neighbour must not resize a column: {w1_frame1:?} -> {w1_frame2:?}"
        );
        assert!((w2_frame2.size.width - 400.0).abs() < 1.0);
    }

    /// Full width is still reachable — explicitly, rather than as a side effect of being
    /// alone in a workspace.
    #[test]
    fn full_width_mode_still_fills_the_viewport() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.column_width_ratio = 0.4;
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        system.add_window_after_selection(layout, w1);

        system.set_window_full_width(layout, w1, true);
        assert!(system.is_window_full_width(layout, w1));

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let frame = frame_for(&render(&system, layout, screen, &gaps), w1);
        assert!(
            (frame.size.width - 1000.0).abs() < 1.0,
            "full-width mode must fill the tiling width, got {frame:?}"
        );

        // And it round-trips back to the preset, so the mode is not a one-way door.
        system.set_window_full_width(layout, w1, false);
        let frame = frame_for(&render(&system, layout, screen, &gaps), w1);
        assert!((frame.size.width - 400.0).abs() < 1.0, "got {frame:?}");
    }

    #[test]
    fn app_reconciliation_honors_updated_next_to_selection_policy() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.base.window_insertion_point = Some(WindowInsertionPoint::EndOfTree);
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);
        let w3 = wid(1, 3);
        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);
        assert!(system.select_window(layout, w1));

        settings.base.window_insertion_point = Some(WindowInsertionPoint::NextToSelection);
        system.update_settings(&settings);
        system.set_windows_for_app(layout, 1, vec![w1, w2, w3]);

        assert_eq!(system.all_windows_in_layout(layout), vec![w1, w3, w2]);
    }

    #[test]
    fn app_reconciliation_honors_updated_end_of_tree_policy() {
        let mut settings = ScrollingLayoutSettings::default();
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);
        let w3 = wid(1, 3);
        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);
        assert!(system.select_window(layout, w1));

        settings.base.window_insertion_point = Some(WindowInsertionPoint::EndOfTree);
        system.update_settings(&settings);
        system.set_windows_for_app(layout, 1, vec![w1, w2, w3]);

        assert_eq!(system.all_windows_in_layout(layout), vec![w1, w2, w3]);
    }

    // ── full-width columns participate in the strip ─────────────────────────

    fn niri_settings(ratio: f64) -> ScrollingLayoutSettings {
        let mut settings = ScrollingLayoutSettings::default();
        settings.focus_navigation_style =
            crate::layout::settings::ScrollingFocusNavigationStyle::Niri;
        settings.column_width_ratio = ratio;
        settings.min_column_width_ratio = 0.2;
        settings.max_column_width_ratio = 1.0;
        settings
    }

    /// A full-width column must keep its strip-relative x, not be pinned to the
    /// viewport origin. Regression test for `frame = tiling`, which lifted the
    /// window out of the strip so it stayed put while everything scrolled past.
    #[test]
    fn fullscreen_within_gaps_column_scrolls_with_the_strip() {
        let (mut system, layout, w1, w2) = setup_two_windows(niri_settings(0.5));
        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();

        assert!(system.select_window(layout, w1));
        system.toggle_fullscreen_within_gaps_of_selection(layout);

        let x_before = frame_for(&render(&system, layout, screen, &gaps), w1).origin.x;

        // Scroll the strip by focusing the other column.
        assert!(system.select_window(layout, w2));
        let x_after = frame_for(&render(&system, layout, screen, &gaps), w1).origin.x;

        assert!(
            (x_before - x_after).abs() > 1.0,
            "full-width column stayed at x={} after the strip scrolled; it is not \
             participating in the strip",
            x_before
        );
    }

    /// The column holding a full-width window must reserve the whole viewport, or
    /// the next column is laid out on top of it.
    #[test]
    fn fullscreen_within_gaps_column_reserves_full_width() {
        let (mut system, layout, w1, w2) = setup_two_windows(niri_settings(0.5));
        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();

        assert!(system.select_window(layout, w1));
        system.toggle_fullscreen_within_gaps_of_selection(layout);

        let frames = render(&system, layout, screen, &gaps);
        let f1 = frame_for(&frames, w1);
        let f2 = frame_for(&frames, w2);

        let tiling_width = compute_tiling_area(screen, &gaps).size.width;
        assert!(
            (f1.size.width - tiling_width).abs() < 2.0,
            "expected full tiling width {}, got {}",
            tiling_width,
            f1.size.width
        );
        assert!(
            f2.origin.x >= f1.origin.x + f1.size.width - 1.0,
            "next column at x={} overlaps the full-width column ending at {}",
            f2.origin.x,
            f1.origin.x + f1.size.width
        );
    }

    /// Toggling must be symmetric: a second press returns the original width.
    #[test]
    fn fullscreen_within_gaps_toggles_back_to_preset_width() {
        let (mut system, layout, w1, _) = setup_two_windows(niri_settings(0.5));
        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();

        assert!(system.select_window(layout, w1));
        let width_before = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;

        system.toggle_fullscreen_within_gaps_of_selection(layout);
        let width_full = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;

        system.toggle_fullscreen_within_gaps_of_selection(layout);
        let width_after = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;

        assert!(
            width_full > width_before + 1.0,
            "toggle did not widen the column"
        );
        assert!(
            (width_after - width_before).abs() < 2.0,
            "expected {} after toggling back, got {}",
            width_before,
            width_after
        );
    }

    /// Toggling a column at the RIGHT EDGE of the viewport must rescroll so the
    /// widened column is visible. Without the reveal it grew off-screen and the
    /// command looked like it had done nothing at all.
    #[test]
    fn fullscreen_within_gaps_reveals_column_at_right_edge() {
        let settings = niri_settings(0.5);
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let (w1, w2, w3) = (wid(1, 1), wid(1, 2), wid(1, 3));
        for w in [w1, w2, w3] {
            system.add_window_after_selection(layout, w);
        }
        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();

        // Select the last column, which sits at the right edge of the viewport.
        assert!(system.select_window(layout, w3));
        let _ = render(&system, layout, screen, &gaps);

        system.toggle_fullscreen_within_gaps_of_selection(layout);
        let f3 = frame_for(&render(&system, layout, screen, &gaps), w3);

        let tiling = compute_tiling_area(screen, &gaps);
        assert!(
            f3.origin.x >= tiling.origin.x - 1.0
                && f3.origin.x + f3.size.width <= tiling.origin.x + tiling.size.width + 1.0,
            "widened column at x={} w={} is not fully inside the viewport {}..{}",
            f3.origin.x,
            f3.size.width,
            tiling.origin.x,
            tiling.origin.x + tiling.size.width
        );
    }

    // ── preset column widths ────────────────────────────────────────────────

    /// ctrl-R cycles 1/3 -> 1/2 -> 2/3 and wraps back to 1/3.
    #[test]
    fn cycle_preset_column_width_walks_presets_and_wraps() {
        let mut settings = niri_settings(0.33333);
        settings.preset_column_widths = vec![0.33333, 0.5, 0.66667];
        let (mut system, layout, w1, _) = setup_two_windows(settings);
        let screen = screen(1200.0, 800.0);
        let gaps = GapSettings::default();
        let tiling_width = compute_tiling_area(screen, &gaps).size.width;

        assert!(system.select_window(layout, w1));

        let width_of = |system: &ScrollingLayoutSystem| {
            frame_for(&render(system, layout, screen, &gaps), w1).size.width / tiling_width
        };

        // Starts at the first preset.
        assert!(
            (width_of(&system) - 0.33333).abs() < 0.02,
            "start {}",
            width_of(&system)
        );

        system.cycle_preset_column_width(layout);
        assert!(
            (width_of(&system) - 0.5).abs() < 0.02,
            "after 1st {}",
            width_of(&system)
        );

        system.cycle_preset_column_width(layout);
        assert!(
            (width_of(&system) - 0.66667).abs() < 0.02,
            "after 2nd {}",
            width_of(&system)
        );

        // Wraps rather than sticking at the widest.
        system.cycle_preset_column_width(layout);
        assert!(
            (width_of(&system) - 0.33333).abs() < 0.02,
            "after wrap {}",
            width_of(&system)
        );
    }

    /// Two columns at ratio 0.5 must fit, so moving focus between them does not scroll the strip.
    #[test]
    fn two_half_width_columns_do_not_shift_when_focus_alternates() {
        let (mut system, layout, w1, w2) = setup_two_windows(niri_settings(0.5));
        let screen = screen(1728.0, 1117.0);
        // With zero gaps two 0.5 columns fit exactly and the overflow cannot appear.
        let mut gaps = GapSettings::default();
        gaps.outer.left = 4.0;
        gaps.outer.right = 4.0;
        gaps.inner.horizontal = 4.0;

        let positions = |system: &ScrollingLayoutSystem| {
            let frames = render(system, layout, screen, &gaps);
            (
                frame_for(&frames, w1).origin.x.round(),
                frame_for(&frames, w2).origin.x.round(),
            )
        };

        assert!(system.select_window(layout, w1));
        // Render once so the strip settles before measuring.
        let _ = positions(&system);
        let first = positions(&system);

        // Alternate focus with move_focus, which is what ctrl-J / ctrl-L invoke and
        // what triggers reveal-on-demand. select_window does NOT take that path, so
        // it cannot reproduce the shift.
        for i in 0..3 {
            let _ = system.move_focus(layout, Direction::Right);
            assert_eq!(
                positions(&system),
                first,
                "strip shifted moving right (iter {i})"
            );
            let _ = system.move_focus(layout, Direction::Left);
            assert_eq!(positions(&system), first, "strip shifted moving left (iter {i})");
        }

        // And both must genuinely be on screen, not merely stable.
        let frames = render(&system, layout, screen, &gaps);
        let tiling = compute_tiling_area(screen, &gaps);
        let f2 = frame_for(&frames, w2);
        assert!(
            f2.origin.x + f2.size.width <= tiling.origin.x + tiling.size.width + 1.0,
            "second column runs past the viewport: ends at {}, viewport ends at {}",
            f2.origin.x + f2.size.width,
            tiling.origin.x + tiling.size.width
        );
    }

    /// An empty preset list must be a no-op rather than a panic or a zero width.
    #[test]
    fn cycle_preset_column_width_with_no_presets_is_a_noop() {
        let mut settings = niri_settings(0.5);
        settings.preset_column_widths = vec![];
        let (mut system, layout, w1, _) = setup_two_windows(settings);
        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();

        assert!(system.select_window(layout, w1));
        let before = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;

        let touched = system.cycle_preset_column_width(layout);
        let after = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;

        assert!(touched.is_empty(), "expected no windows reported changed");
        assert!(
            (before - after).abs() < 1.0,
            "width changed from {} to {}",
            before,
            after
        );
    }

    // ── stacking direction ──────────────────────────────────────────────────

    /// ctrl-, moves the SELECTED window into the PREVIOUS column.
    #[test]
    fn stacking_moves_selected_window_into_previous_column() {
        let settings = niri_settings(0.33333);
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let (w1, w2, w3) = (wid(1, 1), wid(1, 2), wid(1, 3));
        for w in [w1, w2, w3] {
            system.add_window_after_selection(layout, w);
        }

        // Select the middle column and stack it leftwards.
        assert!(system.select_window(layout, w2));
        let moved = system.apply_stacking_to_parent_of_selection(layout);

        assert_eq!(moved, vec![w2], "expected the SELECTED window to move");

        let state = system.layouts.get(layout).expect("layout state");
        assert!(
            state.columns[0].windows.contains(&w1) && state.columns[0].windows.contains(&w2),
            "w2 should now share the first column with w1, got {:?}",
            state.columns.iter().map(|c| c.windows.clone()).collect::<Vec<_>>()
        );
        assert!(
            state.columns.iter().any(|c| c.windows == vec![w3]),
            "w3 should be untouched in its own column"
        );
    }

    /// From the first column there is no previous column, so fall forward instead
    /// of doing nothing.
    #[test]
    fn stacking_from_first_column_falls_back_to_next() {
        let (mut system, layout, w1, w2) = setup_two_windows(niri_settings(0.5));

        assert!(system.select_window(layout, w1));
        let moved = system.apply_stacking_to_parent_of_selection(layout);

        assert_eq!(moved, vec![w1]);
        let state = system.layouts.get(layout).expect("layout state");
        assert_eq!(state.columns.len(), 1, "expected a single stacked column");
        assert!(
            state.columns[0].windows.contains(&w1) && state.columns[0].windows.contains(&w2),
            "both windows should share one column"
        );
    }

    /// Changing the default column ratio must not leave restored columns at their
    /// old absolute width.
    ///
    /// Widths are stored as an offset from column_width_ratio, so a restored layout
    /// reproduced its old width against the new base: moving 0.49 -> 0.5 kept every
    /// window at 842pt instead of 859pt, a width matching no preset.
    #[test]
    fn changing_default_ratio_rebases_column_widths() {
        let settings = niri_settings(0.49);
        let (mut system, layout, w1, _) = setup_two_windows(settings.clone());
        let screen = screen(1728.0, 1117.0);
        let mut gaps = GapSettings::default();
        gaps.outer.left = 4.0;
        gaps.outer.right = 4.0;
        gaps.inner.horizontal = 2.0;

        let width_of = |system: &ScrollingLayoutSystem| {
            frame_for(&render(system, layout, screen, &gaps), w1).size.width.round()
        };

        let before = width_of(&system);

        let mut wider = settings.clone();
        wider.column_width_ratio = 0.5;
        system.update_settings(&wider);

        let after = width_of(&system);
        assert!(
            after > before,
            "raising the default ratio must widen an un-overridden column: {before} -> {after}"
        );

        // And it must land on the new default rather than anywhere in between.
        let tiling = compute_tiling_area(screen, &gaps).size.width;
        let expected = (tiling * 0.5 - gaps.inner.horizontal / 2.0).round();
        assert!(
            (after - expected).abs() <= 1.0,
            "expected the new default width {expected}, got {after}"
        );
    }

    /// A column the user deliberately resized keeps its absolute width across a
    /// default-ratio change, rather than being reset along with the others.
    #[test]
    fn changing_default_ratio_preserves_explicit_column_widths() {
        let settings = niri_settings(0.49);
        let (mut system, layout, w1, _) = setup_two_windows(settings.clone());
        let screen = screen(1728.0, 1117.0);
        let gaps = GapSettings::default();

        assert!(system.select_window(layout, w1));
        system.resize_selection_by(layout, 0.10, ResizeOrientation::Horizontal);
        let chosen = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;

        let mut wider = settings.clone();
        wider.column_width_ratio = 0.5;
        system.update_settings(&wider);

        let after = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;
        assert!(
            (after - chosen).abs() <= 2.0,
            "an explicitly resized column must keep its width: {chosen} -> {after}"
        );
    }

    /// Stacking a window into another column must divide the height evenly, not let
    /// the newcomer take almost everything.
    ///
    /// move_window_to_column_end carried the window's weight over from the column it
    /// left, where as the sole window it held that column's entire share. Pushed into a
    /// column of 1.0-weighted windows it dominated, collapsing the existing window to
    /// its title bar.
    #[test]
    fn stacking_into_a_column_equalises_heights() {
        let settings = niri_settings(0.5);
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let (w1, w2) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);

        let screen = screen(1728.0, 1117.0);
        let gaps = GapSettings::default();

        // Stack w2 into w1's column.
        assert!(system.select_window(layout, w2));
        system.apply_stacking_to_parent_of_selection(layout);

        let frames = render(&system, layout, screen, &gaps);
        let h1 = frame_for(&frames, w1).size.height;
        let h2 = frame_for(&frames, w2).size.height;

        assert!(
            (h1 - h2).abs() <= 2.0,
            "stacked windows should share the height evenly, got {h1} and {h2}"
        );
        assert!(
            h1 > 100.0,
            "the pre-existing window must not collapse to a title bar, got height {h1}"
        );
    }
    /// The shape of the strip: one inner vec per column, windows top to bottom.
    fn shape(system: &ScrollingLayoutSystem, layout: LayoutId) -> Vec<Vec<WindowId>> {
        system
            .layout_state(layout)
            .expect("layout")
            .columns
            .iter()
            .map(|column| column.windows.clone())
            .collect()
    }

    fn stacked_three(settings: ScrollingLayoutSettings) -> (ScrollingLayoutSystem, LayoutId, [WindowId; 3]) {
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w = [wid(1, 1), wid(1, 2), wid(1, 3)];
        for id in w {
            system.add_window_after_selection(layout, id);
        }
        // Pull w2 and w3 into w1's column, so the strip is one column of three.
        assert!(system.select_window(layout, w[1]));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert!(system.select_window(layout, w[2]));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&system, layout), vec![vec![w[0], w[1], w[2]]], "fixture is one stack");
        (system, layout, w)
    }

    /// Maximizing a stacked window used to leave it in the column, where it covered the siblings it
    /// was sharing with: the tree said three windows abreast and the screen showed one.
    #[test]
    fn maximizing_a_stacked_window_pulls_it_out_of_the_stack() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[1]));
        system.toggle_fullscreen_within_gaps_of_selection(layout);

        assert_eq!(
            shape(&system, layout),
            vec![vec![w[0], w[2]], vec![w[1]]],
            "the maximized window gets its own column and the stack closes up"
        );
        assert!(system.is_window_full_width(layout, w[1]));
    }

    #[test]
    fn a_second_press_puts_it_back_between_the_windows_it_left() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[1]));
        system.toggle_fullscreen_within_gaps_of_selection(layout);
        system.toggle_fullscreen_within_gaps_of_selection(layout);

        assert_eq!(shape(&system, layout), vec![vec![w[0], w[1], w[2]]], "back in its old row");
        assert!(!system.is_window_full_width(layout, w[1]));
    }

    // Row 0 has no window above it, so the anchor is the one BELOW and it has to be restored in
    // front of it rather than after it.
    #[test]
    fn the_top_of_a_stack_goes_back_to_the_top() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[0]));
        system.toggle_fullscreen_within_gaps_of_selection(layout);
        assert_eq!(shape(&system, layout), vec![vec![w[1], w[2]], vec![w[0]]]);

        system.toggle_fullscreen_within_gaps_of_selection(layout);
        assert_eq!(shape(&system, layout), vec![vec![w[0], w[1], w[2]]], "back on top, not below");
    }

    /// The "if available" half. Every window it could go back beside has closed, so it stays the
    /// column it became rather than being put somewhere the user never had it.
    #[test]
    fn a_window_whose_stack_is_gone_stays_its_own_column() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[1]));
        system.toggle_fullscreen_within_gaps_of_selection(layout);
        system.remove_window(w[0]);
        system.remove_window(w[2]);

        system.toggle_fullscreen_within_gaps_of_selection(layout);
        assert_eq!(shape(&system, layout), vec![vec![w[1]]]);
        assert!(!system.is_window_full_width(layout, w[1]), "it still stops being maximized");
    }

    // The anchor closing must not leave an origin pointing at it: restoring into a window that is
    // gone is how a stale record turns into a window in the wrong place.
    #[test]
    fn closing_the_anchor_alone_leaves_the_rest_of_the_stack_reachable() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[1]));
        system.toggle_fullscreen_within_gaps_of_selection(layout);
        // w0 was the anchor, being directly above w1.
        system.remove_window(w[0]);

        system.toggle_fullscreen_within_gaps_of_selection(layout);
        assert_eq!(shape(&system, layout), vec![vec![w[2]], vec![w[1]]], "no stack to rejoin");
    }

    // A window alone in its column has nothing to be pulled out of, which is the common case and
    // must not gain a column on every press.
    #[test]
    fn maximizing_a_lone_window_does_not_reshape_the_strip() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let (a, b) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, a);
        system.add_window_after_selection(layout, b);

        assert!(system.select_window(layout, a));
        system.toggle_fullscreen_within_gaps_of_selection(layout);
        assert_eq!(shape(&system, layout), vec![vec![a], vec![b]]);
        system.toggle_fullscreen_within_gaps_of_selection(layout);
        assert_eq!(shape(&system, layout), vec![vec![a], vec![b]]);
    }

    /// The reservation and the frame have to agree. The strip reserved the CLAMPED width for the
    /// column while the window was handed the whole tiling width, so the next column was laid out
    /// on top of it.
    #[test]
    fn a_maximized_window_that_cannot_fill_the_viewport_does_not_overlap_the_next_column() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let (narrow, next) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, narrow);
        system.add_window_after_selection(layout, next);

        assert!(system.select_window(layout, narrow));
        system.toggle_fullscreen_within_gaps_of_selection(layout);

        let mut constraints = HashMap::default();
        constraints.insert(
            narrow,
            WindowLayoutConstraints { is_resizable: true, max_width: 400.0, ..Default::default() },
        );
        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let frames = system.calculate_layout(layout, screen, &constraints, &gaps);

        let maximized = frame_for(&frames, narrow);
        let neighbour = frame_for(&frames, next);
        assert_eq!(maximized.size.width, 400.0, "it cannot be wider than it says");
        assert!(
            maximized.origin.x + maximized.size.width <= neighbour.origin.x + 1.0,
            "maximized {maximized:?} runs into the next column at {neighbour:?}"
        );
    }
    /// Folding two windows together divides the height evenly, and it has to do that whether or not
    /// macOS happened to report a minimum height for either of them.
    ///
    /// The reported minimum used to be subtracted from the weight. Weights are 1.0 in a freshly
    /// folded column and minima are pixels, so a window with a minimum got 0.001 and a window
    /// without got 1.0: the first was left at its minimum, which on screen is its title bar. Two
    /// windows that both reported one, or neither, split evenly — which is why this came and went.
    #[test]
    fn folding_divides_the_height_evenly_even_when_one_window_reports_a_minimum() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let (top, bottom) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, top);
        system.add_window_after_selection(layout, bottom);
        assert!(system.select_window(layout, bottom));
        system.toggle_fold_of_selection(layout, Direction::Left);

        let mut constraints = HashMap::default();
        constraints.insert(
            top,
            WindowLayoutConstraints { is_resizable: true, min_height: 100.0, ..Default::default() },
        );
        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let frames = system.calculate_layout(layout, screen, &constraints, &gaps);

        let (a, b) = (frame_for(&frames, top), frame_for(&frames, bottom));
        assert!(
            (a.size.height - b.size.height).abs() < 2.0,
            "folded windows must share the height: top {a:?} bottom {b:?}"
        );
    }

    #[test]
    fn folding_divides_the_height_evenly_with_no_constraints_at_all() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let (top, bottom) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, top);
        system.add_window_after_selection(layout, bottom);
        assert!(system.select_window(layout, bottom));
        system.toggle_fold_of_selection(layout, Direction::Left);

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let frames = system.calculate_layout(layout, screen, &constraints_none(), &gaps);
        let (a, b) = (frame_for(&frames, top), frame_for(&frames, bottom));
        assert!((a.size.height - b.size.height).abs() < 2.0, "top {a:?} bottom {b:?}");
    }

    /// A deliberate vertical resize still wins, and still survives a re-render. Equalising
    /// unconditionally would have made the resize command do nothing.
    #[test]
    fn a_deliberate_vertical_resize_is_not_equalised_away() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let (top, bottom) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, top);
        system.add_window_after_selection(layout, bottom);
        assert!(system.select_window(layout, bottom));
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert!(system.select_window(layout, top));
        system.resize_selection_by(layout, 0.2, ResizeOrientation::Vertical);

        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let frames = system.calculate_layout(layout, screen, &constraints_none(), &gaps);
        let (a, b) = (frame_for(&frames, top), frame_for(&frames, bottom));
        assert!(a.size.height > b.size.height + 10.0, "resize lost: top {a:?} bottom {b:?}");
    }

    /// Folding a third window in re-equalises: the column was not deliberately split, so the new
    /// arrival should not be squeezed in beside two windows keeping their old shares.
    #[test]
    fn folding_another_window_in_re_equalises_the_column() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let w = [wid(1, 1), wid(1, 2), wid(1, 3)];
        for id in w {
            system.add_window_after_selection(layout, id);
        }
        assert!(system.select_window(layout, w[1]));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert!(system.select_window(layout, w[2]));
        system.toggle_fold_of_selection(layout, Direction::Left);

        let screen = screen(1000.0, 900.0);
        let gaps = GapSettings::default();
        let frames = system.calculate_layout(layout, screen, &constraints_none(), &gaps);
        let heights: Vec<f64> = w.iter().map(|id| frame_for(&frames, *id).size.height).collect();
        let spread = heights.iter().cloned().fold(f64::MIN, f64::max)
            - heights.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread < 2.0, "three folded windows must share the height, got {heights:?}");
    }
    /// A window whose minimum width is wider than the configured column gets the width it needs,
    /// and its neighbour starts after it. Without this the window was drawn at the default 50% and
    /// clipped, because macOS refuses the resize and keeps it at its own minimum.
    #[test]
    fn a_column_reserves_the_minimum_width_its_window_demands() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.column_width_ratio = 0.5;
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let (acme, neighbour) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, acme);
        system.add_window_after_selection(layout, neighbour);

        let mut constraints = HashMap::default();
        constraints.insert(
            acme,
            WindowLayoutConstraints { is_resizable: true, min_width: 600.0, ..Default::default() },
        );
        let screen = screen(1000.0, 800.0);
        let gaps = GapSettings::default();
        let frames = system.calculate_layout(layout, screen, &constraints, &gaps);

        let wide = frame_for(&frames, acme);
        let next = frame_for(&frames, neighbour);
        assert!(
            wide.size.width >= 600.0 - 1.0,
            "a 600pt minimum against a 500pt default column, got {wide:?}"
        );
        assert!(
            next.origin.x >= wide.origin.x + wide.size.width - 1.0,
            "the neighbour must start after it: {wide:?} then {next:?}"
        );
    }
    /// One key, both directions, and it comes back. `toggle_stack`'s fold-out half explodes the
    /// whole column, so pressing it twice on a column of three does not return you to a column of
    /// three; this does.
    #[test]
    fn folding_a_window_in_and_out_returns_it_to_the_same_row() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[1]));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(
            shape(&system, layout),
            vec![vec![w[0], w[2]], vec![w[1]]],
            "folded out into its own column"
        );

        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(
            shape(&system, layout),
            vec![vec![w[0], w[1], w[2]]],
            "and back between the two it left"
        );
    }

    #[test]
    fn folding_a_lone_window_in_puts_it_under_the_column_to_its_left() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let (left, right) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, left);
        system.add_window_after_selection(layout, right);

        assert!(system.select_window(layout, right));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&system, layout), vec![vec![left, right]]);

        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&system, layout), vec![vec![left], vec![right]], "and out again");
    }

    /// The first column has nothing to its left, and the left key does NOT quietly fold right
    /// instead: with a key bound per side, doing that would make the two keys agree at the ends of
    /// the strip, which is the surprise this replaced.
    #[test]
    fn folding_left_from_the_first_column_does_nothing() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let (first, second) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, first);
        system.add_window_after_selection(layout, second);

        assert!(system.select_window(layout, first));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(
            shape(&system, layout),
            vec![vec![first], vec![second]],
            "nothing on the left to fold into"
        );

        // The other key reaches the column that IS there.
        system.toggle_fold_of_selection(layout, Direction::Right);
        assert_eq!(shape(&system, layout), vec![vec![second, first]]);
    }

    #[test]
    fn folding_right_puts_the_window_under_the_column_to_its_right() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let (left, right) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, left);
        system.add_window_after_selection(layout, right);

        assert!(system.select_window(layout, left));
        system.toggle_fold_of_selection(layout, Direction::Right);
        assert_eq!(shape(&system, layout), vec![vec![right, left]]);

        // Out again, to the left of that column, which is where it started.
        system.toggle_fold_of_selection(layout, Direction::Right);
        assert_eq!(shape(&system, layout), vec![vec![left], vec![right]], "and back out");
    }

    /// The other key also unfolds, but sends the window out its own way: each key is the inverse of
    /// itself, not of the other one.
    #[test]
    fn the_other_key_unfolds_to_its_own_side() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let (left, right) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, left);
        system.add_window_after_selection(layout, right);

        assert!(system.select_window(layout, right));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&system, layout), vec![vec![left, right]]);

        system.toggle_fold_of_selection(layout, Direction::Right);
        assert_eq!(
            shape(&system, layout),
            vec![vec![right], vec![left]],
            "the right key sends it out to the left, so it lands the other side of its neighbour"
        );
    }

    /// Folding OUT lands away from the key's own side, which is what makes pressing one key twice
    /// a round trip: a window folded into the column on the left came FROM its right.
    #[test]
    fn folding_out_lands_away_from_the_side_the_key_names() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());
        assert!(system.select_window(layout, w[1]));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(
            shape(&system, layout),
            vec![vec![w[0], w[2]], vec![w[1]]],
            "the left key sends it out to the right"
        );

        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());
        assert!(system.select_window(layout, w[1]));
        system.toggle_fold_of_selection(layout, Direction::Right);
        assert_eq!(
            shape(&system, layout),
            vec![vec![w[1]], vec![w[0], w[2]]],
            "and the right key sends it out to the left"
        );
    }

    /// The property that matters: one key, pressed twice, leaves the strip as it was. Checked for
    /// both keys and from both starting states, since the two directions take different paths
    /// through the toggle.
    #[test]
    fn pressing_one_fold_key_twice_returns_the_strip_to_its_shape() {
        for side in [Direction::Left, Direction::Right] {
            // Starting folded in.
            let (mut system, layout, _w) = stacked_three(ScrollingLayoutSettings::default());
            let selected = system.selected_window(layout).expect("a selection");
            let before = shape(&system, layout);
            system.toggle_fold_of_selection(layout, side);
            system.toggle_fold_of_selection(layout, side);
            assert_eq!(shape(&system, layout), before, "{side:?} from a stack, window {selected:?}");

            // Starting as its own column, with a neighbour on each side to fold into.
            let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
            let layout = system.create_layout();
            let w = [wid(1, 1), wid(1, 2), wid(1, 3)];
            for id in w {
                system.add_window_after_selection(layout, id);
            }
            assert!(system.select_window(layout, w[1]));
            let before = shape(&system, layout);
            system.toggle_fold_of_selection(layout, side);
            assert_ne!(shape(&system, layout), before, "{side:?} should have folded it in");
            system.toggle_fold_of_selection(layout, side);
            assert_eq!(shape(&system, layout), before, "{side:?} from a lone column");
        }
    }

    /// Folding acts on the SELECTED window, never on whichever happens to be first. The reported
    /// confusion was the old `toggle_stack` exploding a column and moving focus, but a fold that
    /// fell back to the top of the strip would look identical, so it is pinned here.
    #[test]
    fn folding_out_takes_the_selected_window_not_the_top_of_the_stack() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[2]));
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(
            shape(&system, layout),
            vec![vec![w[0], w[1]], vec![w[2]]],
            "the bottom window is the one that left"
        );
        assert_eq!(system.selected_window(layout), Some(w[2]), "and it keeps the selection");
    }

    #[test]
    fn folding_with_nothing_selected_does_nothing() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());
        let before = shape(&system, layout);

        system.clear_selection_for_test(layout);
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(shape(&system, layout), before, "no selection is not a licence to move w0");
        let _ = w;
    }
}
