//! A column's width: carried between layouts (`column_width`), full width as a mode
//! (`toggle_full_width`), the preset widths a key cycles through (`cycle_preset_column_width`), and
//! the incremental resize keys (`resize_selection_by`).

use rini_core::ids::WindowId;

use super::ScrollingLayoutSystem;
use crate::layout::domain::column_width::{ColumnWidth, GivenWidth};
use crate::layout::domain::constraints::{ColumnKind, column_ratio};
use crate::layout::domain::preset_width::{PresetWidth, next_preset};
use crate::layout::{LayoutId, ResizeOrientation};

impl ScrollingLayoutSystem {
    /// The width a window carries to another layout: full width when it is maximised, else its
    /// column's own width, `None` while the column takes the default ratio.
    ///
    /// Needed so moving a window between workspaces or displays keeps the width the user gave
    /// it; a fresh column otherwise starts at the default ratio and the window appeared to
    /// reset to 50%.
    pub fn column_width(&self, layout: LayoutId, wid: WindowId) -> Option<ColumnWidth> {
        let state = self.layout_state(layout)?;
        if state.fullscreen_within_gaps.contains(&wid) {
            return Some(ColumnWidth::FullWidth);
        }
        let (col_idx, _) = state.locate(wid)?;
        // Only a deliberately sized column is worth carrying. An untouched one should adopt
        // the destination's default, which may differ (a wider display, a different config).
        state.columns.get(col_idx)?.width.map(GivenWidth::carried)
    }

    /// Re-apply a width carried from another layout: full width as the mode, any other as the
    /// column's own.
    pub fn set_column_width(&mut self, layout: LayoutId, wid: WindowId, width: ColumnWidth) {
        let Some(given) = GivenWidth::from_carried(width) else {
            self.restore_window_full_width(layout, wid);
            return;
        };
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        let Some((col_idx, _)) = state.locate(wid) else {
            return;
        };
        if let Some(column) = state.columns.get_mut(col_idx) {
            column.width = Some(given);
        }
    }

    /// Whether this window's column occupies the whole viewport width.
    ///
    /// Full width is a MODE rather than a ratio: it stays full on a display of any size, so it
    /// cannot be represented as an offset from the configured default and survive a move between
    /// displays.
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

    /// Maximise `wid`, or put it back to the width and stack it had before, and select it.
    ///
    /// The one toggle: `Mod+F` asks it of the selection, a title-bar zoom of the window zoomed.
    /// Under `"never"` both ways are a width change about the column's place, as
    /// `cycle_preset_column_width`'s is: entering takes the place as a width key does, and the way
    /// back lands the column where it stood, unless any other scroll came in between. Leaving takes
    /// no place of its own, so with none kept it reveals. `false` when the window is not a column of
    /// this layout.
    pub fn toggle_full_width(&mut self, layout: LayoutId, wid: WindowId) -> bool {
        let reveals_only = self.settings.reveals_only();
        let Some(state) = self.layout_state_mut(layout) else {
            return false;
        };
        if state.locate(wid).is_none() {
            return false;
        }

        let entering = !state.fullscreen_within_gaps.contains(&wid);
        let place = if !reveals_only {
            None
        } else if entering {
            state.place_of(wid)
        } else {
            state.kept_place_of(wid)
        };
        if entering {
            state.enter_full_width(wid);
        } else {
            state.leave_full_width_restoring(wid);
        }
        state.selected = Some(wid);
        state.rescroll_after_width(wid, place, reveals_only);
        true
    }

    /// Cycle the selected column through `preset_column_widths`.
    ///
    /// niri's switch-preset-column-width. Unlike `resize_selection_by`, which leaves a
    /// column at whatever width it reaches, this snaps to a known set — so every column
    /// ends up at one of a few predictable sizes instead of drifting.
    ///
    /// Only ever a width: the 100% step makes the whole column, stack and all, the viewport wide and
    /// held only to it, as a drag across the viewport does, and enters no full-width mode. A
    /// maximised column takes its turn from 1.0, and the step is what ends the mode, in place. The
    /// rule is [`next_preset`]; what a width does to a maximised column is
    /// `LayoutState::set_column_ratio`. Under `"never"` the column grows and shrinks about where it
    /// stood when focus came to it (`LayoutState::place_of`).
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

        let (ratio, given): (f64, fn(f64) -> GivenWidth) =
            match next_preset(state.ratio_of(selected, col_idx), &presets) {
                Some(PresetWidth::Full) => (1.0, GivenWidth::Unbounded),
                Some(PresetWidth::Ratio(ratio)) => (ratio, GivenWidth::Bounded),
                None => return Vec::new(),
            };
        let place = reveals_only.then(|| state.place_of(selected)).flatten();
        state.set_column_ratio(selected, col_idx, ratio, given);
        state.selected = Some(selected);
        state.rescroll_after_width(selected, place, reveals_only);

        vec![selected]
    }

    /// The incremental resize keys: the selected column's width by `amount` of the viewport, held
    /// to the bounds, or in a stack under `Vertical` or `Smart` its row's height. A width grows and
    /// shrinks about the column's place, as `cycle_preset_column_width`'s does.
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
        let place = reveals_only.then(|| state.place_of(selected)).flatten();
        state.set_column_ratio(selected, col_idx, clamped, GivenWidth::Bounded);
        state.rescroll_after_width(selected, place, reveals_only);
    }
}
