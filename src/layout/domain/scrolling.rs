use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU64, Ordering};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use serde::{Deserialize, Serialize};

use crate::layout::WindowLayoutConstraints;
use crate::layout::domain::area::compute_tiling_area;
use crate::layout::domain::constraints::{
    AxisConstraints, ColumnKind, clamp_to_constraints, column_limits, column_ratio, column_width,
    solve_axis_lengths,
};
use crate::layout::domain::preset_width::{PresetWidth, next_preset};
use crate::layout::domain::edge_drag::{Drag, End, Side, dragged_side, dragged_width, trade_height};
use crate::layout::domain::tile;
use crate::layout::domain::strip::{
    Reveal, anchor_x, centres, column_starts, offset_keeping_right_edge, reveal_offset,
};
use crate::layout::settings::{Gaps, ScrollingLayoutSettings, WindowInsertionPoint};
use crate::layout::{Direction, LayoutId, ResizeOrientation};
use rini_core::ids::{WindowId, pid_t};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

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

/// What the next layout holds the selected column to, by the value in `pending_pin`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pin {
    /// Its right edge at that x, in display coordinates: a left-edge drag keeps the right one.
    RightEdge,
    /// Its start that far right of the scroll offset: where a toggle found the column.
    Start,
}

/// How the strip showed a column when a toggle took its window, for the toggle back.
#[derive(Clone, Copy, Debug, PartialEq)]
enum View {
    /// Centred by a centre override.
    Centred,
    /// Its start this far right of the scroll offset.
    StartingAt(f64),
}

/// The toggle whose way back returns to a kept view.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Toggle {
    FullWidth,
    /// The fold key for this side.
    Fold(Direction),
}

/// The view `window`'s column had before `by` took it, kept until any other scroll.
#[derive(Clone, Copy, Debug, PartialEq)]
struct KeptView {
    window: WindowId,
    by: Toggle,
    view: View,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Column {
    windows: Vec<WindowId>,
    width_offset: f64,
    #[serde(default)]
    width_overridden: bool,
    /// The width was last given by a mouse drag, which `min-column-width` and `max-column-width`
    /// do not hold.
    #[serde(default)]
    width_dragged: bool,
    #[serde(default)]
    height_weights: Vec<f64>,
    /// Whether `height_weights` are DESIRED PIXEL HEIGHTS from a deliberate vertical resize, rather
    /// than the equal ratios a freshly folded column starts at. The two are read differently and
    /// conflating them is what collapsed a folded window to its title bar.
    #[serde(default)]
    height_overridden: bool,
}

impl Column {
    /// A width of its own, as an offset from the default ratio, and whether a drag gave it.
    fn give_width(&mut self, offset: f64, dragged: bool) {
        self.width_offset = offset;
        self.width_overridden = true;
        self.width_dragged = dragged;
    }

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

    /// Each row's height in points out of `available`, before the windows' own limits. A column
    /// nobody has resized divides evenly whatever its weights, as `calculate_layout` reads it.
    fn row_heights(&self, available: f64) -> Vec<f64> {
        let count = self.windows.len();
        let sum: f64 = self.height_weights.iter().sum();
        if !self.height_overridden || sum <= 0.0 {
            return vec![available / count as f64; count];
        }
        self.height_weights.iter().map(|weight| available * weight / sum).collect()
    }

    /// A dragged edge: `row` takes `height` from the row across `end` alone.
    fn trade_row_height(&mut self, row: usize, end: End, height: f64, available: f64) {
        let mut heights = self.row_heights(available);
        trade_height(&mut heights, row, end, height);
        self.height_weights = heights;
        self.height_overridden = true;
    }

    /// The app's own height: `row` takes it, and the other rows share what is left in their
    /// proportions.
    fn take_row_height(&mut self, row: usize, height: f64, available: f64) {
        // Weights as desired pixel heights: `solve_axis_lengths` reserves each window's minimum
        // first and shares the remainder by weight, so the split lands as asked, within the
        // windows' own limits.
        self.height_overridden = true;
        let height = height.max(1.0);
        let others_total = (available - height).max(1.0);
        let others_weight: f64 = self
            .height_weights
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != row)
            .map(|(_, &w)| w)
            .sum();
        let others = (self.windows.len() - 1).max(1) as f64;
        for (i, weight) in self.height_weights.iter_mut().enumerate() {
            *weight = if i == row {
                height
            } else if others_weight > 0.0 {
                others_total * (*weight / others_weight)
            } else {
                others_total / others
            };
        }
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
    /// The value `pending_pin_of`'s pin holds the selected column to, or NaN once a pass spent it.
    /// Applied only while the window it names is still the selection.
    #[serde(skip, default = "default_atomic_unset")]
    pending_pin: AtomicU64,
    #[serde(skip)]
    pending_pin_of: Option<(WindowId, Pin)>,
    #[serde(skip)]
    kept_view: Option<KeptView>,
    center_override_window: Option<WindowId>,
    #[serde(skip, default = "default_atomic")]
    last_screen_width: AtomicU64,
    #[serde(skip, default = "default_atomic")]
    last_gap_x: AtomicU64,
    /// Where the selected column's start sat against the scroll offset after the last layout, or NaN
    /// before the first. A change of gap re-spaces the strip around it, so the column stays put.
    #[serde(skip, default = "default_atomic_unset")]
    last_selected_rel_px: AtomicU64,
    /// The selection `last_selected_rel_px` was measured for, `packed`.
    #[serde(skip)]
    last_selected_rel_of: AtomicU64,
    /// Where the strip's first column started after the last layout, against the tiling area's left
    /// edge: the anchor less the scroll offset, or NaN before the first. See `strip_origin`.
    #[serde(skip, default = "default_atomic_unset")]
    last_strip_origin_px: AtomicU64,
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
            pending_pin: default_atomic_unset(),
            pending_pin_of: None,
            kept_view: None,
            center_override_window: None,
            last_screen_width: AtomicU64::new(0.0f64.to_bits()),
            last_gap_x: AtomicU64::new(0.0f64.to_bits()),
            last_selected_rel_px: default_atomic_unset(),
            last_selected_rel_of: AtomicU64::new(packed(None)),
            last_strip_origin_px: default_atomic_unset(),
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
            StackOrigin {
                anchor: self.columns[col_idx].windows[row_idx - 1],
                below: true,
            }
        } else {
            StackOrigin {
                anchor: self.columns[col_idx].windows[row_idx + 1],
                below: false,
            }
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
                width_dragged: false,
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
        let at = if origin.below {
            anchor_row + 1
        } else {
            anchor_row
        };
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

    /// The width `wid`'s column is actually at, as a ratio of the viewport.
    ///
    /// 1.0 while maximised, whatever the column's stored offset says. Every rule that asks "how wide
    /// is this now" has to read it this way: the stored offset is the width the column will go BACK
    /// to, and answering with it made `ctrl-R` and the resize keys compute their next width from a
    /// size the window had not been for some time.
    fn ratio_of(&self, wid: WindowId, col_idx: usize) -> f64 {
        if self.fullscreen_within_gaps.contains(&wid) {
            return 1.0;
        }
        self.column_width_ratio + self.columns[col_idx].width_offset
    }

    /// Whether this column holds a maximised window, and so occupies the whole viewport.
    fn is_full_width_column(&self, col: &Column) -> bool {
        col.windows.iter().any(|wid| self.fullscreen_within_gaps.contains(wid))
    }

    fn column_kind(&self, col: &Column) -> ColumnKind {
        if self.is_full_width_column(col) {
            ColumnKind::FullWidth
        } else if col.width_dragged {
            ColumnKind::Dragged
        } else {
            ColumnKind::Bounded
        }
    }

    /// Maximise `wid`, pulling it out of any stack it shares.
    ///
    /// A maximised window fills the tiling area, which would cover the siblings sharing its column
    /// while the tree still claimed they were abreast. It becomes its own column, and where it came
    /// from is kept so the toggle can put it back.
    fn enter_full_width(&mut self, wid: WindowId) {
        if let Some(origin) = self.split_out(wid, Direction::Right) {
            self.stack_origins.insert(wid, origin);
        }
        self.fullscreen_within_gaps.insert(wid);
    }

    /// Stop being maximised and go back into the stack `enter_full_width` pulled the window out of.
    ///
    /// A window that was alone in its column has no origin and simply stops being maximised. So does
    /// one whose whole former column has since closed.
    fn leave_full_width_restoring(&mut self, wid: WindowId) {
        self.fullscreen_within_gaps.remove(&wid);
        if let Some(origin) = self.stack_origins.remove(&wid) {
            self.restore_into_stack(wid, origin);
        }
    }

    /// Stop being maximised because the window was given a width of its own, and stay put.
    ///
    /// The window becomes an ordinary column with nowhere to return to, so the origin is FORGOTTEN
    /// rather than kept for later: the maximise that recorded it is over. Leaving it recorded meant a
    /// later `ctrl-F` — which records no new origin, the window being alone in its column by then —
    /// toggled off into a stack the user had not touched since.
    fn leave_full_width_in_place(&mut self, wid: WindowId) {
        self.fullscreen_within_gaps.remove(&wid);
        self.stack_origins.remove(&wid);
        self.unpin();
    }

    /// Give `wid`'s column an explicit ratio, by a drag or not, leaving full width if it was
    /// maximised.
    fn set_column_ratio(&mut self, wid: WindowId, col_idx: usize, ratio: f64, dragged: bool) {
        self.leave_full_width_in_place(wid);
        self.columns[col_idx].give_width(ratio - self.column_width_ratio, dragged);
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
        self.unpin();
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
        self.unpin();
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
        self.unpin();
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
        self.unpin();
        self.center_override_window = None;
        self.pending_center_align.store(false, Ordering::Relaxed);
        self.pending_align.store(false, Ordering::Relaxed);
        // 2 = neutral reveal: keep current offset unless selected would be clipped.
        self.pending_reveal_direction.store(2, Ordering::Relaxed);
    }

    fn pin(&mut self, wid: WindowId, pin: Pin, value: f64) {
        self.pending_pin.store(value.to_bits(), Ordering::Relaxed);
        self.pending_pin_of = Some((wid, pin));
    }

    /// Any scroll replaces what is held for a later pass: the pin, and the view a restore takes.
    fn unpin(&mut self) {
        self.pending_pin.store(f64::NAN.to_bits(), Ordering::Relaxed);
        self.pending_pin_of = None;
        self.kept_view = None;
    }

    /// Whether a scroll request for `wid` would drop something held for it: its centring, a pin, or
    /// the view a toggle back returns to.
    fn holds_scroll_for(&self, wid: WindowId) -> bool {
        self.center_override_window == Some(wid)
            || self.pending_pin_of.is_some_and(|(of, _)| of == wid)
            || self.kept_view.is_some_and(|kept| kept.window == wid)
    }

    /// How the last pass showed `wid`'s column, if `wid` was its selection.
    fn laid_out_view_of(&self, wid: WindowId) -> Option<View> {
        if unpacked(self.last_selected_rel_of.load(Ordering::Relaxed)) != Some(wid) {
            return None;
        }
        if self.center_override_window == Some(wid) {
            return Some(View::Centred);
        }
        let rel = f64::from_bits(self.last_selected_rel_px.load(Ordering::Relaxed));
        rel.is_finite().then_some(View::StartingAt(rel))
    }

    /// The view `by`'s way back returns `wid` to, if it kept one. Any other kept view goes too.
    fn take_kept_view(&mut self, wid: WindowId, by: Toggle) -> Option<View> {
        let kept = self.kept_view.take()?;
        (kept.window == wid && kept.by == by).then_some(kept.view)
    }

    /// Rescroll so a toggled column stays visible, as the resize path does, unless it is going back
    /// to a view nothing has scrolled away from since.
    fn rescroll_after_toggle(&mut self, wid: WindowId, back_to: Option<View>, reveals_only: bool) {
        match back_to {
            Some(view) => self.return_to_view(wid, view),
            None if reveals_only => self.reveal_selected_without_direction(),
            None => self.align_scroll_to_selected(),
        }
    }

    /// Show the selected `wid`'s column the way `view` says the strip showed it.
    fn return_to_view(&mut self, wid: WindowId, view: View) {
        self.unpin();
        self.pending_align.store(false, Ordering::Relaxed);
        self.pending_reveal_direction.store(0, Ordering::Relaxed);
        match view {
            View::Centred => {
                self.center_override_window = Some(wid);
                self.pending_center_align.store(true, Ordering::Relaxed);
            }
            View::StartingAt(rel) => {
                self.center_override_window = None;
                self.pending_center_align.store(false, Ordering::Relaxed);
                self.pin(wid, Pin::Start, rel);
            }
        }
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

        self.unpin();
        self.clamp_scroll_offset();
        self.selected
    }

    fn insert_column_after(&mut self, index: usize, wid: WindowId) {
        let column = Column {
            windows: vec![wid],
            width_offset: 0.0,
            width_overridden: false,
            width_dragged: false,
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
            width_dragged: false,
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
                    width_dragged: false,
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
            pending_pin: AtomicU64::new(self.pending_pin.load(Ordering::Relaxed)),
            pending_pin_of: self.pending_pin_of,
            kept_view: self.kept_view,
            center_override_window: self.center_override_window,
            last_screen_width: AtomicU64::new(self.last_screen_width.load(Ordering::Relaxed)),
            last_gap_x: AtomicU64::new(self.last_gap_x.load(Ordering::Relaxed)),
            last_selected_rel_px: AtomicU64::new(self.last_selected_rel_px.load(Ordering::Relaxed)),
            last_selected_rel_of: AtomicU64::new(self.last_selected_rel_of.load(Ordering::Relaxed)),
            last_strip_origin_px: AtomicU64::new(self.last_strip_origin_px.load(Ordering::Relaxed)),
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

fn default_atomic_unset() -> AtomicU64 {
    AtomicU64::new(f64::NAN.to_bits())
}

/// A window, or none, as one atomic word: `idx` is never zero, so zero is none.
fn packed(wid: Option<WindowId>) -> u64 {
    wid.map_or(0, |wid| (u64::from(wid.pid as u32) << 32) | u64::from(wid.idx.get()))
}

fn unpacked(bits: u64) -> Option<WindowId> {
    let idx = std::num::NonZeroU32::new(bits as u32)?;
    Some(WindowId { pid: (bits >> 32) as u32 as pid_t, idx })
}

/// The scroll offset that keeps the selected column where it was on screen after the gap between
/// columns changed. The offset is in strip points and every column start includes the gaps before
/// it, so a wider gap slides the selected column right by one gap change per column ahead of it
/// unless the offset follows. `None` when the gap is unchanged or nothing was laid out before.
fn rebased_offset(previous_gap: f64, gap: f64, previous_rel: f64, selected_start: f64) -> Option<f64> {
    (gap != previous_gap && previous_rel.is_finite()).then(|| selected_start - previous_rel)
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

    /// The configured default width, held inside its own bounds.
    ///
    /// The same clamp a column gets, with no offset to add: `column_ratio` is the only place the
    /// bounds are applied, so the default and the columns derived from it cannot drift apart.
    fn clamp_base_ratio(ratio: f64, min_ratio: f64, max_ratio: f64) -> f64 {
        column_ratio(ColumnKind::Bounded, ratio, 0.0, min_ratio, max_ratio)
    }

    fn column_widths_and_starts(
        state: &LayoutState,
        screen_width: f64,
        gap_x: f64,
        min_ratio: f64,
        max_ratio: f64,
    ) -> (Vec<f64>, Vec<f64>) {
        let base_ratio = Self::clamp_base_ratio(state.column_width_ratio, min_ratio, max_ratio);
        let mut widths = Vec::with_capacity(state.columns.len());
        let mut starts = Vec::with_capacity(state.columns.len());
        let mut cursor = 0.0;
        for col in &state.columns {
            starts.push(cursor);
            let ratio = column_ratio(
                state.column_kind(col),
                base_ratio,
                col.width_offset,
                min_ratio,
                max_ratio,
            );
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
        state.unpin();
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
        state.unpin();
        state.scroll_offset_px.store(next.to_bits(), Ordering::Relaxed);
    }

    pub fn center_selected_column(&mut self, layout: LayoutId) {
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        state.request_center_on_selected();
    }

    /// Where the strip's first column starts on screen after the last layout, in points from the
    /// tiling area's left edge: the anchor less the scroll offset. `None` before the first layout.
    ///
    /// A column at strip position p is drawn at this plus p. The animation path uses the CHANGE in this
    /// number as the distance the strip travelled, because it is the only description of the movement
    /// that does not depend on where each window really is: macOS clamps windows it will not place off
    /// screen, and the layout is recomputed several times per keystroke. The scroll offset alone missed
    /// a change of anchor, a centred column losing its centring, which moves every column with the
    /// offset unchanged. Against the tiling area, so a display moving in global space is not a pan.
    pub fn strip_origin(&self, layout: LayoutId) -> Option<f64> {
        self.layout_state(layout)
            .map(|state| f64::from_bits(state.last_strip_origin_px.load(Ordering::Relaxed)))
            .filter(|origin| !origin.is_nan())
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
                    width_dragged: false,
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

    /// Every tiled window's frame: its slot in the strip, inset by the border each window wears
    /// (`borders`, `tile::window_in`) and held to the window's own limits. Every width, gap and
    /// limit the strip is laid out from is a slot's.
    pub fn calculate_layout(
        &self,
        layout: LayoutId,
        screen: CGRect,
        constraints: &HashMap<WindowId, WindowLayoutConstraints>,
        borders: &tile::Borders,
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let Some(state) = self.layouts.get(layout) else {
            return Vec::new();
        };
        let tiling = compute_tiling_area(screen, gaps);
        let gap_x = gaps.inner;
        let gap_y = gaps.inner;
        let base_ratio = Self::clamp_base_ratio(
            state.column_width_ratio,
            self.settings.min_column_width_ratio,
            self.settings.max_column_width_ratio,
        );

        let mut column_widths = Vec::with_capacity(state.columns.len());
        let mut column_ratios = Vec::with_capacity(state.columns.len());
        for col in state.columns.iter() {
            // A maximised column must be width 1.0 here as well as in the frame assignment below:
            // column_widths feeds column_starts, which is what reserves horizontal space in the
            // strip, and reserving the ordinary width laid the next column out on top of it.
            //
            // A lone column keeps its width; expanding it tied a window's size to its neighbours.
            // See "Column width" in `src/layout/docs/strip.md`.
            let kind = state.column_kind(col);
            let ratio = column_ratio(
                kind,
                base_ratio,
                col.width_offset,
                self.settings.min_column_width_ratio,
                self.settings.max_column_width_ratio,
            );
            let limits = column_limits(
                col.windows.iter().filter_map(|wid| Some((constraints.get(wid).copied()?, borders.of(*wid)))),
            );
            let width = column_width(kind, ratio, tiling.size.width, gap_x, limits);
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
        let previous_gap = f64::from_bits(state.last_gap_x.load(Ordering::Relaxed));
        let previous_rel = f64::from_bits(state.last_selected_rel_px.load(Ordering::Relaxed));
        if let Some(offset) = rebased_offset(
            previous_gap,
            gap_x,
            previous_rel,
            column_starts.get(selected_col_idx).copied().unwrap_or(0.0),
        ) {
            state.scroll_offset_px.store(offset.to_bits(), Ordering::Relaxed);
        }
        state.last_screen_width.store(tiling.size.width.to_bits(), Ordering::Relaxed);
        state.last_gap_x.store(gap_x.to_bits(), Ordering::Relaxed);
        state.last_step_px.store(step.to_bits(), Ordering::Relaxed);

        let anchor_x = anchor_x(
            tiling,
            selected_width,
            selected_col_idx,
            state.columns.len(),
            self.settings.center_focused_column,
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
        let pinned = f64::from_bits(state.pending_pin.swap(f64::NAN.to_bits(), Ordering::Relaxed));
        let mut keeps_right_edge = false;
        if !pinned.is_nan()
            && let Some((wid, pin)) = state.pending_pin_of
            && state.selected == Some(wid)
            && let Some((col_idx, _)) = state.selected_location()
        {
            let start = column_starts.get(col_idx).copied().unwrap_or(0.0);
            let offset = match pin {
                Pin::RightEdge => {
                    keeps_right_edge = true;
                    let width = column_widths.get(col_idx).copied().unwrap_or(0.0);
                    offset_keeping_right_edge(anchor_x, start, width, pinned)
                }
                Pin::Start => start - pinned,
            };
            state.scroll_offset_px.store(offset.to_bits(), Ordering::Relaxed);
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
                    _ if keeps_right_edge => Some(Reveal::KeepingRightEdge),
                    -1 => Some(Reveal::FromRight),
                    1 => Some(Reveal::FromLeft),
                    2 => Some(Reveal::Either),
                    _ => None,
                };
                if let Some(reveal) = reveal
                    && let Some(corrected) = reveal_offset(
                        reveal,
                        tiling,
                        anchor_x,
                        selected_start,
                        selected_width,
                        offset,
                    )
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
        let selected_rel = column_starts.get(selected_col_idx).copied().unwrap_or(0.0) - clamped;
        state.last_selected_rel_px.store(selected_rel.to_bits(), Ordering::Relaxed);
        let measured_for = state.selected_location().and(state.selected);
        state.last_selected_rel_of.store(packed(measured_for), Ordering::Relaxed);
        let origin = anchor_x - tiling.origin.x - clamped;
        state.last_strip_origin_px.store(origin.to_bits(), Ordering::Relaxed);

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
                frame = tile::window_in(frame, borders.of(*wid));
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
        let reveals_only = self.settings.reveals_only();
        let Some(state) = self.layout_state_mut(layout) else {
            return (None, vec![]);
        };
        let new_sel = match direction {
            Direction::Left | Direction::Right => Self::move_focus_horizontal(state, direction),
            Direction::Up | Direction::Down => Self::move_focus_vertical(state, direction),
        };
        if new_sel.is_some() && reveals_only {
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
        let reveals_only = self.settings.reveals_only();
        let insertion_point = self.settings.window_insertion_point;
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        Self::insert_new_column(state, wid, insertion_point);
        if reveals_only {
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
            if let Some(kept) = &mut state.kept_view
                && kept.window == from
            {
                kept.window = to;
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

    /// The width a window's column carries, if it has been sized away from the default, and
    /// whether a mouse drag gave it, which the bounds do not hold.
    ///
    /// Needed so moving a window between workspaces or displays keeps the width the user gave
    /// it; a fresh column otherwise starts at the default ratio and the window appeared to
    /// reset to 50%.
    pub fn column_width_offset(&self, layout: LayoutId, wid: WindowId) -> Option<(f64, bool)> {
        let state = self.layout_state(layout)?;
        let (col_idx, _) = state.locate(wid)?;
        let column = state.columns.get(col_idx)?;
        // Only a deliberately sized column is worth carrying. An untouched one should adopt
        // the destination's default, which may differ (a wider display, a different config).
        column.width_overridden.then_some((column.width_offset, column.width_dragged))
    }

    /// Re-apply a width carried from another column.
    pub fn set_column_width_offset(
        &mut self,
        layout: LayoutId,
        wid: WindowId,
        offset: f64,
        dragged: bool,
    ) {
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        let Some((col_idx, _)) = state.locate(wid) else {
            return;
        };
        if let Some(column) = state.columns.get_mut(col_idx) {
            column.give_width(offset, dragged);
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

    /// Put a window back to the full viewport width it was remembered at.
    ///
    /// The same transition the key makes, so a window that comes back into a column it shares is
    /// pulled out of it rather than left maximised on top of its siblings. There is no setter for the
    /// other direction: nothing restores a window to "not maximised", because a window being placed
    /// is not maximised to begin with.
    pub fn restore_window_full_width(&mut self, layout: LayoutId, wid: WindowId) {
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        if state.locate(wid).is_none() {
            return;
        }
        state.enter_full_width(wid);
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
        let reveals_only = self.settings.reveals_only();
        let insertion_point = self.settings.window_insertion_point;
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
        if reveals_only {
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
        let reveals_only = self.settings.reveals_only();
        let Some(state) = self.layout_state_mut(layout) else {
            return false;
        };
        if state.locate(wid).is_some() {
            // Refocusing the selected window keeps what is held for it: its centring, the right
            // edge a left-edge drag keeps, which the release refocuses before its pass, and the
            // view its full width returns to, which the raise refocuses.
            if state.selected == Some(wid) && state.holds_scroll_for(wid) {
                return true;
            }
            state.selected = Some(wid);
            if reveals_only {
                state.reveal_selected_without_direction();
            } else {
                state.align_scroll_to_selected();
            }
            true
        } else {
            false
        }
    }

    /// The window's own frame, read back as its slot under its `border` (`tile::slot_of`) and taken
    /// as its column's width and its row's height. `drag` is the mouse drag that resized it, `None`
    /// for any other resize: the app's own, or a window rule's size. See "Mouse resize" in
    /// `src/layout/docs/strip.md`.
    pub fn on_window_resized(
        &mut self,
        layout: LayoutId,
        wid: WindowId,
        drag: Option<Drag>,
        new_frame: CGRect,
        border: f64,
        screen: CGRect,
        gaps: &Gaps,
    ) {
        let drag = drag.map(|drag| Drag { from: tile::slot_of(drag.from, border), ..drag });
        let new_frame = tile::slot_of(new_frame, border);
        let min_ratio = self.settings.min_column_width_ratio;
        let max_ratio = self.settings.max_column_width_ratio;
        let reveals_only = self.settings.reveals_only();
        let center = self.settings.center_focused_column;

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
        let base_ratio = state.column_width_ratio;
        let Some((col_idx, row_idx)) = state.locate(wid) else {
            return;
        };
        let dragged_from = drag.map(|drag| drag.from);
        let side_drag = dragged_from
            .and_then(|start| dragged_side(start, new_frame).map(|side| (start, side)));
        let full_width = state.is_full_width_column(&state.columns[col_idx]);
        if full_width && side_drag.is_none() {
            // A full-width column keeps the width it had before, for the toggle back: neither the
            // app settling its own size nor a drag that moves neither side gives it a width.
            return;
        }
        if reveals_only {
            state.reveal_selected_without_direction();
        } else {
            state.align_scroll_to_selected();
        }
        let width = dragged_from.map_or(new_frame.size.width, |start| {
            let has_override = state.center_override_window.is_some();
            let centred = centres(col_idx, state.columns.len(), center, has_override);
            dragged_width(start.size.width, new_frame.size.width, centred)
        });
        let kind = if side_drag.is_some() || state.columns[col_idx].width_dragged {
            ColumnKind::Dragged
        } else {
            ColumnKind::Bounded
        };
        let ratio = column_ratio(kind, width / tiling.size.width, 0.0, min_ratio, max_ratio);
        if let Some((start, _)) = side_drag
            && ratio >= 1.0
            && state.columns[col_idx].windows.len() == 1
        {
            if !full_width {
                // The width before the drag, for the toggle back.
                let offset = start.size.width / tiling.size.width - base_ratio;
                state.columns[col_idx].give_width(offset, true);
                state.enter_full_width(wid);
            }
        } else {
            state.set_column_ratio(wid, col_idx, ratio, kind == ColumnKind::Dragged);
        }

        let col = &mut state.columns[col_idx];
        if col.windows.len() > 1 && tiling.size.height > 0.0 {
            col.ensure_height_weights();
            let total_gap = gaps.inner * (col.windows.len().saturating_sub(1) as f64);
            let available_height = (tiling.size.height - total_gap).max(0.0);
            if available_height > 0.0 {
                let height = new_frame.size.height;
                match drag {
                    Some(Drag { end: Some(end), .. }) => {
                        col.trade_row_height(row_idx, end, height, available_height);
                    }
                    Some(Drag { end: None, .. }) => {}
                    None => col.take_row_height(row_idx, height, available_height),
                }
            }
        }

        if let Some((start, Side::Left)) = side_drag
            && reveals_only
        {
            state.pin(wid, Pin::RightEdge, start.origin.x + start.size.width);
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
        let reveals_only = self.settings.reveals_only();
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
            if reveals_only {
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
    /// of (`StackOrigin`) over appending to the end, so the rows keep their order too. Under
    /// `"never"` the fold back by the same key also puts the strip back, unless any other scroll
    /// came in between.
    pub fn toggle_fold_of_selection(&mut self, layout: LayoutId, side: Direction) -> Vec<WindowId> {
        let away = match side {
            Direction::Left => Direction::Right,
            Direction::Right => Direction::Left,
            _ => return Vec::new(),
        };
        let reveals_only = self.settings.reveals_only();
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
        let folding_out = state.columns[col_idx].windows.len() > 1;
        let view_before =
            (folding_out && reveals_only).then(|| state.laid_out_view_of(selected)).flatten();

        if folding_out {
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

        let by = Toggle::Fold(side);
        let back_to = state.take_kept_view(selected, by).filter(|_| !folding_out);
        state.selected = Some(selected);
        state.rescroll_after_toggle(selected, back_to, reveals_only);
        if let Some(view) = view_before {
            state.kept_view = Some(KeptView { window: selected, by, view });
        }
        vec![selected]
    }

    /// Maximise `wid`, or put it back to the width and stack it had before, and select it.
    ///
    /// The one toggle: `ctrl-F` asks it of the selection, a title-bar zoom of the window zoomed.
    /// Under `"never"` the way back also puts the strip back, its column at the screen x it left or
    /// centred again, unless any other scroll came in between. `false` when the window is not a
    /// column of this layout.
    pub fn toggle_full_width(&mut self, layout: LayoutId, wid: WindowId) -> bool {
        let reveals_only = self.settings.reveals_only();
        let Some(state) = self.layout_state_mut(layout) else {
            return false;
        };
        if state.locate(wid).is_none() {
            return false;
        }

        let entering = !state.fullscreen_within_gaps.contains(&wid);
        // Read before this toggle's own scroll request, which drops the centre override.
        let view_before = (entering && reveals_only).then(|| state.laid_out_view_of(wid)).flatten();
        let back_to = state.take_kept_view(wid, Toggle::FullWidth).filter(|_| !entering);
        if entering {
            state.enter_full_width(wid);
        } else {
            state.leave_full_width_restoring(wid);
        }
        state.selected = Some(wid);
        state.rescroll_after_toggle(wid, back_to, reveals_only);
        if let Some(view) = view_before {
            state.kept_view = Some(KeptView { window: wid, by: Toggle::FullWidth, view });
        }
        true
    }

    /// Cycle the selected column through `preset_column_widths`.
    ///
    /// niri's switch-preset-column-width. Unlike ResizeWindowGrow/Shrink, which
    /// step by a fixed ~5% and leave columns at arbitrary in-between widths, this
    /// snaps to a known set — so every column ends up at one of a few predictable
    /// sizes instead of drifting.
    ///
    /// Full width is one of those widths rather than a mode outside the cycle: a maximised column
    /// takes its turn like the rest, and a step off it is what UNMAXIMISES the window. The rule is
    /// [`next_preset`]; what a width means for a maximised column is `LayoutState::set_column_ratio`.
    pub fn cycle_preset_column_width(&mut self, layout: LayoutId) -> Vec<WindowId> {
        let presets = self.settings.preset_column_widths.clone();
        let reveals_only = self.settings.reveals_only();

        let Some(state) = self.layout_state_mut(layout) else {
            return Vec::new();
        };
        let Some(selected) = state.selected_or_first() else {
            return Vec::new();
        };
        let Some((col_idx, _)) = state.locate(selected) else {
            return Vec::new();
        };

        match next_preset(state.ratio_of(selected, col_idx), &presets) {
            Some(PresetWidth::Full) => state.enter_full_width(selected),
            Some(PresetWidth::Ratio(ratio)) => state.set_column_ratio(selected, col_idx, ratio, false),
            None => return Vec::new(),
        }
        state.selected = Some(selected);

        // A width change moves every column start after it, so the strip has to be
        // rescrolled or a column at the viewport edge grows off-screen. Same
        // reasoning as the fullscreen toggle above.
        if reveals_only {
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
        let reveals_only = self.settings.reveals_only();
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        let base_ratio = state.column_width_ratio;

        let Some((col_idx, row_idx)) = state.selected_location() else {
            if orientation == ResizeOrientation::Vertical {
                return;
            }
            state.column_width_ratio =
                column_ratio(ColumnKind::Bounded, base_ratio, amount, min_ratio, max_ratio);
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

        let selected = state.columns[col_idx].windows[row_idx];
        let current = state.ratio_of(selected, col_idx);
        // A width outside the bounds, maximised or dragged there, would be clamped back across
        // them, so the key would move it against its own direction.
        if (amount > 0.0 && current >= max_ratio) || (amount < 0.0 && current <= min_ratio) {
            return;
        }
        let clamped = column_ratio(ColumnKind::Bounded, current, amount, min_ratio, max_ratio);
        state.set_column_ratio(selected, col_idx, clamped, false);
        if reveals_only {
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

    use super::{Column, ScrollingLayoutSystem, rebased_offset};
    use crate::layout::WindowLayoutConstraints;
    use crate::layout::domain::area::compute_tiling_area;
    use crate::layout::domain::edge_drag::Drag;
    use crate::layout::domain::tile;
    use crate::layout::settings::{
        CenterFocusedColumn, Gaps, ScrollingLayoutSettings, WindowInsertionPoint,
    };
    use crate::layout::{Direction, LayoutId, ResizeOrientation};
    use rini_core::ids::{WindowId, pid_t};
    use rustc_hash::FxHashMap as HashMap;

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
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let constraints = HashMap::default();
        system.calculate_layout(layout, screen, &constraints, &tile::Borders::default(), gaps)
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

    /// What `ctrl-F` asks of the layout: the toggle, on the selection.
    fn toggle_selection(system: &mut ScrollingLayoutSystem, layout: LayoutId) {
        let selected = system.selected_window(layout).expect("a selection to toggle");
        assert!(system.toggle_full_width(layout, selected));
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
            &tile::Borders::default(),
            &Gaps::default(),
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
            width_dragged: false,
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
            &tile::Borders::default(),
            &Gaps::default(),
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
            &tile::Borders::default(),
            &Gaps::default(),
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
            width_dragged: false,
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
            &tile::Borders::default(),
            &Gaps::default(),
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
        let gaps = Gaps::default();
        let tiling = compute_tiling_area(screen, &gaps);
        let frames = system.calculate_layout(layout, screen, &constraints, &tile::Borders::default(), &gaps);
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
        let frames = render(&system, layout, screen(1000.0, 800.0), &Gaps::default());

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
        let settings = ScrollingLayoutSettings::default();
        let (mut system, layout, _, w2) = setup_two_windows(settings);
        system.center_selected_column(layout);

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
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

    /// With `center-focused-column "never"`, moving a centred column drops the centring: the
    /// anchor goes from the centre to the left edge while the scroll offset stays put, and every
    /// column moves on screen by that much. The strip's origin says so; the offset alone did not.
    #[test]
    fn the_strip_origin_moves_with_the_anchor_as_well_as_the_offset() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.column_width_ratio = 1.0 / 3.0;
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let windows: Vec<WindowId> = (1..=5).map(|i| wid(1, i)).collect();
        for &window in &windows {
            system.add_window_after_selection(layout, window);
        }
        let (screen, gaps) = (screen(1728.0, 1117.0), Gaps::default());
        assert_eq!(system.strip_origin(layout), None, "nothing laid out yet");
        system.select_window(layout, windows[2]);
        system.center_selected_column(layout);
        let centred = render(&system, layout, screen, &gaps);
        let (offset, origin) = (scroll_offset(&system, layout), system.strip_origin(layout));
        let tiling = compute_tiling_area(screen, &gaps);
        let first = frame_for(&centred, windows[0]).origin.x - tiling.origin.x;
        assert!(
            (origin.unwrap() - first).abs() < 1.0,
            "{origin:?} against {first}"
        );

        assert!(system.move_selection(layout, Direction::Right));
        let moved = render(&system, layout, screen, &gaps);
        assert!(
            (scroll_offset(&system, layout) - offset).abs() < 1e-9,
            "the offset stays put"
        );
        let shift = system.strip_origin(layout).unwrap() - origin.unwrap();
        assert!(shift.abs() > 100.0, "the anchor left the centre: {shift}");
        for &window in [windows[0], windows[1], windows[4]].iter() {
            let travel = frame_for(&moved, window).origin.x - frame_for(&centred, window).origin.x;
            assert!(
                (travel - shift).abs() < 1.0,
                "{window:?} moved {travel}, the origin {shift}"
            );
        }
    }

    #[test]
    fn center_selection_clears_when_focus_moves() {
        let settings = ScrollingLayoutSettings::default();
        let (mut system, layout, _, _) = setup_two_windows(settings);
        system.center_selected_column(layout);
        let _ = system.move_focus(layout, Direction::Left);

        let state = system.layouts.get(layout).expect("layout state missing");
        assert_eq!(state.center_override_window, None);
    }

    #[test]
    fn center_selection_toggles_back_to_layout_alignment() {
        let settings = ScrollingLayoutSettings::default();
        let (mut system, layout, _, w2) = setup_two_windows(settings);

        // First call centers the current selection.
        system.center_selected_column(layout);
        // Second call on the same selection toggles centering off.
        system.center_selected_column(layout);

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
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
        settings.column_width_ratio = 0.45;
        settings.min_column_width_ratio = 0.2;
        settings.max_column_width_ratio = 0.9;
        let (mut system, layout, w1, w2) = setup_two_windows(settings);

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();

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
        settings.center_focused_column = crate::layout::settings::CenterFocusedColumn::Always;
        settings.column_width_ratio = 0.45;
        settings.min_column_width_ratio = 0.2;
        settings.max_column_width_ratio = 0.9;
        let (mut system, layout, _, _) = setup_two_windows(settings);

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
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
        settings.center_focused_column = crate::layout::settings::CenterFocusedColumn::Always;
        let (mut system, layout, w1, w2) = setup_two_windows(settings);
        let _ = system.move_focus(layout, Direction::Left);
        system.resize_selection_by(layout, 0.12, ResizeOrientation::Horizontal);

        let gaps = Gaps::default();
        let frames = render(&system, layout, screen(1000.0, 800.0), &gaps);

        let w1_frame = frame_for(&frames, w1);
        let w2_frame = frame_for(&frames, w2);

        let expected_w2_x = w1_frame.origin.x + w1_frame.size.width + gaps.inner;
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
        settings.column_width_ratio = 0.45;
        settings.min_column_width_ratio = 0.2;
        settings.max_column_width_ratio = 0.9;
        let (mut system, layout, w1, _) = setup_two_windows(settings);

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
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
        settings.column_width_ratio = 0.42;
        settings.min_column_width_ratio = 0.2;
        settings.max_column_width_ratio = 0.9;
        let (mut system, layout, w1, _) = setup_two_windows(settings);

        // Make focused-left column wider so selected widths differ across focus moves.
        let _ = system.move_focus(layout, Direction::Left);
        system.resize_selection_by(layout, 0.15, ResizeOrientation::Horizontal);

        let screen = screen(1200.0, 800.0);
        let gaps = Gaps::default();

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
        settings.column_width_ratio = 0.45;
        settings.min_column_width_ratio = 0.2;
        settings.max_column_width_ratio = 0.95;
        let (mut system, layout, _, w2) = setup_two_windows(settings); // selected rightmost

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();

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
        let settings = ScrollingLayoutSettings::default();
        let (mut system, layout, _, w2) = setup_two_windows(settings);

        system.center_selected_column(layout);

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
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
        let gaps = Gaps::default();

        // 1. Initial layout calculation (should be split equally)
        let frames_before = render(&system, layout, screen, &gaps);
        let f1_before = frame_for(&frames_before, w1);
        let f2_before = frame_for(&frames_before, w2);
        assert!((f1_before.size.height - f2_before.size.height).abs() < 1.0);

        // 2. Select w1 and resize it
        assert!(system.select_window(layout, w1));
        let mut new_f1 = f1_before;
        new_f1.size.height = f1_before.size.height + 100.0;

        let drag = Drag::report(f1_before, new_f1, None);
        system.on_window_resized(layout, w1, Some(drag), new_f1, 0.0, screen, &gaps);

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
        let gaps = Gaps::default();
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
        let gaps = Gaps::default();
        let before = frame_for(&render(&system, layout, screen, &gaps), w1);
        system.resize_selection_by(layout, 0.05, ResizeOrientation::Smart);
        let after = frame_for(&render(&system, layout, screen, &gaps), w1);

        assert!(after.size.height > before.size.height);
        assert!((after.size.width - before.size.width).abs() < 1.0);
    }

    #[test]
    fn niri_new_window_reveal_does_not_unnecessarily_push_offscreen() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.column_width_ratio = 0.4;
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);

        // Add first window
        system.add_window_after_selection(layout, w1);
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
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
    fn always_centring_sits_the_outer_columns_flush() {
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();

        {
            let mut settings = ScrollingLayoutSettings::default();
            settings.center_focused_column = crate::layout::settings::CenterFocusedColumn::Always;
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
        let gaps = Gaps::default();
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

        system.restore_window_full_width(layout, w1);
        assert!(system.is_window_full_width(layout, w1));

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let frame = frame_for(&render(&system, layout, screen, &gaps), w1);
        assert!(
            (frame.size.width - 1000.0).abs() < 1.0,
            "full-width mode must fill the tiling width, got {frame:?}"
        );

        // And it round-trips back to the preset, so the mode is not a one-way door.
        assert!(system.select_window(layout, w1));
        toggle_selection(&mut system, layout);
        let frame = frame_for(&render(&system, layout, screen, &gaps), w1);
        assert!((frame.size.width - 400.0).abs() < 1.0, "got {frame:?}");
    }

    #[test]
    fn app_reconciliation_honors_updated_next_to_selection_policy() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.window_insertion_point = WindowInsertionPoint::EndOfTree;
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);
        let w3 = wid(1, 3);
        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);
        assert!(system.select_window(layout, w1));

        settings.window_insertion_point = WindowInsertionPoint::NextToSelection;
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

        settings.window_insertion_point = WindowInsertionPoint::EndOfTree;
        system.update_settings(&settings);
        system.set_windows_for_app(layout, 1, vec![w1, w2, w3]);

        assert_eq!(system.all_windows_in_layout(layout), vec![w1, w2, w3]);
    }

    // ── full-width columns participate in the strip ─────────────────────────

    fn niri_settings(ratio: f64) -> ScrollingLayoutSettings {
        let mut settings = ScrollingLayoutSettings::default();
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
        let gaps = Gaps::default();

        assert!(system.select_window(layout, w1));
        toggle_selection(&mut system, layout);

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
        let gaps = Gaps::default();

        assert!(system.select_window(layout, w1));
        toggle_selection(&mut system, layout);

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
        let gaps = Gaps::default();

        assert!(system.select_window(layout, w1));
        let width_before = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;

        toggle_selection(&mut system, layout);
        let width_full = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;

        toggle_selection(&mut system, layout);
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

    /// An app reporting its own size while its column is full width does not replace the width the
    /// column goes back to. Found 2026-09-30: an app that put its zoom frame back after rini's write,
    /// or a terminal snapping to its cell grid, left the toggle back at the clamped maximum ratio
    /// instead of the width it had.
    #[test]
    fn a_resize_report_under_full_width_keeps_the_width_to_return_to() {
        let (mut system, layout, w1, _) = setup_two_windows(niri_settings(0.5));
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        assert!(system.select_window(layout, w1));
        let width_before = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;

        toggle_selection(&mut system, layout);
        let full = frame_for(&render(&system, layout, screen, &gaps), w1);
        let settled = CGRect::new(full.origin, CGSize::new(full.size.width - 6.0, full.size.height - 4.0));
        system.on_window_resized(layout, w1, None, settled, 0.0, screen, &gaps);
        assert_eq!(
            frame_for(&render(&system, layout, screen, &gaps), w1).size.width,
            full.size.width,
            "still full width"
        );

        toggle_selection(&mut system, layout);
        let width_after = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;
        assert!(
            (width_after - width_before).abs() < 2.0,
            "expected {width_before} after toggling back, got {width_after}"
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
        let gaps = Gaps::default();

        // Select the last column, which sits at the right edge of the viewport.
        assert!(system.select_window(layout, w3));
        let _ = render(&system, layout, screen, &gaps);

        toggle_selection(&mut system, layout);
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
        let gaps = Gaps::default();
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

    /// The bounds the shipped config uses: a maximum well below 1.0, so that `ctrl-+` cannot crawl
    /// to the full viewport and full width stays `ctrl-F`'s to give.
    fn preset_settings() -> ScrollingLayoutSettings {
        let mut settings = niri_settings(0.5);
        settings.min_column_width_ratio = 0.33333;
        settings.max_column_width_ratio = 0.66667;
        settings.preset_column_widths = vec![0.33333, 0.5, 0.66667, 1.0];
        settings
    }

    /// The reported defect. `ctrl-F` maximised the window and `ctrl-R` then appeared dead: it wrote a
    /// width the layout pass ignored, because a maximised column is 1.0 whatever its stored width
    /// says. The step has to UNMAXIMISE, which is the only way out of full width besides `ctrl-F`.
    #[test]
    fn a_step_off_full_width_actually_resizes_the_window() {
        let (mut system, layout, w1, _) = setup_two_windows(preset_settings());
        let screen = screen(1200.0, 800.0);
        let gaps = Gaps::default();
        let tiling_width = compute_tiling_area(screen, &gaps).size.width;
        let width_of = |system: &ScrollingLayoutSystem| {
            frame_for(&render(system, layout, screen, &gaps), w1).size.width / tiling_width
        };

        assert!(system.select_window(layout, w1));
        toggle_selection(&mut system, layout);
        assert!(
            (width_of(&system) - 1.0).abs() < 0.02,
            "maximised {}",
            width_of(&system)
        );

        system.cycle_preset_column_width(layout);

        assert!(
            !system.is_window_full_width(layout, w1),
            "a window given a width is no longer maximised"
        );
        assert!(
            (width_of(&system) - 0.33333).abs() < 0.02,
            "nothing is wider than the viewport, so the cycle starts over at the narrowest; got {}",
            width_of(&system)
        );
    }

    /// Full width is reachable from the cycle even though it is above `max_column_width_ratio`,
    /// because it is the maximise MODE rather than a ratio. Asking for it as a ratio would be
    /// clamped to two thirds and the widest preset would be unreachable.
    #[test]
    fn the_cycle_reaches_full_width_past_the_configured_maximum() {
        let (mut system, layout, w1, _) = setup_two_windows(preset_settings());
        let screen = screen(1200.0, 800.0);
        let gaps = Gaps::default();
        let tiling_width = compute_tiling_area(screen, &gaps).size.width;

        assert!(system.select_window(layout, w1));
        system.cycle_preset_column_width(layout); // 0.5 -> 0.66667
        system.cycle_preset_column_width(layout); // 0.66667 -> full

        assert!(system.is_window_full_width(layout, w1));
        let width = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;
        assert!(
            (width / tiling_width - 1.0).abs() < 0.02,
            "the widest preset must fill the viewport, got {}",
            width / tiling_width
        );
    }

    /// Growing a maximised column used to write a width nothing read. It cannot grow — there is
    /// nothing wider than the viewport — so it stays maximised rather than unmaximising onto
    /// `max_column_width_ratio`, which is NARROWER than what it has and would read as a shrink.
    #[test]
    fn growing_a_maximised_column_leaves_it_maximised() {
        let (mut system, layout, w1, _) = setup_two_windows(preset_settings());

        assert!(system.select_window(layout, w1));
        toggle_selection(&mut system, layout);
        system.resize_selection_by(layout, 0.05, ResizeOrientation::Horizontal);

        assert!(system.is_window_full_width(layout, w1));
    }

    /// Shrinking one, on the other hand, has somewhere to go: it leaves full width and starts from
    /// the width it actually HAS rather than the width it was before being maximised.
    #[test]
    fn shrinking_a_maximised_column_unmaximises_it() {
        let (mut system, layout, w1, _) = setup_two_windows(preset_settings());
        let screen = screen(1200.0, 800.0);
        let gaps = Gaps::default();
        let tiling_width = compute_tiling_area(screen, &gaps).size.width;

        assert!(system.select_window(layout, w1));
        toggle_selection(&mut system, layout);
        system.resize_selection_by(layout, -0.05, ResizeOrientation::Horizontal);

        assert!(!system.is_window_full_width(layout, w1));
        let ratio =
            frame_for(&render(&system, layout, screen, &gaps), w1).size.width / tiling_width;
        assert!(
            (ratio - 0.66667).abs() < 0.02,
            "one step down from the viewport, clamped to the configured maximum; got {ratio}"
        );
    }

    /// Found while fixing the above, not reported: the scroll gesture computed its own column widths
    /// and did not know about maximised ones, so it stepped the strip by `max_column_width_ratio` of
    /// the viewport for a column occupying all of it. One swipe left the next column part-way on
    /// screen. Both sides read `constraints::column_ratio` now.
    #[test]
    fn a_swipe_steps_over_a_maximised_column_by_its_real_width() {
        let (mut system, layout, w1, w2) = setup_two_windows(preset_settings());
        let screen = screen(1200.0, 800.0);
        let gaps = Gaps::default();
        let tiling_width = compute_tiling_area(screen, &gaps).size.width;

        assert!(system.select_window(layout, w1));
        toggle_selection(&mut system, layout);
        // The gesture reads the geometry the last layout pass recorded.
        render(&system, layout, screen, &gaps);
        assert_eq!(scroll_offset(&system, layout), 0.0);

        system.scroll_by_delta(layout, 1.0);

        let stepped = scroll_offset(&system, layout);
        assert!(
            (stepped - (tiling_width + gaps.inner)).abs() < 2.0,
            "one swipe should clear the whole maximised column ({} + gap), stepped {stepped}",
            tiling_width
        );
        assert!(
            frame_for(&render(&system, layout, screen, &gaps), w2).origin.x < tiling_width,
            "the next column should be on screen after the swipe"
        );
    }

    /// Two columns at ratio 0.5 must fit, so moving focus between them does not scroll the strip.
    #[test]
    fn two_half_width_columns_do_not_shift_when_focus_alternates() {
        let (mut system, layout, w1, w2) = setup_two_windows(niri_settings(0.5));
        let screen = screen(1728.0, 1117.0);
        // With zero gaps two 0.5 columns fit exactly and the overflow cannot appear.
        let mut gaps = Gaps::default();
        gaps.outer.left = 4.0;
        gaps.outer.right = 4.0;
        gaps.inner = 4.0;

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
        let gaps = Gaps::default();

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

    /// Only a changed gap re-bases the offset, and only once a layout has recorded where the selected
    /// column sat.
    #[test]
    fn only_a_changed_gap_rebases_the_offset() {
        assert_eq!(rebased_offset(2.0, 4.0, -4.0, 1726.0), Some(1730.0));
        assert_eq!(rebased_offset(4.0, 4.0, -4.0, 1726.0), None, "the same gap");
        assert_eq!(rebased_offset(2.0, 4.0, f64::NAN, 1726.0), None, "never laid out");
    }

    /// Found 2026-09-30: raising the gap between windows from 2 to 4 left a full-width column two
    /// columns along the strip at left 6 / right 2 instead of 4 / 4. The scroll offset is in strip
    /// points and each column's start includes the gaps before it, so the columns ahead of the
    /// selected one pushed it right. The selected column MUST stay where it was on screen.
    #[test]
    fn a_gap_change_leaves_the_selected_column_in_place() {
        let (mut system, layout, w1, w2) = setup_two_windows(niri_settings(0.5));
        let w3 = wid(1, 3);
        system.add_window_after_selection(layout, w3);
        assert!(system.select_window(layout, w3));
        system.resize_selection_by(layout, 0.5, ResizeOrientation::Horizontal);
        let screen = screen(1728.0, 1117.0);
        let mut gaps = Gaps::default();
        gaps.outer.left = 4.0;
        gaps.outer.right = 4.0;
        gaps.inner = 2.0;

        let before = frame_for(&render(&system, layout, screen, &gaps), w3);
        assert_eq!(before.origin.x, 4.0, "the full-width column starts at the left margin");

        gaps.inner = 4.0;
        let frames = render(&system, layout, screen, &gaps);
        let after = frame_for(&frames, w3);
        assert_eq!(after.origin.x, 4.0, "it stays at the left margin");
        assert_eq!(after.origin.x + after.size.width, 1724.0, "and ends at the right one");
        // The columns ahead of it are re-spaced with the new gap, one gap to its left.
        assert_eq!(frame_for(&frames, w2).origin.x + frame_for(&frames, w2).size.width, 0.0);
        let _ = w1;
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
        let mut gaps = Gaps::default();
        gaps.outer.left = 4.0;
        gaps.outer.right = 4.0;
        gaps.inner = 2.0;

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
        let expected = (tiling * 0.5 - gaps.inner / 2.0).round();
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
        let gaps = Gaps::default();

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
        let gaps = Gaps::default();

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

    fn stacked_three(
        settings: ScrollingLayoutSettings,
    ) -> (ScrollingLayoutSystem, LayoutId, [WindowId; 3]) {
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
        assert_eq!(
            shape(&system, layout),
            vec![vec![w[0], w[1], w[2]]],
            "fixture is one stack"
        );
        (system, layout, w)
    }

    /// Maximizing a stacked window used to leave it in the column, where it covered the siblings it
    /// was sharing with: the tree said three windows abreast and the screen showed one.
    #[test]
    fn maximizing_a_stacked_window_pulls_it_out_of_the_stack() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);

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
        toggle_selection(&mut system, layout);
        toggle_selection(&mut system, layout);

        assert_eq!(
            shape(&system, layout),
            vec![vec![w[0], w[1], w[2]]],
            "back in its old row"
        );
        assert!(!system.is_window_full_width(layout, w[1]));
    }

    /// A title-bar zoom names the window it zoomed, which need not be the selection.
    #[test]
    fn full_width_toggles_the_window_it_names_and_selects_it() {
        let (mut system, layout, w1, w2) = setup_two_windows(niri_settings(0.5));
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        assert!(system.select_window(layout, w2));
        let width_before = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;

        assert!(system.toggle_full_width(layout, w1));
        assert!(system.is_window_full_width(layout, w1));
        assert!(!system.is_window_full_width(layout, w2));
        assert_eq!(system.selected_window(layout), Some(w1));
        let width_full = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;
        assert_eq!(width_full, compute_tiling_area(screen, &gaps).size.width);

        assert!(system.toggle_full_width(layout, w1));
        let width_after = frame_for(&render(&system, layout, screen, &gaps), w1).size.width;
        assert!(!system.is_window_full_width(layout, w1));
        assert_eq!(width_after, width_before, "back to the width it had");
    }

    #[test]
    fn full_width_of_an_unselected_stacked_window_puts_it_back_in_its_stack() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());
        assert!(system.select_window(layout, w[0]));

        assert!(system.toggle_full_width(layout, w[1]));
        assert_eq!(shape(&system, layout), vec![vec![w[0], w[2]], vec![w[1]]]);

        assert!(system.toggle_full_width(layout, w[1]));
        assert_eq!(shape(&system, layout), vec![vec![w[0], w[1], w[2]]]);
    }

    #[test]
    fn full_width_of_a_window_outside_the_layout_changes_nothing() {
        let (mut system, layout, _, _) = setup_two_windows(niri_settings(0.5));
        let selected = system.selected_window(layout);
        let stranger = wid(9, 9);

        assert!(!system.toggle_full_width(layout, stranger));
        assert!(!system.is_window_full_width(layout, stranger));
        assert_eq!(system.selected_window(layout), selected);
    }

    // Row 0 has no window above it, so the anchor is the one BELOW and it has to be restored in
    // front of it rather than after it.
    #[test]
    fn the_top_of_a_stack_goes_back_to_the_top() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[0]));
        toggle_selection(&mut system, layout);
        assert_eq!(shape(&system, layout), vec![vec![w[1], w[2]], vec![w[0]]]);

        toggle_selection(&mut system, layout);
        assert_eq!(
            shape(&system, layout),
            vec![vec![w[0], w[1], w[2]]],
            "back on top, not below"
        );
    }

    /// The "if available" half. Every window it could go back beside has closed, so it stays the
    /// column it became rather than being put somewhere the user never had it.
    #[test]
    fn a_window_whose_stack_is_gone_stays_its_own_column() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);
        system.remove_window(w[0]);
        system.remove_window(w[2]);

        toggle_selection(&mut system, layout);
        assert_eq!(shape(&system, layout), vec![vec![w[1]]]);
        assert!(
            !system.is_window_full_width(layout, w[1]),
            "it still stops being maximized"
        );
    }

    // The anchor closing must not leave an origin pointing at it: restoring into a window that is
    // gone is how a stale record turns into a window in the wrong place.
    #[test]
    fn closing_the_anchor_alone_leaves_the_rest_of_the_stack_reachable() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);
        // w0 was the anchor, being directly above w1.
        system.remove_window(w[0]);

        toggle_selection(&mut system, layout);
        assert_eq!(
            shape(&system, layout),
            vec![vec![w[2]], vec![w[1]]],
            "no stack to rejoin"
        );
    }

    /// A window given a width has nowhere to return to.
    ///
    /// `ctrl-R` off full width leaves the window where it is, so the stack it was pulled out of stops
    /// being its destination and is forgotten. Keeping the record made the NEXT `ctrl-F` pair a
    /// teleport: maximising records no new origin (the window is alone in its column by then), so the
    /// press that unmaximised it read the stale one and dropped the window into a stack the user had
    /// not touched since.
    #[test]
    fn a_window_given_a_width_does_not_rejoin_its_old_stack_later() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);
        system.cycle_preset_column_width(layout);
        assert_eq!(shape(&system, layout), vec![vec![w[0], w[2]], vec![w[1]]]);

        // A fresh maximise and a fresh press to leave it. Nothing here asked for the old stack.
        toggle_selection(&mut system, layout);
        toggle_selection(&mut system, layout);

        assert_eq!(
            shape(&system, layout),
            vec![vec![w[0], w[2]], vec![w[1]]],
            "it stays the ordinary column ctrl-R made it"
        );
    }

    /// The other half of the same rule: a window that has NOT been given a width still round-trips.
    /// This is what stops the record being dropped too eagerly.
    #[test]
    fn a_maximised_window_still_rejoins_its_stack_on_the_second_press() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);
        toggle_selection(&mut system, layout);

        assert_eq!(shape(&system, layout), vec![vec![w[0], w[1], w[2]]]);
    }

    /// Reaching full width through the width cycle has to pull the window out of its stack, exactly
    /// as `ctrl-F` does. It fills the tiling area either way, so leaving it in the column would cover
    /// the siblings the tree still says are abreast of it.
    #[test]
    fn cycling_to_full_width_pulls_a_stacked_window_out_too() {
        let mut settings = ScrollingLayoutSettings::default();
        settings.preset_column_widths = vec![1.0];
        let (mut system, layout, w) = stacked_three(settings);

        assert!(system.select_window(layout, w[1]));
        system.cycle_preset_column_width(layout);

        assert_eq!(shape(&system, layout), vec![vec![w[0], w[2]], vec![w[1]]]);
        assert!(system.is_window_full_width(layout, w[1]));
    }

    /// Restoring a remembered full width at startup goes through the same transition, so a window
    /// whose column is shared is pulled out of it rather than left maximised over its siblings.
    #[test]
    fn restoring_a_remembered_full_width_pulls_the_window_out_of_its_stack() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());

        system.restore_window_full_width(layout, w[1]);

        assert_eq!(shape(&system, layout), vec![vec![w[0], w[2]], vec![w[1]]]);
        assert!(system.is_window_full_width(layout, w[1]));
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
        toggle_selection(&mut system, layout);
        assert_eq!(shape(&system, layout), vec![vec![a], vec![b]]);
        toggle_selection(&mut system, layout);
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
        toggle_selection(&mut system, layout);

        let mut constraints = HashMap::default();
        constraints.insert(
            narrow,
            WindowLayoutConstraints {
                is_resizable: true,
                max_width: 400.0,
                ..Default::default()
            },
        );
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let frames = system.calculate_layout(layout, screen, &constraints, &tile::Borders::default(), &gaps);

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
            WindowLayoutConstraints {
                is_resizable: true,
                min_height: 100.0,
                ..Default::default()
            },
        );
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let frames = system.calculate_layout(layout, screen, &constraints, &tile::Borders::default(), &gaps);

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
        let gaps = Gaps::default();
        let frames = system.calculate_layout(layout, screen, &constraints_none(), &tile::Borders::default(), &gaps);
        let (a, b) = (frame_for(&frames, top), frame_for(&frames, bottom));
        assert!(
            (a.size.height - b.size.height).abs() < 2.0,
            "top {a:?} bottom {b:?}"
        );
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
        let gaps = Gaps::default();
        let frames = system.calculate_layout(layout, screen, &constraints_none(), &tile::Borders::default(), &gaps);
        let (a, b) = (frame_for(&frames, top), frame_for(&frames, bottom));
        assert!(
            a.size.height > b.size.height + 10.0,
            "resize lost: top {a:?} bottom {b:?}"
        );
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
        let gaps = Gaps::default();
        let frames = system.calculate_layout(layout, screen, &constraints_none(), &tile::Borders::default(), &gaps);
        let heights: Vec<f64> = w.iter().map(|id| frame_for(&frames, *id).size.height).collect();
        let spread = heights.iter().cloned().fold(f64::MIN, f64::max)
            - heights.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            spread < 2.0,
            "three folded windows must share the height, got {heights:?}"
        );
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
            WindowLayoutConstraints {
                is_resizable: true,
                min_width: 600.0,
                ..Default::default()
            },
        );
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let frames = system.calculate_layout(layout, screen, &constraints, &tile::Borders::default(), &gaps);

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
        assert_eq!(
            shape(&system, layout),
            vec![vec![left], vec![right]],
            "and out again"
        );
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
        assert_eq!(
            shape(&system, layout),
            vec![vec![left], vec![right]],
            "and back out"
        );
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
            assert_eq!(
                shape(&system, layout),
                before,
                "{side:?} from a stack, window {selected:?}"
            );

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
            assert_ne!(
                shape(&system, layout),
                before,
                "{side:?} should have folded it in"
            );
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
        assert_eq!(
            system.selected_window(layout),
            Some(w[2]),
            "and it keeps the selection"
        );
    }

    #[test]
    fn folding_with_nothing_selected_does_nothing() {
        let (mut system, layout, w) = stacked_three(ScrollingLayoutSettings::default());
        let before = shape(&system, layout);

        system.clear_selection_for_test(layout);
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(
            shape(&system, layout),
            before,
            "no selection is not a licence to move w0"
        );
        let _ = w;
    }

    // ── a mouse drag on an edge ─────────────────────────────────────────────

    /// Kai's display on 2026-10-06: 1728pt wide with `gaps 3`, so the tiling area is x=3, w 1722.
    const KAI_SCREEN: CGRect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize { width: 1728.0, height: 1117.0 },
    };

    fn kai_s_gaps() -> Gaps {
        let edge = 3.0;
        Gaps {
            outer: crate::layout::settings::Edges {
                top: edge,
                left: edge,
                bottom: edge,
                right: edge,
            },
            inner: edge,
        }
    }

    /// A report's frame, `[x, w]` across the tiling area's full height.
    fn xw(x: f64, width: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, 3.0), CGSize::new(width, 1111.0))
    }

    /// `[x, w]` as `calculate_layout` returns it.
    fn laid_out_xw(system: &ScrollingLayoutSystem, layout: LayoutId, wid: WindowId) -> (f64, f64) {
        let frame = frame_for(&render(system, layout, KAI_SCREEN, &kai_s_gaps()), wid);
        (frame.origin.x, frame.size.width)
    }

    /// The app sizing `wid` itself, then a pass.
    fn app_sizes(system: &mut ScrollingLayoutSystem, layout: LayoutId, wid: WindowId, width: f64) {
        assert!(system.select_window(layout, wid));
        system.on_window_resized(layout, wid, None, xw(0.0, width), 0.0, KAI_SCREEN, &kai_s_gaps());
        render(system, layout, KAI_SCREEN, &kai_s_gaps());
    }

    fn drag(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        wid: WindowId,
        from: CGRect,
        to: CGRect,
    ) {
        drag_through(system, layout, wid, from, &[to]);
    }

    /// One drag's reports, each carrying the end the ones before it moved, as the reactor's drag
    /// session carries it.
    fn drag_through(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        wid: WindowId,
        from: CGRect,
        reports: &[CGRect],
    ) {
        let mut end = None;
        for &to in reports {
            let drag = Drag::report(from, to, end);
            end = drag.end;
            system.on_window_resized(layout, wid, Some(drag), to, 0.0, KAI_SCREEN, &kai_s_gaps());
        }
    }

    /// Kai's bounds (a third to two thirds, a half by default) and three columns: column 0 594pt
    /// wide and column 1, selected, 882pt.
    fn kai_s_columns(
        center: CenterFocusedColumn,
    ) -> (ScrollingLayoutSystem, LayoutId, [WindowId; 3]) {
        let mut settings = preset_settings();
        settings.column_width_ratio = 0.5;
        settings.center_focused_column = center;
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w = [wid(1, 1), wid(1, 2), wid(1, 3)];
        for window in w {
            system.add_window_after_selection(layout, window);
        }
        app_sizes(&mut system, layout, w[0], 596.0);
        app_sizes(&mut system, layout, w[1], 883.5);
        assert_eq!(laid_out_xw(&system, layout, w[0]).1, 594.0);
        assert_eq!(laid_out_xw(&system, layout, w[1]).1, 882.0);
        (system, layout, w)
    }

    /// Kai's columns under `never`: column 0 at [3, w 594], and column 1 at [600, w 882] with the
    /// strip at offset 0.
    fn kai_s_strip() -> (ScrollingLayoutSystem, LayoutId, [WindowId; 3]) {
        let (system, layout, w) = kai_s_columns(CenterFocusedColumn::Never);
        assert_eq!(laid_out_xw(&system, layout, w[0]), (3.0, 594.0));
        assert_eq!(laid_out_xw(&system, layout, w[1]), (600.0, 882.0));
        assert_eq!(scroll_offset(&system, layout), 0.0);
        (system, layout, w)
    }

    /// Kai's columns under `always`, column 1 centred at [423, w 882].
    fn kai_s_centred_strip() -> (ScrollingLayoutSystem, LayoutId, [WindowId; 3]) {
        let (system, layout, w) = kai_s_columns(CenterFocusedColumn::Always);
        assert_eq!(laid_out_xw(&system, layout, w[1]), (423.0, 882.0));
        (system, layout, w)
    }

    fn assert_near(got: f64, want: f64, what: &str) {
        assert!((got - want).abs() < 1e-6, "{what}: {got}, not {want}");
    }

    /// Kai's press 14, which landed at [577, w 1148] with the right edge pushed to the screen's.
    /// The dragged column takes the dragged width whole, so it ends where it was let go.
    #[test]
    fn a_left_edge_drag_grows_the_column_left_and_the_columns_before_it_scroll_by_the_change() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));

        assert_eq!(laid_out_xw(&system, layout, w[1]), (334.0, 1148.0));
        assert_near(scroll_offset(&system, layout), 266.0, "offset");
        assert_eq!(laid_out_xw(&system, layout, w[0]).0, -263.0);
    }

    /// Kai's press 16, which landed back at 1148pt, `max-column-width`. 1443/1722 takes no gap
    /// share.
    #[test]
    fn kai_s_drag_to_1443_keeps_1443() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));

        assert_eq!(laid_out_xw(&system, layout, w[1]), (39.0, 1443.0));
        assert_eq!(laid_out_xw(&system, layout, w[0]).0, -558.0);
    }

    #[test]
    fn a_left_edge_shrink_keeps_the_right_edge_while_the_strip_can_scroll() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        render(&system, layout, KAI_SCREEN, &kai_s_gaps());

        drag(&mut system, layout, w[1], xw(334.0, 1148.0), xw(434.0, 1048.0));
        assert_eq!(laid_out_xw(&system, layout, w[1]), (434.0, 1048.0));
        assert_near(scroll_offset(&system, layout), 166.0, "offset");
    }

    /// Keeping the right edge would need offset -100: space before the first column, which the
    /// strip never shows.
    #[test]
    fn a_left_edge_shrink_at_the_strip_start_keeps_the_left_edge() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(700.0, 782.0));

        assert_eq!(laid_out_xw(&system, layout, w[1]), (600.0, 782.0));
        assert_eq!(scroll_offset(&system, layout), 0.0);
    }

    /// Kai's press 8, which landed at 572pt, `min-column-width`. The dragged 487pt, with the left
    /// edge kept at the strip's start.
    #[test]
    fn a_drag_below_min_column_width_keeps_its_width() {
        let (mut system, layout, w) = kai_s_strip();
        app_sizes(&mut system, layout, w[1], 597.0);
        assert_eq!(laid_out_xw(&system, layout, w[1]), (600.0, 595.0));

        drag(&mut system, layout, w[1], xw(600.0, 595.0), xw(708.0, 487.0));
        assert_eq!(laid_out_xw(&system, layout, w[1]), (600.0, 487.0));
        assert_eq!(scroll_offset(&system, layout), 0.0);
    }

    /// A terminal settling to its cell grid after the release. Clamped, it would undo the drag.
    #[test]
    fn an_app_settling_under_a_dragged_width_keeps_it_unbounded() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));
        render(&system, layout, KAI_SCREEN, &kai_s_gaps());

        system.on_window_resized(layout, w[1], None, xw(39.0, 1440.0), 0.0, KAI_SCREEN, &kai_s_gaps());
        assert_eq!(laid_out_xw(&system, layout, w[1]), (39.0, 1440.0));
    }

    /// Only a drag that moves the left or right edge marks the width as dragged, so a stacked
    /// edge's drag leaves the column held to the bounds, and the app's own width with it.
    #[test]
    fn a_drag_on_a_stacked_edge_leaves_the_bounds_on_the_column() {
        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[1]));
        let from = at_yh(&system, layout, w[1], 374.0, 368.0);
        let to = at_yh(&system, layout, w[1], 274.0, 468.0);
        drag(&mut system, layout, w[1], from, to);

        let report = CGRect::new(from.origin, CGSize::new(1443.0, from.size.height));
        system.on_window_resized(layout, w[1], None, report, 0.0, KAI_SCREEN, &kai_s_gaps());
        assert_eq!(laid_out_xw(&system, layout, w[1]).1, 1148.0);
    }

    /// `ctrl-F` gives back the width the drag started from, 860pt, whole, as a dragged width lands.
    /// A stacked window stays in its stack, a column the whole viewport wide.
    #[test]
    fn a_drag_to_the_full_viewport_makes_a_lone_window_full_width() {
        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let lone = wid(2, 1);
        system.add_window_after_selection(layout, lone);
        let start = frame_for(&render(&system, layout, KAI_SCREEN, &kai_s_gaps()), lone);
        let full = CGRect::new(start.origin, CGSize::new(1722.0, start.size.height));
        drag(&mut system, layout, lone, start, full);

        assert!(system.is_window_full_width(layout, lone));
        assert_eq!(frame_for(&render(&system, layout, KAI_SCREEN, &kai_s_gaps()), lone), full);
        toggle_selection(&mut system, layout);
        let back = frame_for(&render(&system, layout, KAI_SCREEN, &kai_s_gaps()), lone);
        assert_eq!((start.size.width, back.size.width), (860.0, 860.0), "the toggle back");

        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[1]));
        let from = frame_for(&render(&system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]);
        let across = CGRect::new(from.origin, CGSize::new(1722.0, from.size.height));
        drag(&mut system, layout, w[1], from, across);
        assert!(!system.is_window_full_width(layout, w[1]));
        assert_eq!(shape(&system, layout), vec![w.to_vec()]);
        assert_eq!(laid_out_xw(&system, layout, w[1]), (3.0, 1722.0), "stacked");
    }

    /// Bounded, the gesture would step it by 1148pt plus the gap while the layout drew it 1443.
    #[test]
    fn the_scroll_gesture_steps_a_dragged_column_by_its_dragged_width() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));
        render(&system, layout, KAI_SCREEN, &kai_s_gaps());
        let before = scroll_offset(&system, layout);

        system.scroll_by_delta(layout, 1.0);
        assert_near(scroll_offset(&system, layout) - before, 1446.0, "step");
    }

    /// Clamped back to `max-column-width`, the grow would shrink it to 1148pt.
    #[test]
    fn a_grow_key_on_a_column_dragged_past_max_does_nothing() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));
        render(&system, layout, KAI_SCREEN, &kai_s_gaps());

        system.resize_selection_by(layout, 0.05, ResizeOrientation::Horizontal);
        assert_eq!(laid_out_xw(&system, layout, w[1]), (39.0, 1443.0));
    }

    /// Clamped up to `min-column-width`, the shrink would grow it to 572pt.
    #[test]
    fn a_shrink_key_on_a_column_dragged_below_min_does_nothing() {
        let (mut system, layout, w) = kai_s_strip();
        app_sizes(&mut system, layout, w[1], 597.0);
        drag(&mut system, layout, w[1], xw(600.0, 595.0), xw(708.0, 487.0));
        render(&system, layout, KAI_SCREEN, &kai_s_gaps());

        system.resize_selection_by(layout, -0.05, ResizeOrientation::Horizontal);
        assert_eq!(laid_out_xw(&system, layout, w[1]), (600.0, 487.0));
    }

    /// Dragged to 0.69977 of the tiling width, one step down is 0.64977, 1117pt after the gap
    /// share; from the bound it would be 1060. From 1443pt the step lands past the bound, on it.
    #[test]
    fn a_shrink_key_on_it_starts_from_its_width() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(277.0, 1205.0));
        render(&system, layout, KAI_SCREEN, &kai_s_gaps());
        system.resize_selection_by(layout, -0.05, ResizeOrientation::Horizontal);
        assert_eq!(laid_out_xw(&system, layout, w[1]).1, 1117.0, "inside the bounds");

        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));
        render(&system, layout, KAI_SCREEN, &kai_s_gaps());
        system.resize_selection_by(layout, -0.05, ResizeOrientation::Horizontal);
        assert_eq!(laid_out_xw(&system, layout, w[1]).1, 1148.0, "past max");
    }

    /// The preset is a width given by a key, so the bounds hold it again.
    #[test]
    fn ctrl_r_on_it_takes_the_first_preset_above() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));
        system.cycle_preset_column_width(layout);
        assert!(system.is_window_full_width(layout, w[1]), "past two thirds");

        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(782.0, 700.0));
        system.cycle_preset_column_width(layout);
        assert_eq!(laid_out_xw(&system, layout, w[1]).1, 860.0, "a half");
        system.on_window_resized(layout, w[1], None, xw(39.0, 1443.0), 0.0, KAI_SCREEN, &kai_s_gaps());
        assert_eq!(laid_out_xw(&system, layout, w[1]).1, 1148.0, "bounded again");
    }

    #[test]
    fn the_first_column_grown_from_its_left_edge_ends_grown_to_the_right() {
        let (mut system, layout, w) = kai_s_strip();
        assert!(system.select_window(layout, w[0]));
        render(&system, layout, KAI_SCREEN, &kai_s_gaps());
        drag(&mut system, layout, w[0], xw(3.0, 594.0), xw(-77.0, 674.0));
        assert_eq!(laid_out_xw(&system, layout, w[0]), (3.0, 674.0), "3 columns");
        assert_eq!(laid_out_xw(&system, layout, w[1]).0, 680.0);

        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let lone = wid(2, 1);
        system.add_window_after_selection(layout, lone);
        let (x, width) = laid_out_xw(&system, layout, lone);
        drag(&mut system, layout, lone, xw(x, width), xw(x - 80.0, width + 80.0));
        let (x_after, width_after) = laid_out_xw(&system, layout, lone);
        assert_eq!(x_after, x, "1 column");
        assert!(width_after > width + 75.0, "1 column: {width} -> {width_after}");
    }

    /// The left edge in by 200pt: 1522pt, past `max-column-width`, with the right edge kept.
    #[test]
    fn a_drag_on_a_full_width_column_ends_it_at_the_dragged_width() {
        let (mut system, layout, w) = kai_s_strip();
        toggle_selection(&mut system, layout);
        let full = frame_for(&render(&system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]);

        let shrunk = CGRect::new(
            CGPoint::new(full.origin.x + 200.0, full.origin.y),
            CGSize::new(full.size.width - 200.0, full.size.height),
        );
        drag(&mut system, layout, w[1], full, shrunk);
        assert!(!system.is_window_full_width(layout, w[1]));
        assert_eq!(frame_for(&render(&system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]), shrunk);
    }

    /// No width was dragged, so the window stays the full-width column it was.
    #[test]
    fn a_top_edge_drag_on_a_full_width_column_leaves_it_full_width() {
        let (mut system, layout, w) = kai_s_strip();
        toggle_selection(&mut system, layout);
        let full = frame_for(&render(&system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]);

        let lower = CGRect::new(
            CGPoint::new(full.origin.x, full.origin.y + 100.0),
            CGSize::new(full.size.width, full.size.height - 100.0),
        );
        drag(&mut system, layout, w[1], full, lower);
        assert!(system.is_window_full_width(layout, w[1]));
        assert_eq!(frame_for(&render(&system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]), full);
    }

    /// The layout file keeps the mark, so the bounds still pass it after a restart.
    /// A `Column` as rini read it before a drag had a mark, to read a new file as a rollback would.
    #[derive(serde::Deserialize)]
    struct ColumnBeforeDragged {
        windows: Vec<WindowId>,
        width_offset: f64,
        #[serde(default)]
        width_overridden: bool,
    }

    #[test]
    fn a_dragged_column_is_written_so_a_rini_from_before_it_reads_it() {
        let column = Column {
            windows: vec![wid(1, 1)],
            width_offset: 0.33797,
            width_overridden: true,
            width_dragged: true,
            ..Column::default()
        };
        let old: ColumnBeforeDragged =
            ron::from_str(&ron::ser::to_string(&column).unwrap()).expect("an older rini reads it");
        assert_eq!(old.windows, column.windows);
        assert_eq!(old.width_offset, 0.33797);
        assert!(old.width_overridden);
    }

    #[test]
    fn a_dragged_width_survives_a_save_and_restore() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));
        render(&system, layout, KAI_SCREEN, &kai_s_gaps());

        let mut restored: ScrollingLayoutSystem =
            ron::from_str(&ron::ser::to_string(&system).unwrap()).unwrap();
        restored.settings = system.settings.clone();
        assert_eq!(laid_out_xw(&restored, layout, w[1]).1, 1443.0);
    }

    #[test]
    fn a_window_resizing_itself_keeps_its_column_where_it_was() {
        let (mut system, layout, w) = kai_s_strip();
        system.on_window_resized(layout, w[1], None, xw(500.0, 982.0), 0.0, KAI_SCREEN, &kai_s_gaps());

        assert_eq!(laid_out_xw(&system, layout, w[1]), (600.0, 981.0));
        assert_eq!(scroll_offset(&system, layout), 0.0);
    }

    /// Pinned, column 2 would end with its right edge at 1482; revealed, it ends at the screen's
    /// edge, x 865.5 and w 859.5 before rounding.
    #[test]
    fn a_focus_change_after_a_left_edge_report_leaves_the_new_column_unpinned() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        assert!(system.select_window(layout, w[2]));

        assert_eq!(laid_out_xw(&system, layout, w[2]), (866.0, 860.0));
        assert_near(scroll_offset(&system, layout), 885.5, "offset");
    }

    /// A request for the dragged window made after its report and before the pass wins over the
    /// pin, which would put its right edge back at 1482 and its x at 336: centring, the reveal a key
    /// or `ctrl-F` asks for, and the alignment a focus move that finds no window asks for.
    #[test]
    fn a_scroll_request_after_a_left_edge_report_wins_over_the_kept_edge() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        system.center_selected_column(layout);
        assert_eq!(laid_out_xw(&system, layout, w[1]), (290.0, 1148.0), "centred");

        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        system.resize_selection_by(layout, 0.05, ResizeOrientation::Horizontal);
        assert_eq!(laid_out_xw(&system, layout, w[1]), (577.0, 1148.0), "a key, revealed");

        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        assert!(system.toggle_full_width(layout, w[1]));
        assert_eq!(laid_out_xw(&system, layout, w[1]), (3.0, 1722.0), "ctrl-F, revealed");

        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        assert_eq!(system.move_focus(layout, Direction::Down).0, None);
        assert_eq!(laid_out_xw(&system, layout, w[1]), (3.0, 1148.0), "a focus move that finds none");
    }

    /// Any frame under a held button counts as the drag, rini's own write to a neighbour coming
    /// back included.
    #[test]
    fn a_drag_report_for_a_window_other_than_the_focused_one_changes_no_width() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[0], xw(3.0, 594.0), xw(3.0, 694.0));

        assert_eq!(laid_out_xw(&system, layout, w[0]), (3.0, 594.0));
        assert_eq!(laid_out_xw(&system, layout, w[1]), (600.0, 882.0));
    }

    /// The release refocuses the window it let go of before its pass, which must not cost the pin.
    #[test]
    fn refocusing_the_window_after_a_left_edge_report_keeps_its_right_edge() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        assert!(system.select_window(layout, w[1]));

        assert_eq!(laid_out_xw(&system, layout, w[1]), (334.0, 1148.0));
        assert_near(scroll_offset(&system, layout), 266.0, "offset");
    }

    /// The selection passes to column 2 without a scroll request, and the drag's reveal is still
    /// pending. Pinned, column 2 would end at offset 264.5; revealed, flush with the screen's edge at
    /// x 578.5 and w 1146.5 before rounding.
    #[test]
    fn a_removed_window_s_pin_does_not_move_the_column_selected_after_it() {
        let (mut system, layout, w) = kai_s_strip();
        app_sizes(&mut system, layout, w[2], 1148.0);
        app_sizes(&mut system, layout, w[0], 596.0);
        assert!(system.select_window(layout, w[1]));
        assert_eq!(laid_out_xw(&system, layout, w[1]), (600.0, 882.0));

        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(500.0, 982.0));
        system.remove_window(w[1]);
        assert_eq!(system.selected_window(layout), Some(w[2]));

        assert_eq!(laid_out_xw(&system, layout, w[2]), (579.0, 1147.0));
        assert_near(scroll_offset(&system, layout), 21.5, "offset");
    }

    /// 684pt asked and laid out, 519pt from either edge of the tiling area. Undoubled it would end
    /// at [547, w 634].
    #[test]
    fn in_centred_mode_a_middle_column_dragged_out_50_from_its_left_edge_grows_100_and_stays_centred() {
        let (mut system, layout, w) = kai_s_centred_strip();
        app_sizes(&mut system, layout, w[1], 586.0);
        assert_eq!(laid_out_xw(&system, layout, w[1]), (572.0, 584.0));

        drag(&mut system, layout, w[1], xw(572.0, 584.0), xw(522.0, 634.0));
        assert_eq!(laid_out_xw(&system, layout, w[1]), (522.0, 684.0));
    }

    /// The first column, flush left, takes a right-edge drag as dragged; the last, flush right, a
    /// left-edge one. Doubled, they would end 752.5 and 756.5pt wide.
    #[test]
    fn in_centred_mode_the_end_columns_grow_away_from_their_flush_edge() {
        let (mut system, layout, w) = kai_s_centred_strip();
        assert!(system.select_window(layout, w[0]));
        assert_eq!(laid_out_xw(&system, layout, w[0]), (3.0, 594.0));
        drag(&mut system, layout, w[0], xw(3.0, 594.0), xw(3.0, 674.0));
        assert_eq!(laid_out_xw(&system, layout, w[0]), (3.0, 674.0), "first");

        app_sizes(&mut system, layout, w[2], 600.0);
        assert_eq!(laid_out_xw(&system, layout, w[2]), (1127.0, 598.0));
        drag(&mut system, layout, w[2], xw(1127.0, 598.0), xw(1047.0, 678.0));
        assert_eq!(laid_out_xw(&system, layout, w[2]), (1047.0, 678.0), "last");
    }

    /// Out 150 asks for 1182pt, past `max-column-width`, and takes it whole. Bounded, it would
    /// end at [290, w 1148].
    #[test]
    fn in_centred_mode_a_doubled_drag_past_max_column_width_keeps_its_width() {
        let (mut system, layout, w) = kai_s_centred_strip();
        drag(&mut system, layout, w[1], xw(423.0, 882.0), xw(273.0, 1032.0));

        assert_eq!(laid_out_xw(&system, layout, w[1]), (273.0, 1182.0));
    }

    /// Three windows stacked on Kai's display, each `[y, h]` 368.333pt high before rounding.
    fn kai_s_stack() -> (ScrollingLayoutSystem, LayoutId, [WindowId; 3]) {
        let (system, layout, w) = stacked_three(preset_settings());
        let rows: Vec<(f64, f64)> = w.iter().map(|&id| laid_out_yh(&system, layout, id)).collect();
        assert_eq!(rows, [(3.0, 368.0), (374.0, 368.0), (746.0, 368.0)]);
        (system, layout, w)
    }

    /// `[y, h]` as `calculate_layout` returns it.
    fn laid_out_yh(system: &ScrollingLayoutSystem, layout: LayoutId, wid: WindowId) -> (f64, f64) {
        let frame = frame_for(&render(system, layout, KAI_SCREEN, &kai_s_gaps()), wid);
        (frame.origin.y, frame.size.height)
    }

    /// `wid`'s laid-out frame with its top at `y` and `height` high.
    fn at_yh(
        system: &ScrollingLayoutSystem,
        layout: LayoutId,
        wid: WindowId,
        y: f64,
        height: f64,
    ) -> CGRect {
        let frame = frame_for(&render(system, layout, KAI_SCREEN, &kai_s_gaps()), wid);
        CGRect::new(CGPoint::new(frame.origin.x, y), CGSize::new(frame.size.width, height))
    }

    /// The middle window takes 99.667pt, its 468 less the 368.333 it had, from the top one alone.
    /// Spread over the column as before, the bottom window would end 318.5pt high.
    #[test]
    fn a_middle_window_s_top_edge_in_a_3_stack_leaves_the_bottom_window_alone() {
        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[1]));
        let from = at_yh(&system, layout, w[1], 374.0, 368.0);
        let to = at_yh(&system, layout, w[1], 274.0, 468.0);
        drag(&mut system, layout, w[1], from, to);

        assert_eq!(laid_out_yh(&system, layout, w[0]), (3.0, 269.0), "top");
        assert_eq!(laid_out_yh(&system, layout, w[1]), (275.0, 468.0), "middle");
        assert_eq!(laid_out_yh(&system, layout, w[2]), (746.0, 368.0), "bottom");
    }

    /// A window that sizes itself takes its height from the rest of its column in their
    /// proportions. Unequal first, 268.667 and 368.333 around a 468.333 middle, so sharing differs
    /// from trading with one neighbour.
    #[test]
    fn a_stacked_window_sizing_itself_takes_its_height_from_the_others_in_proportion() {
        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[1]));
        let from = at_yh(&system, layout, w[1], 374.0, 368.0);
        let out = at_yh(&system, layout, w[1], 274.0, 468.0);
        drag(&mut system, layout, w[1], from, out);

        let grown = at_yh(&system, layout, w[1], 275.0, 568.0);
        system.on_window_resized(layout, w[1], None, grown, 0.0, KAI_SCREEN, &kai_s_gaps());

        let rows: Vec<(f64, f64)> = w.iter().map(|&id| laid_out_yh(&system, layout, id)).collect();
        assert_eq!(rows[1].1, 568.0, "the window's own height");
        let (top, bottom) = (rows[0].1, rows[2].1);
        assert!((top / bottom - 268.667 / 368.333).abs() < 0.01, "{rows:?}");
        assert!((top + 568.0 + bottom - 1105.0).abs() <= 1.0, "{rows:?}");
    }

    /// Every report carries the frame the drag started from; each trades only what changed since
    /// the last, so the middle window ends 99.667pt shorter, not shorter by all three reports.
    #[test]
    fn three_reports_of_one_drag_move_the_neighbour_by_the_drag_once() {
        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[0]));
        let from = at_yh(&system, layout, w[0], 3.0, 368.0);
        let reports = [398.0, 428.0, 468.0].map(|height| at_yh(&system, layout, w[0], 3.0, height));
        drag_through(&mut system, layout, w[0], from, &reports);

        assert_eq!(laid_out_yh(&system, layout, w[0]), (3.0, 468.0), "top");
        assert_eq!(laid_out_yh(&system, layout, w[1]), (474.0, 269.0), "middle");
        assert_eq!(laid_out_yh(&system, layout, w[2]), (746.0, 368.0), "bottom");
    }

    /// Back at its start the report moved neither edge. The column ends as it began, within the
    /// third of a point the start frame's rounding leaves, not at the 468pt the report before left.
    #[test]
    fn a_stacked_edge_dragged_back_to_where_it_started_gives_the_height_back() {
        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[1]));
        let from = at_yh(&system, layout, w[1], 374.0, 368.0);
        let out = at_yh(&system, layout, w[1], 274.0, 468.0);
        drag_through(&mut system, layout, w[1], from, &[out, from]);

        let rows: Vec<(f64, f64)> = w.iter().map(|&id| laid_out_yh(&system, layout, id)).collect();
        assert_eq!(rows, [(3.0, 369.0), (375.0, 368.0), (746.0, 368.0)]);
    }

    /// Marked as a deliberate split, the column would keep its two 552pt rows when the third window
    /// came back with the 1.0 its own column held, and that window would end 1pt high.
    #[test]
    fn a_width_drag_on_a_stack_leaves_its_heights_and_a_window_folded_back_shares_evenly() {
        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[2]));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&system, layout), vec![vec![w[0], w[1]], vec![w[2]]]);

        assert!(system.select_window(layout, w[0]));
        let from = frame_for(&render(&system, layout, KAI_SCREEN, &kai_s_gaps()), w[0]);
        let wider = CGRect::new(from.origin, CGSize::new(from.size.width + 100.0, from.size.height));
        drag(&mut system, layout, w[0], from, wider);
        assert_eq!(laid_out_yh(&system, layout, w[0]), (3.0, 554.0), "top");
        assert_eq!(laid_out_yh(&system, layout, w[1]), (560.0, 554.0), "bottom");

        assert!(system.select_window(layout, w[2]));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&system, layout), vec![vec![w[0], w[1], w[2]]]);
        let rows: Vec<(f64, f64)> = w.iter().map(|&id| laid_out_yh(&system, layout, id)).collect();
        assert_eq!(rows, [(3.0, 368.0), (374.0, 368.0), (746.0, 368.0)]);
    }

    // ── the view full width returns to ──────────────────────────────────────

    fn kai_s_frames(system: &ScrollingLayoutSystem, layout: LayoutId) -> Vec<(WindowId, CGRect)> {
        render(system, layout, KAI_SCREEN, &kai_s_gaps())
    }

    fn assert_same_frames(got: &[(WindowId, CGRect)], want: &[(WindowId, CGRect)]) {
        assert_eq!(got.len(), want.len(), "{got:?} against {want:?}");
        for ((got_id, got), (want_id, want)) in got.iter().zip(want) {
            assert_eq!(got_id, want_id);
            for (g, w) in [
                (got.origin.x, want.origin.x),
                (got.origin.y, want.origin.y),
                (got.size.width, want.size.width),
                (got.size.height, want.size.height),
            ] {
                assert!((g - w).abs() < 1e-6, "{got_id:?} at {got:?}, not {want:?}");
            }
        }
    }

    /// `ctrl-F` on `wid` twice, with the pass after each.
    fn maximize_and_restore(system: &mut ScrollingLayoutSystem, layout: LayoutId, wid: WindowId) {
        assert!(system.toggle_full_width(layout, wid));
        kai_s_frames(system, layout);
        assert!(system.toggle_full_width(layout, wid));
    }

    /// Kai's report of 2026-10-09 on their display: Z a half, A and B a third, the view reached by
    /// walking focus right and back, so A sits at x=578 with the strip at 287.48852.
    fn kai_s_thirds() -> (ScrollingLayoutSystem, LayoutId, [WindowId; 3]) {
        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let w = [wid(1, 1), wid(1, 2), wid(1, 3)];
        for window in w {
            system.add_window_after_selection(layout, window);
            kai_s_frames(&system, layout);
        }
        system.set_column_width_offset(layout, w[1], 0.33333 - 0.5, false);
        system.set_column_width_offset(layout, w[2], 0.33333 - 0.5, false);
        assert!(system.select_window(layout, w[0]));
        kai_s_frames(&system, layout);
        for direction in [Direction::Right, Direction::Right, Direction::Left] {
            system.move_focus(layout, direction);
            kai_s_frames(&system, layout);
        }
        assert_eq!(system.selected_window(layout), Some(w[1]));
        assert_near(scroll_offset(&system, layout), 287.48852, "offset");
        assert_eq!(laid_out_xw(&system, layout, w[1]), (578.0, 572.0));
        (system, layout, w)
    }

    #[test]
    fn restoring_from_full_width_puts_the_strip_back_where_it_was() {
        let (mut system, layout, w) = kai_s_thirds();
        let before = kai_s_frames(&system, layout);

        assert!(system.toggle_full_width(layout, w[1]));
        assert_eq!(laid_out_xw(&system, layout, w[1]), (3.0, 1722.0), "the strip scrolled");
        assert!(system.toggle_full_width(layout, w[1]));

        assert_same_frames(&kai_s_frames(&system, layout), &before);
        assert_near(scroll_offset(&system, layout), 287.48852, "offset");
    }

    /// Two halves, B on the right: full width scrolls A off screen, and the restore brings it back.
    #[test]
    fn a_right_column_restores_with_its_left_neighbour_on_screen() {
        let (mut system, layout, w1, w2) = setup_two_windows(preset_settings());
        assert!(system.select_window(layout, w2));
        let before = kai_s_frames(&system, layout);
        assert_eq!(laid_out_xw(&system, layout, w2), (866.0, 860.0));

        maximize_and_restore(&mut system, layout, w2);

        assert_same_frames(&kai_s_frames(&system, layout), &before);
        assert_eq!(laid_out_xw(&system, layout, w1), (3.0, 860.0));
    }

    /// B2 of [A][B1,B2]: full width puts B2 two columns along, and the restore folds it back with
    /// the strip where it was.
    #[test]
    fn a_right_stack_restores_with_its_left_neighbour_on_screen() {
        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let [a, b1, b2] = [wid(1, 1), wid(1, 2), wid(1, 3)];
        for window in [a, b1, b2] {
            system.add_window_after_selection(layout, window);
        }
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&system, layout), vec![vec![a], vec![b1, b2]]);
        let before = kai_s_frames(&system, layout);
        assert_eq!(scroll_offset(&system, layout), 0.0);

        assert!(system.toggle_full_width(layout, b2));
        kai_s_frames(&system, layout);
        assert_near(scroll_offset(&system, layout), 1725.0, "full width");
        assert!(system.toggle_full_width(layout, b2));

        assert_same_frames(&kai_s_frames(&system, layout), &before);
        assert_eq!(shape(&system, layout), vec![vec![a], vec![b1, b2]]);
    }

    #[test]
    fn a_centred_column_comes_back_centred() {
        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let w = [wid(1, 1), wid(1, 2), wid(1, 3)];
        for window in w {
            system.add_window_after_selection(layout, window);
        }
        assert!(system.select_window(layout, w[1]));
        system.center_selected_column(layout);
        let before = kai_s_frames(&system, layout);
        assert_eq!(laid_out_xw(&system, layout, w[1]), (434.0, 860.0));

        maximize_and_restore(&mut system, layout, w[1]);

        assert_same_frames(&kai_s_frames(&system, layout), &before);
    }

    /// The window's own focus event, which the raise after the toggle sends.
    #[test]
    fn refocusing_the_full_width_window_keeps_the_view_it_returns_to() {
        let (mut system, layout, w) = kai_s_thirds();
        let before = kai_s_frames(&system, layout);

        assert!(system.toggle_full_width(layout, w[1]));
        kai_s_frames(&system, layout);
        assert!(system.select_window(layout, w[1]));
        kai_s_frames(&system, layout);
        assert!(system.toggle_full_width(layout, w[1]));

        assert_same_frames(&kai_s_frames(&system, layout), &before);
    }

    #[test]
    fn a_focus_change_while_full_width_restores_by_the_reveal() {
        let (mut system, layout, w) = kai_s_thirds();

        assert!(system.toggle_full_width(layout, w[1]));
        kai_s_frames(&system, layout);
        for window in [w[0], w[1]] {
            assert!(system.select_window(layout, window));
            kai_s_frames(&system, layout);
        }
        assert!(system.toggle_full_width(layout, w[1]));

        assert_eq!(laid_out_xw(&system, layout, w[1]), (3.0, 572.0));
    }

    #[test]
    fn a_strip_scroll_while_full_width_restores_by_the_reveal() {
        let (mut system, layout, w) = kai_s_thirds();

        assert!(system.toggle_full_width(layout, w[1]));
        kai_s_frames(&system, layout);
        system.scroll_by_delta(layout, -1.0);
        kai_s_frames(&system, layout);
        assert!(system.toggle_full_width(layout, w[1]));

        assert_eq!(laid_out_xw(&system, layout, w[1]), (866.0, 572.0));
    }

    /// A title-bar zoom of B while the last pass had A selected: there is no view of B's column to
    /// go back to.
    #[test]
    fn a_toggle_on_a_window_the_last_pass_did_not_select_restores_by_the_reveal() {
        let (mut system, layout, w) = kai_s_thirds();

        maximize_and_restore(&mut system, layout, w[2]);

        assert_eq!(laid_out_xw(&system, layout, w[2]), (3.0, 572.0));
    }

    // ── the view a fold returns to ──────────────────────────────────────────

    /// [A1][B1..] on Kai's display, two halves with the stack on the right, the strip at 0 with the
    /// stack at x=866.
    fn a_stack_on_the_right(rows: u32) -> (ScrollingLayoutSystem, LayoutId, WindowId, Vec<WindowId>) {
        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let a = wid(1, 1);
        let b: Vec<WindowId> = (2..2 + rows).map(|idx| wid(1, idx)).collect();
        for &window in std::iter::once(&a).chain(&b) {
            system.add_window_after_selection(layout, window);
        }
        for &window in &b[1..] {
            assert!(system.select_window(layout, window));
            system.toggle_fold_of_selection(layout, Direction::Left);
        }
        assert!(system.select_window(layout, a));
        kai_s_frames(&system, layout);
        assert_eq!(shape(&system, layout), vec![vec![a], b.clone()]);
        assert_eq!(scroll_offset(&system, layout), 0.0);
        (system, layout, a, b)
    }

    /// One fold key on the selected `window` twice, with the pass after each.
    fn fold_out_and_back(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        window: WindowId,
        side: Direction,
    ) {
        assert_eq!(system.toggle_fold_of_selection(layout, side), vec![window]);
        kai_s_frames(system, layout);
        assert_eq!(system.toggle_fold_of_selection(layout, side), vec![window]);
    }

    /// `ctrl-,` from a right stack lands the window right of it and scrolls one column: the fold
    /// back used to leave A1 at x=-860 with half the screen empty.
    #[test]
    fn a_fold_round_trip_puts_the_strip_back_where_it_was() {
        for rows in [2, 3] {
            for row in 0..rows as usize {
                for side in [Direction::Left, Direction::Right] {
                    let (mut system, layout, a, b) = a_stack_on_the_right(rows);
                    assert!(system.select_window(layout, b[row]));
                    let before = kai_s_frames(&system, layout);
                    let shape_before = shape(&system, layout);

                    fold_out_and_back(&mut system, layout, b[row], side);

                    let case = format!("row {row} of {rows}, {side:?}");
                    assert_eq!(shape(&system, layout), shape_before, "{case}");
                    assert_same_frames(&kai_s_frames(&system, layout), &before);
                    assert_eq!(laid_out_xw(&system, layout, a), (3.0, 860.0), "{case}");
                }
            }
        }
    }

    #[test]
    fn the_fold_out_of_a_right_stack_scrolls_the_strip() {
        let (mut system, layout, _a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[1]));
        kai_s_frames(&system, layout);

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&system, layout);

        assert_near(scroll_offset(&system, layout), 862.5, "offset");
        assert_eq!(laid_out_xw(&system, layout, b[1]), (866.0, 860.0));
    }

    #[test]
    fn a_centred_stack_comes_back_centred_from_a_fold() {
        let (mut system, layout, _a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[1]));
        system.add_window_after_selection(layout, wid(1, 9));
        assert!(system.select_window(layout, b[1]));
        system.center_selected_column(layout);
        let before = kai_s_frames(&system, layout);
        assert_eq!(laid_out_xw(&system, layout, b[1]).0, 434.0);

        fold_out_and_back(&mut system, layout, b[1], Direction::Left);

        assert_same_frames(&kai_s_frames(&system, layout), &before);
    }

    #[test]
    fn refocusing_the_folded_out_window_keeps_the_view_it_returns_to() {
        let (mut system, layout, a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[1]));
        let before = kai_s_frames(&system, layout);

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&system, layout);
        assert!(system.select_window(layout, b[1]));
        kai_s_frames(&system, layout);
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_same_frames(&kai_s_frames(&system, layout), &before);
        assert_eq!(laid_out_xw(&system, layout, a), (3.0, 860.0));
    }

    #[test]
    fn a_strip_scroll_between_the_folds_folds_back_by_the_reveal() {
        let (mut system, layout, _a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[1]));
        kai_s_frames(&system, layout);

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&system, layout);
        system.scroll_by_delta(layout, -0.5);
        kai_s_frames(&system, layout);
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(laid_out_xw(&system, layout, b[1]), (435.0, 860.0));
    }

    /// Out by `ctrl-,`, in by `ctrl-.`: the second key undoes nothing the first did, so the strip
    /// only scrolls as far as it must.
    #[test]
    fn folding_back_with_the_other_key_folds_back_by_the_reveal() {
        let (mut system, layout, a, b) = a_stack_on_the_right(2);
        let c = wid(1, 9);
        assert!(system.select_window(layout, b[1]));
        system.add_window_after_selection(layout, c);
        assert!(system.select_window(layout, b[1]));
        kai_s_frames(&system, layout);
        assert_eq!(scroll_offset(&system, layout), 0.0);
        assert_eq!(shape(&system, layout), vec![vec![a], b.clone(), vec![c]]);

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&system, layout);
        system.toggle_fold_of_selection(layout, Direction::Right);

        assert_eq!(shape(&system, layout), vec![vec![a], b.clone(), vec![c]]);
        assert_eq!(laid_out_xw(&system, layout, b[1]), (3.0, 860.0));
    }

    /// A border inside each tiled window's slot: the strip is laid out from the slots as with no
    /// border, and every window comes back inset (`tile.rs`).
    mod borders {
        use super::*;
        use crate::layout::domain::tile::{self, window_in};

        const BORDER: f64 = 4.0;

        fn laid_out_with(
            system: &ScrollingLayoutSystem,
            layout: LayoutId,
            constraints: &HashMap<WindowId, WindowLayoutConstraints>,
            border: f64,
        ) -> Vec<(WindowId, CGRect)> {
            system.calculate_layout(layout, KAI_SCREEN, constraints, &tile::Borders::every(border), &kai_s_gaps())
        }

        fn bordered(system: &ScrollingLayoutSystem, layout: LayoutId, wid: WindowId) -> CGRect {
            frame_for(&laid_out_with(system, layout, &constraints_none(), BORDER), wid)
        }

        /// A window's report, `[x, w]` inside a full-height slot.
        fn window_xw(x: f64, width: f64) -> CGRect { window_in(xw(x - BORDER, width + 2.0 * BORDER), BORDER) }

        fn drag_bordered(system: &mut ScrollingLayoutSystem, layout: LayoutId, wid: WindowId, from: CGRect, to: CGRect) {
            let drag = Drag::report(from, to, None);
            system.on_window_resized(layout, wid, Some(drag), to, BORDER, KAI_SCREEN, &kai_s_gaps());
        }

        #[test]
        fn a_border_takes_its_width_out_of_every_tiled_window() {
            let (system, layout, w) = kai_s_strip();
            let slots = render(&system, layout, KAI_SCREEN, &kai_s_gaps());
            let windows = laid_out_with(&system, layout, &constraints_none(), BORDER);
            assert_eq!(windows.len(), slots.len());
            for (wid, slot) in slots {
                assert_eq!(frame_for(&windows, wid), window_in(slot, BORDER), "{wid:?}");
            }
            assert_eq!(bordered(&system, layout, w[1]), CGRect::new(CGPoint::new(604.0, 7.0), CGSize::new(874.0, 1103.0)));
            assert_eq!(scroll_offset(&system, layout), 0.0, "the strip as it was");
        }

        #[test]
        fn a_resize_report_with_a_border_on_reads_the_same_ratio_as_without_a_border() {
            let (mut plain, layout, w) = kai_s_strip();
            let (mut with_border, other, _) = kai_s_strip();
            plain.on_window_resized(layout, w[1], None, xw(600.0, 700.0), 0.0, KAI_SCREEN, &kai_s_gaps());
            with_border.on_window_resized(other, w[1], None, window_xw(604.0, 692.0), BORDER, KAI_SCREEN, &kai_s_gaps());
            assert_eq!(plain.column_width_offset(layout, w[1]), with_border.column_width_offset(other, w[1]));
            let slot = frame_for(&render(&plain, layout, KAI_SCREEN, &kai_s_gaps()), w[1]);
            assert_eq!(bordered(&with_border, other, w[1]), window_in(slot, BORDER));
        }

        /// b1's `a_left_edge_shrink_keeps_the_right_edge_while_the_strip_can_scroll`, the window
        /// 4pt inside each of its slots.
        #[test]
        fn a_left_edge_drag_with_a_border_on_keeps_the_right_edge() {
            let (mut system, layout, w) = kai_s_strip();
            drag_bordered(&mut system, layout, w[1], window_xw(604.0, 874.0), window_xw(338.0, 1140.0));
            assert_eq!(bordered(&system, layout, w[1]), window_xw(338.0, 1140.0));

            drag_bordered(&mut system, layout, w[1], window_xw(338.0, 1140.0), window_xw(438.0, 1040.0));
            assert_eq!(bordered(&system, layout, w[1]), window_xw(438.0, 1040.0), "the right edge kept at 1478");
            assert_near(scroll_offset(&system, layout), 166.0, "offset");
        }

        /// 9338f580's dragged width, whole, with the border: a slot 813pt wide, which as a bounded
        /// column would give up 1.5pt of it to the gap.
        #[test]
        fn a_release_with_a_border_on_leaves_the_window_where_the_cursor_let_go() {
            let (mut system, layout, w) = kai_s_strip();
            drag_bordered(&mut system, layout, w[1], window_xw(604.0, 874.0), window_xw(604.0, 805.0));
            assert_eq!(bordered(&system, layout, w[1]), window_xw(604.0, 805.0));
        }

        #[test]
        fn a_stacked_resize_with_a_border_on_splits_the_height_as_without() {
            let (mut plain, layout, w) = kai_s_stack();
            let (mut with_border, other, _) = kai_s_stack();
            assert!(plain.select_window(layout, w[1]) && with_border.select_window(other, w[1]));
            let from = at_yh(&plain, layout, w[1], 374.0, 368.0);
            let to = at_yh(&plain, layout, w[1], 274.0, 468.0);
            drag(&mut plain, layout, w[1], from, to);
            let reported = |slot: CGRect| window_in(slot, BORDER);
            let drag = Drag::report(reported(from), reported(to), None);
            with_border.on_window_resized(other, w[1], Some(drag), reported(to), BORDER, KAI_SCREEN, &kai_s_gaps());

            let slots = render(&plain, layout, KAI_SCREEN, &kai_s_gaps());
            for window in w {
                assert_eq!(bordered(&with_border, other, window), window_in(frame_for(&slots, window), BORDER), "{window:?}");
            }
        }

        fn limited(wid: WindowId, limits: WindowLayoutConstraints) -> HashMap<WindowId, WindowLayoutConstraints> {
            let mut constraints = constraints_none();
            constraints.insert(wid, limits);
            constraints
        }

        /// Counted without the border, the row's slot would be 700pt and the window 692.
        #[test]
        fn a_stacked_windows_minimum_height_counts_its_border() {
            let (system, layout, w) = kai_s_stack();
            let tall = WindowLayoutConstraints { is_resizable: true, min_height: 700.0, ..Default::default() };
            let frames = laid_out_with(&system, layout, &limited(w[0], tall), BORDER);
            let top = frame_for(&frames, w[0]);
            assert_eq!((top.origin.y, top.size.height), (7.0, 700.0));
            assert_eq!(frame_for(&frames, w[1]).origin.y, 3.0 + 708.0 + 3.0 + BORDER, "below the whole slot and the gap");
        }

        /// A column as wide as its widest minimum, about 808pt with the border, holds a window that
        /// may be at most 500pt wide at its slot's left edge, inside the border.
        #[test]
        fn a_window_held_to_its_maximum_is_clamped_inside_its_slot() {
            let (system, layout, w) = kai_s_stack();
            let mut constraints = limited(w[0], WindowLayoutConstraints { is_resizable: true, max_width: 500.0, ..Default::default() });
            constraints.insert(w[1], WindowLayoutConstraints { is_resizable: true, min_width: 800.0, ..Default::default() });
            let frames = laid_out_with(&system, layout, &constraints, BORDER);
            let slot = tile::slot_of(frame_for(&frames, w[1]), BORDER);
            assert!(slot.size.width > 500.0 + 2.0 * BORDER, "{slot:?}");
            let narrow = frame_for(&frames, w[0]);
            assert_eq!((narrow.origin.x, narrow.size.width), (slot.origin.x + BORDER, 500.0));
        }

        /// A rule can give one window of a stack another border: each is laid inside its own slot by
        /// its own width, and the slots are the strip's as before.
        #[test]
        fn each_window_of_a_stack_is_inset_by_its_own_border() {
            let (system, layout, w) = kai_s_stack();
            let slots = render(&system, layout, KAI_SCREEN, &kai_s_gaps());
            let borders = tile::Borders { every: BORDER, windows: [(w[0], 0.0)].into_iter().collect() };
            let frames = system.calculate_layout(layout, KAI_SCREEN, &constraints_none(), &borders, &kai_s_gaps());
            assert_eq!(frame_for(&frames, w[0]), frame_for(&slots, w[0]), "no border, the whole slot");
            assert_eq!(frame_for(&frames, w[1]), window_in(frame_for(&slots, w[1]), BORDER));
        }
    }
}
