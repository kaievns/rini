use objc2_core_foundation::{CGRect, CGSize};
use rini_core::ids::{WindowId, pid_t};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use serde::{Deserialize, Serialize};

use crate::layout::domain::area::compute_tiling_area;
use crate::layout::domain::column_width::GivenWidth;
use crate::layout::domain::constraints::{
    ColumnKind, column_ratio, holds_viewport, ratio_of_column_width,
};
use crate::layout::domain::edge_drag::{
    Drag, End, Side, dragged_side, dragged_width, trade_height,
};
use crate::layout::domain::strip::{Reveal, centres};
use crate::layout::domain::tile;
use crate::layout::settings::{Gaps, ScrollingLayoutSettings};
use crate::layout::{Direction, LayoutId, StripEnd};

mod pass;
mod width;

/// Where a window sat in the stack it left, so the toggle back can put it there again.
///
/// A neighbour rather than an index: while the window is away its old column can move along the
/// strip, gain windows or lose them, and an index would point somewhere else by then.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
struct StackOrigin {
    /// A window that stayed behind in the column.
    anchor: WindowId,
    /// It sat below the anchor rather than above it.
    below: bool,
}

/// What the next layout holds the selected column to, by a `Pinned` value.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pin {
    /// Its right edge at that x, in display coordinates: a left-edge drag keeps the right one.
    RightEdge,
    /// Its start that far right of the scroll offset: where a fold-out found the column.
    Start,
    /// That share of the viewport's free space on its left, at whatever width it now has: where a
    /// width change by key or the full-width toggle keeps it (`Place::Share`).
    Place,
}

/// Where a column stood when a width change by key or the full-width toggle first took it, kept
/// through the ones after so each grows and shrinks it about the same place and the way back from
/// full width lands it there.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Place {
    /// Centred by a centre override.
    Centred,
    /// This share of the viewport's free space on its left (`strip::place_share`).
    Share(f64),
}

/// How the strip showed a column when a fold-out took its window, for the fold back.
#[derive(Clone, Copy, Debug, PartialEq)]
enum View {
    /// Centred by a centre override.
    Centred,
    /// Its start this far right of the scroll offset.
    StartingAt(f64),
}

/// The view the stack `window` was folded out of had before the fold key for `side` took it, kept
/// until any other scroll.
#[derive(Clone, Copy, Debug, PartialEq)]
struct KeptView {
    window: WindowId,
    side: Direction,
    view: View,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(from = "ColumnRecord", into = "ColumnRecord")]
struct Column {
    windows: Vec<WindowId>,
    /// A width of its own, `None` while it takes the layout's `column_width_ratio`.
    width: Option<GivenWidth>,
    height_weights: Vec<f64>,
    /// Whether `height_weights` are DESIRED PIXEL HEIGHTS from a deliberate vertical resize, rather
    /// than the equal ratios a freshly folded column starts at. The two are read differently and
    /// conflating them is what collapsed a folded window to its title bar.
    height_overridden: bool,
}

/// A `Column` as a layout file holds it: its width an offset, whether it was given one, and whether
/// the bounds hold it, a mark an older rini skips.
#[derive(Serialize, Deserialize)]
#[serde(rename = "Column")]
struct ColumnRecord {
    windows: Vec<WindowId>,
    width_offset: f64,
    #[serde(default)]
    width_overridden: bool,
    #[serde(default)]
    width_dragged: bool,
    #[serde(default)]
    height_weights: Vec<f64>,
    #[serde(default)]
    height_overridden: bool,
}

impl From<ColumnRecord> for Column {
    fn from(record: ColumnRecord) -> Self {
        let width = match (record.width_overridden, record.width_dragged) {
            (false, _) => None,
            (true, false) => Some(GivenWidth::Bounded(record.width_offset)),
            (true, true) => Some(GivenWidth::Unbounded(record.width_offset)),
        };
        Self {
            windows: record.windows,
            width,
            height_weights: record.height_weights,
            height_overridden: record.height_overridden,
        }
    }
}

impl From<Column> for ColumnRecord {
    fn from(column: Column) -> Self {
        Self {
            windows: column.windows,
            width_offset: column.width.map_or(0.0, GivenWidth::offset),
            width_overridden: column.width.is_some(),
            width_dragged: matches!(column.width, Some(GivenWidth::Unbounded(_))),
            height_weights: column.height_weights,
            height_overridden: column.height_overridden,
        }
    }
}

impl Column {
    /// Its offset from the layout's `column_width_ratio`: none while it has no width of its own.
    fn width_offset(&self) -> f64 { self.width.map_or(0.0, GivenWidth::offset) }

    /// Whether its width is held only to the viewport.
    fn is_unbounded(&self) -> bool { matches!(self.width, Some(GivenWidth::Unbounded(_))) }

    fn ensure_height_weights(&mut self) {
        if self.height_weights.len() != self.windows.len() {
            self.height_weights.resize(self.windows.len(), 1.0);
        }
    }

    /// Equal shares, and the deliberate split dropped.
    fn equalise_heights(&mut self) {
        let count = self.windows.len();
        self.height_weights.clear();
        self.height_weights.resize(count, 1.0);
        self.height_overridden = false;
    }

    /// The height its rows share out of `height`, less the gaps between them.
    fn available_height(&self, height: f64, gap: f64) -> f64 {
        let total_gap = gap * (self.windows.len().saturating_sub(1) as f64);
        (height - total_gap).max(0.0)
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

    /// A resized row's new `height` out of the column's `height` less its gaps: a drag by a
    /// stacked end trades it with the row across that end, a drag that moves neither end leaves the
    /// rows, and any other resize takes it from the rest. A lone window has no rows to share.
    fn resize_row(&mut self, row: usize, drag: Option<Drag>, height: f64, column: f64, gap: f64) {
        if self.windows.len() <= 1 || column <= 0.0 {
            return;
        }
        self.ensure_height_weights();
        let available = self.available_height(column, gap);
        if available <= 0.0 {
            return;
        }
        match drag {
            Some(Drag { end: Some(end), .. }) => self.trade_row_height(row, end, height, available),
            Some(Drag { end: None, .. }) => {}
            None => self.take_row_height(row, height, available),
        }
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

/// What the next layout pass does to the scroll offset before it clamps it. The pass spends it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum ScrollIntent {
    /// The offset stands.
    #[default]
    Keep,
    /// The selected column's start at the anchor.
    Align,
    /// The selected column centred by its centre override.
    Center,
    /// The selected column brought into view this way, if it is not in view already.
    Reveal(Reveal),
    /// The selected column held by a pin, while the window it names is still the selection.
    Pin(Pinned),
}

/// A pin for the next pass: `pin` holds `window`'s column to `value`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pinned {
    window: WindowId,
    pin: Pin,
    value: f64,
    /// The reveal pending when the pin was set. It runs after the pin, keeping a pinned right edge,
    /// and stays pending when the pin is dropped.
    reveal: Option<Reveal>,
}

/// What the last layout pass measured, for the requests between passes and the pass after.
#[derive(Clone, Copy, Debug)]
struct Measured {
    /// The tiling area's width.
    screen_width: f64,
    gap_x: f64,
    /// How far the anchor stood right of where it would centre the selected column.
    center_offset_delta: f64,
    /// Where the selected column's start sat against the scroll offset. A change of gap re-spaces
    /// the strip around it, so the column stays put.
    selected_rel: f64,
    /// Where the strip's first column started, against the tiling area's left edge: the anchor less
    /// the scroll offset. See `strip_origin`.
    strip_origin: f64,
    /// The selection `share` and `slot` were measured for, and `selected_rel` read for.
    selected: Option<WindowId>,
    /// That selection's column's share of the viewport's free space on its left
    /// (`strip::place_share`), `None` once a scroll request since has made it stale.
    share: Option<f64>,
    /// The slot size the pass gave that selection, after its own limits: what the app answering
    /// rini's write reports back.
    slot: Option<CGSize>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct LayoutState {
    columns: Vec<Column>,
    selected: Option<WindowId>,
    column_width_ratio: f64,
    #[serde(skip)]
    scroll_offset: f64,
    #[serde(skip)]
    intent: ScrollIntent,
    #[serde(skip)]
    kept_view: Option<KeptView>,
    /// The place the width changes by key and the full-width toggle keep the named window's column
    /// at, until any other scroll.
    #[serde(skip)]
    kept_place: Option<(WindowId, Place)>,
    center_override_window: Option<WindowId>,
    /// `None` before the first pass.
    #[serde(skip)]
    measured: Option<Measured>,
    fullscreen_within_gaps: HashSet<WindowId>,
    /// The stacks full width pulled windows out of, for the toggle back.
    #[serde(default)]
    full_width_origins: HashMap<WindowId, StackOrigin>,
    /// The stacks a fold key folded windows out of, for the fold back in.
    #[serde(default)]
    fold_origins: HashMap<WindowId, StackOrigin>,
}

impl LayoutState {
    fn new(column_width_ratio: f64) -> Self {
        Self {
            column_width_ratio,
            ..Self::default()
        }
    }

    /// The strip scrolled to show the selection: only as far as it must under `reveals_only`, else
    /// aligned to it.
    fn show_selection(&mut self, reveals_only: bool) {
        if reveals_only {
            self.reveal_selected_without_direction();
        } else {
            self.align_scroll_to_selected();
        }
    }

    fn first_window(&self) -> Option<WindowId> {
        self.columns.first().and_then(|c| c.windows.first()).copied()
    }

    /// Take `wid` out of its column into a column of its own, on `side` of the one it left.
    ///
    /// The one way a window leaves a stack: an unfold, an explode and full width all take it.
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
        self.columns.insert(insert_at, Column {
            windows: vec![wid],
            width: None,
            height_weights: vec![weight],
            height_overridden: false,
        });
        Some(origin)
    }

    /// Put `wid` back beside the neighbour it was pulled away from.
    ///
    /// Nothing moves when the anchor is gone: the window stays the column it became, because there
    /// is no stack left to rejoin, and inventing one would put it somewhere the user never had it.
    fn restore_into_stack(&mut self, wid: WindowId, origin: StackOrigin) {
        if self.locate(origin.anchor).is_none() {
            return;
        }
        let Some((col_idx, row_idx)) = self.locate(wid) else {
            return;
        };
        self.columns[col_idx].ensure_height_weights();
        self.columns[col_idx].windows.remove(row_idx);
        let weight = self.columns[col_idx].height_weights.remove(row_idx);
        if self.columns[col_idx].windows.is_empty() {
            self.columns.remove(col_idx);
        }
        // Located again: removing the column above may have shifted the anchor's own index.
        let Some((anchor_col, anchor_row)) = self.locate(origin.anchor) else {
            return;
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
        self.keep_full_width_alone(anchor_col);
    }

    /// The width `wid`'s column is actually at, as a ratio of the viewport.
    ///
    /// 1.0 while maximised, whatever the column's stored offset says. Every rule that asks "how wide
    /// is this now" has to read it this way: the stored offset is the width the column will go BACK
    /// to, and answering with it made `Mod+R` and the resize keys compute their next width from a
    /// size the window had not been for some time.
    fn ratio_of(&self, wid: WindowId, col_idx: usize) -> f64 {
        if self.fullscreen_within_gaps.contains(&wid) {
            return 1.0;
        }
        self.column_width_ratio + self.columns[col_idx].width_offset()
    }

    /// Whether this column holds a maximised window, and so occupies the whole viewport.
    fn is_full_width_column(&self, col: &Column) -> bool {
        col.windows.iter().any(|wid| self.fullscreen_within_gaps.contains(wid))
    }

    fn column_kind(&self, col: &Column) -> ColumnKind {
        if self.is_full_width_column(col) {
            ColumnKind::FullWidth
        } else if col.is_unbounded() {
            ColumnKind::Unbounded
        } else {
            ColumnKind::Bounded
        }
    }

    /// Maximise `wid`, pulling it out of any stack it shares.
    ///
    /// A maximised window fills the tiling area, which would cover the siblings sharing its column
    /// while the tree still claimed they were abreast. It becomes its own column in the stack's
    /// place, the stack one column right, so the strip does not scroll for a stack that was on
    /// screen. Where it came from is kept so the toggle can put it back.
    fn enter_full_width(&mut self, wid: WindowId) {
        if let Some(origin) = self.split_out(wid, Direction::Left) {
            self.full_width_origins.insert(wid, origin);
        }
        self.fullscreen_within_gaps.insert(wid);
    }

    /// Stop being maximised and go back into the stack `enter_full_width` pulled the window out of.
    ///
    /// A window that was alone in its column has no origin and simply stops being maximised. So does
    /// one whose whole former column has since closed.
    fn leave_full_width_restoring(&mut self, wid: WindowId) {
        self.fullscreen_within_gaps.remove(&wid);
        if let Some(origin) = self.full_width_origins.remove(&wid) {
            self.restore_into_stack(wid, origin);
        }
    }

    /// Stop being maximised because the window was given a width of its own, and stay put.
    ///
    /// The window becomes an ordinary column with nowhere to return to, so the origin is FORGOTTEN
    /// rather than kept for later: the maximise that recorded it is over. Leaving it recorded meant a
    /// later `Mod+F` — which records no new origin, the window being alone in its column by then —
    /// toggled off into a stack the user had not touched since.
    fn leave_full_width_in_place(&mut self, wid: WindowId) {
        self.fullscreen_within_gaps.remove(&wid);
        self.full_width_origins.remove(&wid);
        self.unpin();
    }

    /// A full-width window is alone in its column: one that comes to share it stops being full
    /// width where it is, as if given a width.
    fn keep_full_width_alone(&mut self, col_idx: usize) {
        let Some(column) = self.columns.get(col_idx).filter(|column| column.windows.len() > 1)
        else {
            return;
        };
        let maximized: Vec<WindowId> = column
            .windows
            .iter()
            .copied()
            .filter(|wid| self.fullscreen_within_gaps.contains(wid))
            .collect();
        for wid in maximized {
            self.leave_full_width_in_place(wid);
        }
    }

    /// Give `wid`'s column an explicit ratio, held as a column of `kind` is, leaving full width if
    /// it was maximised.
    fn set_column_ratio(&mut self, wid: WindowId, col_idx: usize, kind: ColumnKind, ratio: f64) {
        self.leave_full_width_in_place(wid);
        self.columns[col_idx].width = Some(GivenWidth::new(kind, ratio - self.column_width_ratio));
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
            self.intent = ScrollIntent::Center;
            return;
        }
        self.center_override_window = None;
        let Some((_col_idx, _)) = self.selected_location() else {
            self.scroll_offset = 0.0;
            if self.intent != ScrollIntent::Align {
                self.intent = ScrollIntent::Keep;
            }
            return;
        };
        self.intent = ScrollIntent::Align;
    }

    fn request_center_on_selected(&mut self) {
        self.unpin();
        if self.selected_location().is_none() {
            return;
        }
        if self.center_override_window.is_some() && self.center_override_window == self.selected {
            // Toggle off when already centered on the same selection.
            self.center_override_window = None;
            self.intent = ScrollIntent::Align;
        } else {
            self.center_override_window = self.selected;
            self.intent = ScrollIntent::Center;
        }
    }

    fn reveal_selected_in_direction(&mut self, direction: Direction) {
        self.unpin();
        self.center_override_window = None;
        self.intent = match direction {
            Direction::Left => ScrollIntent::Reveal(Reveal::FromRight),
            Direction::Right => ScrollIntent::Reveal(Reveal::FromLeft),
            _ => ScrollIntent::Keep,
        };
    }

    /// Keep the current offset unless the selected column would be clipped.
    fn reveal_selected_without_direction(&mut self) {
        self.unpin();
        self.center_override_window = None;
        self.intent = ScrollIntent::Reveal(Reveal::Either);
    }

    fn pin(&mut self, window: WindowId, pin: Pin, value: f64) {
        let reveal = match self.intent {
            ScrollIntent::Reveal(reveal) => Some(reveal),
            _ => None,
        };
        self.intent = ScrollIntent::Pin(Pinned { window, pin, value, reveal });
    }

    /// Any scroll replaces what is held for a later pass: the pin, the view a fold back takes, and
    /// the place a width change or the full-width toggle keeps or would take from the last pass.
    fn unpin(&mut self) {
        if let ScrollIntent::Pin(pinned) = self.intent {
            self.intent = pinned.reveal.map_or(ScrollIntent::Keep, ScrollIntent::Reveal);
        }
        self.kept_view = None;
        self.kept_place = None;
        if let Some(measured) = &mut self.measured {
            measured.share = None;
        }
    }

    /// Whether a scroll request for `wid` would drop something held for it: its centring, a pin no
    /// pass has spent yet, the view a fold back returns to, or the place a width change or the
    /// full-width toggle keeps.
    fn holds_scroll_for(&self, wid: WindowId) -> bool {
        self.center_override_window == Some(wid)
            || matches!(self.intent, ScrollIntent::Pin(pinned) if pinned.window == wid)
            || self.kept_view.is_some_and(|kept| kept.window == wid)
            || self.kept_place.is_some_and(|(of, _)| of == wid)
    }

    /// What the last pass measured, if `wid` was its selection.
    fn measured_for(&self, wid: WindowId) -> Option<Measured> {
        self.measured.filter(|measured| measured.selected == Some(wid))
    }

    /// Where a width change keeps `wid`'s column: the place kept for it, or else where the last pass
    /// showed it, if `wid` was that pass's selection.
    fn place_of(&self, wid: WindowId) -> Option<Place> {
        if let Some(place) = self.kept_place_of(wid) {
            return Some(place);
        }
        let measured = self.measured_for(wid)?;
        if self.center_override_window == Some(wid) {
            return Some(Place::Centred);
        }
        measured.share.filter(|share| share.is_finite()).map(Place::Share)
    }

    /// The place kept for `wid`'s column, if any.
    fn kept_place_of(&self, wid: WindowId) -> Option<Place> {
        self.kept_place.filter(|&(of, _)| of == wid).map(|(_, place)| place)
    }

    /// Rescroll after a width change: back to `place`, or, with none, as far as it takes to
    /// keep the column visible, every column start after it having moved.
    fn rescroll_after_width(&mut self, wid: WindowId, place: Option<Place>, reveals_only: bool) {
        match place {
            Some(place) => self.keep_place(wid, place),
            None if reveals_only => self.reveal_selected_without_direction(),
            None => self.align_scroll_to_selected(),
        }
    }

    /// Hold the selected `wid`'s column at `place`, for the next pass and the width changes after
    /// it.
    fn keep_place(&mut self, wid: WindowId, place: Place) {
        let pin = match place {
            Place::Centred => None,
            Place::Share(share) => Some((Pin::Place, share)),
        };
        self.hold_selected(wid, pin);
        self.kept_place = Some((wid, place));
    }

    /// Hold the selected `wid`'s column for the next pass: centred by its override, or by `pin`.
    fn hold_selected(&mut self, wid: WindowId, pin: Option<(Pin, f64)>) {
        self.unpin();
        self.intent = match pin {
            None => {
                self.center_override_window = Some(wid);
                ScrollIntent::Center
            }
            Some((pin, value)) => {
                self.center_override_window = None;
                ScrollIntent::Pin(Pinned {
                    window: wid,
                    pin,
                    value,
                    reveal: None,
                })
            }
        };
    }

    /// How the last pass showed `wid`'s column, if `wid` was its selection.
    fn laid_out_view_of(&self, wid: WindowId) -> Option<View> {
        let measured = self.measured_for(wid)?;
        if self.center_override_window == Some(wid) {
            return Some(View::Centred);
        }
        let rel = measured.selected_rel;
        rel.is_finite().then_some(View::StartingAt(rel))
    }

    /// The slot size the last pass gave `wid`, if `wid` was its selection.
    fn laid_out_slot_of(&self, wid: WindowId) -> Option<CGSize> {
        let slot = self.measured_for(wid)?.slot?;
        (slot.width.is_finite() && slot.height.is_finite()).then_some(slot)
    }

    /// The view the fold back by the key for `side` returns `wid` to, if it kept one. Any other
    /// kept view goes too.
    fn take_kept_view(&mut self, wid: WindowId, side: Direction) -> Option<View> {
        let kept = self.kept_view.take()?;
        (kept.window == wid && kept.side == side).then_some(kept.view)
    }

    /// Show the selected `wid`'s column the way `view` says the strip showed it.
    fn return_to_view(&mut self, wid: WindowId, view: View) {
        let pin = match view {
            View::Centred => None,
            View::StartingAt(rel) => Some((Pin::Start, rel)),
        };
        self.hold_selected(wid, pin);
    }

    fn clamp_scroll_offset(&mut self) {
        if self.columns.is_empty() {
            self.scroll_offset = 0.0;
            return;
        }
        // Keep the user's current strip position; final bounds clamping happens in
        // `calculate_layout` where full column geometry is available.
        if self.intent == ScrollIntent::Align {
            self.intent = ScrollIntent::Keep;
        }
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
        // An origin anchored on a window that has closed cannot be restored into, and keeping it
        // would send the toggle back hunting for a window that is gone.
        for origins in [&mut self.full_width_origins, &mut self.fold_origins] {
            origins.remove(&wid);
            origins.retain(|_, origin| origin.anchor != wid);
        }

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
            if self.selected.is_none()
                && col_idx > 0
                && let Some(new_sel) = self.columns[col_idx - 1].windows.last().copied()
            {
                self.selected = Some(new_sel);
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
            width: None,
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
            width: None,
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
            self.columns[col_idx].height_weights.remove(row_idx);
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
                    width: None,
                    height_weights: vec![1.0],
                    height_overridden: false,
                });
            } else {
                self.columns[target].ensure_height_weights();
                self.columns[target].windows.push(window);
                // The weight from the column it left held that whole column's share, so
                // carried over it would take nearly all of this one: stacking divides evenly.
                self.columns[target].equalise_heights();
                self.keep_full_width_alone(target);
            }
            self.selected = Some(window);
            self.align_scroll_to_selected();
        }
    }
}

/// The scroll offset that keeps the selected column where it was on screen after the gap between
/// columns changed. The offset is in strip points and every column start includes the gaps before
/// it, so a wider gap slides the selected column right by one gap change per column ahead of it
/// unless the offset follows. `None` when the gap is unchanged or nothing was laid out before.
fn rebased_offset(
    previous_gap: f64,
    gap: f64,
    previous_rel: f64,
    selected_start: f64,
) -> Option<f64> {
    (gap != previous_gap && previous_rel.is_finite()).then_some(selected_start - previous_rel)
}

/// A window's report of its new frame, as `ScrollingLayoutSystem::on_window_resized` reads it: the
/// `border` it wears, and the mouse `drag` that resized it, `None` for any other resize.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResizeReport {
    pub window: WindowId,
    pub drag: Option<Drag>,
    pub frame: CGRect,
    pub border: f64,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct ScrollingLayoutSystem {
    layouts: slotmap::SlotMap<LayoutId, LayoutState>,
    #[serde(skip, default = "default_scrolling_settings")]
    settings: ScrollingLayoutSettings,
}

fn default_scrolling_settings() -> ScrollingLayoutSettings { ScrollingLayoutSettings::default() }

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
        // faithfully re-applied against a new base. Left as it is, moving the ratio from
        // 0.49 to 0.5 would leave every restored window at 842pt instead of 859pt — a
        // width matching no preset, so Mod+R could never return a window to it.
        //
        // Columns the user deliberately resized (a `width` of their own) keep their chosen
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
                    column.width = column
                        .width
                        .map(|width| width.shifted(old_ratio - settings.column_width_ratio));
                }
            }
        }
    }

    fn insert_new_column(state: &mut LayoutState, wid: WindowId) {
        if let Some((col_idx, _)) = state.selected_location() {
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

    /// What holds `col` and the ratio of the viewport it asks for, as the layout pass lays it out.
    fn laid_out_ratio(
        state: &LayoutState,
        col: &Column,
        min_ratio: f64,
        max_ratio: f64,
    ) -> (ColumnKind, f64) {
        let kind = state.column_kind(col);
        let base_ratio = Self::clamp_base_ratio(state.column_width_ratio, min_ratio, max_ratio);
        (
            kind,
            column_ratio(kind, base_ratio, col.width_offset(), min_ratio, max_ratio),
        )
    }

    fn column_starts(
        state: &LayoutState,
        screen_width: f64,
        gap_x: f64,
        min_ratio: f64,
        max_ratio: f64,
    ) -> Vec<f64> {
        let mut starts = Vec::with_capacity(state.columns.len());
        let mut cursor = 0.0;
        for col in &state.columns {
            starts.push(cursor);
            let (_, ratio) = Self::laid_out_ratio(state, col, min_ratio, max_ratio);
            cursor += (screen_width * ratio).max(1.0) + gap_x;
        }
        starts
    }

    pub fn snap_to_nearest_column(&mut self, layout: LayoutId) {
        let min_ratio = self.settings.min_column_width_ratio;
        let max_ratio = self.settings.max_column_width_ratio;
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        let Some(measured) = state.measured else {
            return;
        };
        let (screen_width, gap_x) = (measured.screen_width, measured.gap_x);
        if screen_width <= 0.0 {
            return;
        }
        let starts = Self::column_starts(state, screen_width, gap_x, min_ratio, max_ratio);
        if starts.is_empty() {
            return;
        }
        let base_max_offset = starts.last().copied().unwrap_or(0.0);
        let center_offset_delta = measured.center_offset_delta;
        let (min_offset, max_offset, baseline) = if state.center_override_window.is_some() {
            (
                center_offset_delta,
                base_max_offset + center_offset_delta,
                center_offset_delta,
            )
        } else {
            (0.0, base_max_offset, 0.0)
        };
        let current = state.scroll_offset;
        let strip_offset = current - baseline;
        let target = starts
            .iter()
            .min_by(|a, b| (*a - strip_offset).abs().total_cmp(&(*b - strip_offset).abs()))
            .copied()
            .unwrap_or(0.0);
        let next = (baseline + target).clamp(min_offset, max_offset);
        state.unpin();
        state.scroll_offset = next;
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
            .and_then(|state| state.measured)
            .map(|measured| measured.strip_origin)
            .filter(|origin| !origin.is_nan())
    }

    fn layout_state(&self, layout: LayoutId) -> Option<&LayoutState> { self.layouts.get(layout) }

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
            let insert_at = match dir {
                Direction::Left => col_idx,
                Direction::Right => (col_idx + 1).min(state.columns.len()),
                _ => return false,
            };
            Self::pull_out_of_stack(state, (col_idx, row_idx), insert_at);
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

    /// Take the selected window to `end` of the strip: a lone window's column moves there, and a
    /// stacked window leaves its column as a new column there. False with no selection, or with a
    /// lone window's column already at that end.
    fn move_selected_window_to_end(state: &mut LayoutState, end: StripEnd) -> bool {
        let Some((col_idx, row_idx)) = state.selected_location() else {
            return false;
        };
        if state.columns[col_idx].windows.len() > 1 {
            let insert_at = match end {
                StripEnd::First => 0,
                StripEnd::Last => state.columns.len(),
            };
            Self::pull_out_of_stack(state, (col_idx, row_idx), insert_at);
            return true;
        }
        let target = match end {
            StripEnd::First => 0,
            StripEnd::Last => state.columns.len() - 1,
        };
        if col_idx == target {
            return false;
        }
        let column = state.columns.remove(col_idx);
        state.columns.insert(target, column);
        true
    }

    /// The window at `at`, a column and a row of a stacked column, taken out into a column of its
    /// own inserted at `insert_at`, and selected.
    fn pull_out_of_stack(state: &mut LayoutState, at: (usize, usize), insert_at: usize) {
        let (col_idx, row_idx) = at;
        state.columns[col_idx].ensure_height_weights();
        let wid = state.columns[col_idx].windows.remove(row_idx);
        let weight = state.columns[col_idx].height_weights.remove(row_idx);
        state.columns.insert(insert_at, Column {
            windows: vec![wid],
            width: None,
            height_weights: vec![weight],
            height_overridden: false,
        });
        state.selected = Some(wid);
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

    /// The strip scrolled to `offset` without a focus move, dropping what a later pass would have
    /// taken, as every scroll does.
    fn scroll_strip_to(&mut self, layout: LayoutId, offset: f64) {
        let state = self.layout_state_mut(layout).expect("layout state missing");
        state.unpin();
        state.scroll_offset = offset;
    }

    fn scroll_offset(&self, layout: LayoutId) -> f64 {
        self.layout_state(layout).expect("layout state missing").scroll_offset
    }
}

impl ScrollingLayoutSystem {
    pub fn create_layout(&mut self) -> LayoutId {
        self.layouts.insert(LayoutState::new(self.settings.column_width_ratio))
    }

    pub fn contains_layout(&self, layout: LayoutId) -> bool { self.layouts.contains_key(layout) }

    pub fn clone_layout(&mut self, layout: LayoutId) -> LayoutId {
        let cloned = self
            .layouts
            .get(layout)
            .cloned()
            .unwrap_or_else(|| LayoutState::new(self.settings.column_width_ratio));
        self.layouts.insert(cloned)
    }

    pub fn remove_layout(&mut self, layout: LayoutId) { self.layouts.remove(layout); }

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
                    weight: Some((state.column_width_ratio + column.width_offset()).max(0.0)),
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
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        Self::insert_new_column(state, wid);
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
            if let ScrollIntent::Pin(pinned) = &mut state.intent
                && pinned.window == from
            {
                pinned.window = to;
            }
            if let Some((of, _)) = &mut state.kept_place
                && *of == from
            {
                *of = to;
            }
            if state.fullscreen_within_gaps.remove(&from) {
                state.fullscreen_within_gaps.insert(to);
            }
            for origins in [&mut state.full_width_origins, &mut state.fold_origins] {
                if let Some(origin) = origins.remove(&from) {
                    origins.insert(to, origin);
                }
                for origin in origins.values_mut() {
                    if origin.anchor == from {
                        origin.anchor = to;
                    }
                }
            }
        }
    }

    pub fn remove_window(&mut self, wid: WindowId) {
        for state in self.layouts.values_mut() {
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
                    Self::insert_new_column(state, **des);
                    desired_iter.next();
                }
                (Some(des), Some(cur)) if des < cur => {
                    Self::insert_new_column(state, **des);
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
            // edge a left-edge drag keeps, which the release refocuses before its pass, the view a
            // fold back returns to, and the place a width change or the full-width toggle keeps,
            // which the raise after each refocuses.
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
    /// for any other resize: the app's own, or a window rule's size, and then an axis within a point
    /// of the slot the last pass gave the window is the app answering rini's write, which changes
    /// nothing. See "Mouse resize" in `src/layout/docs/strip.md`.
    pub fn on_window_resized(
        &mut self,
        layout: LayoutId,
        report: ResizeReport,
        screen: CGRect,
        gaps: &Gaps,
    ) {
        let ResizeReport {
            window: wid,
            drag,
            frame: new_frame,
            border,
        } = report;
        let drag = drag.map(|drag| Drag {
            from: tile::slot_of(drag.from, border),
            ..drag
        });
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
        let side_drag =
            dragged_from.and_then(|start| dragged_side(start, new_frame).map(|side| (start, side)));
        let full_width = state.is_full_width_column(&state.columns[col_idx]);
        if full_width && side_drag.is_none() {
            // A full-width column keeps the width it had before, for the toggle back: neither the
            // app settling its own size nor a drag that moves neither side gives it a width.
            return;
        }
        let (kind, ratio) =
            Self::laid_out_ratio(state, &state.columns[col_idx], min_ratio, max_ratio);
        let kept = tile::LaidOut {
            slot: state.laid_out_slot_of(wid).filter(|_| drag.is_none()),
            held_to_viewport: holds_viewport(kind, ratio, tiling.size.width),
            stacked: state.columns[col_idx].windows.len() > 1,
        }
        .kept_axes(new_frame.size);
        if kept.both() {
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
        let kind = if side_drag.is_some() || state.columns[col_idx].is_unbounded() {
            ColumnKind::Unbounded
        } else {
            ColumnKind::Bounded
        };
        let read_back = ratio_of_column_width(kind, width, tiling.size.width, gaps.inner);
        let ratio = column_ratio(kind, read_back, 0.0, min_ratio, max_ratio);
        if let Some((start, _)) = side_drag
            && ratio >= 1.0
            && state.columns[col_idx].windows.len() == 1
        {
            if !full_width {
                // The width before the drag, for the toggle back.
                let offset = start.size.width / tiling.size.width - base_ratio;
                state.columns[col_idx].width = Some(GivenWidth::Unbounded(offset));
                state.enter_full_width(wid);
            }
        } else if !kept.width {
            state.set_column_ratio(wid, col_idx, kind, ratio);
        }

        if !kept.height {
            let height = new_frame.size.height;
            state.columns[col_idx].resize_row(
                row_idx,
                drag,
                height,
                tiling.size.height,
                gaps.inner,
            );
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
            state.keep_full_width_alone(a_col);
            state.keep_full_width_alone(b_col);
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
            state.show_selection(reveals_only);
        }
        moved
    }

    /// The selected window taken to `end` of the strip as `move_selection` takes it one place: a
    /// lone window's column, or a stacked window as a new column, the strip scrolled to show it.
    /// False when nothing moved: no selection, or a lone window already at that end.
    pub fn move_selection_to_end(&mut self, layout: LayoutId, end: StripEnd) -> bool {
        let reveals_only = self.settings.reveals_only();
        let Some(state) = self.layout_state_mut(layout) else {
            return false;
        };
        let moved = Self::move_selected_window_to_end(state, end);
        if moved {
            state.show_selection(reveals_only);
        }
        moved
    }

    /// How many columns lie between the selected window's column and `end` of the strip: none with
    /// no selection.
    pub fn columns_toward(&self, layout: LayoutId, end: StripEnd) -> usize {
        let Some(state) = self.layout_state(layout) else {
            return 0;
        };
        let Some((col_idx, _)) = state.selected_location() else {
            return 0;
        };
        match end {
            StripEnd::First => col_idx,
            StripEnd::Last => state.columns.len() - 1 - col_idx,
        }
    }

    /// Fold the selection into the column on `side`, or back out of the column it is in.
    ///
    /// `side` names the column this key works with, so one binding per side gives symmetric control.
    /// There is no falling back to the other side: with both keys bound that would make them agree
    /// at the ends of the strip.
    ///
    /// Pressing the same key twice puts the window back, which takes two different mechanisms.
    /// Folding OUT lands the new column on the side AWAY from `side`, because that is where the
    /// window was before it folded into that column. Folding IN returns it to the row it was folded
    /// out of (`fold_origins`) when that row is in the neighbour on `side`, and appends it to the
    /// neighbour otherwise, so the rows keep their order too. Under `"never"` the fold back by the
    /// same key also puts the strip back, unless any other scroll came in between. A full-width
    /// window folds in as an ordinary column, its full width ended where it is.
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
        let view_before = (folding_out && reveals_only)
            .then(|| state.laid_out_view_of(selected))
            .flatten();

        if folding_out {
            if let Some(origin) = state.split_out(selected, away) {
                state.fold_origins.insert(selected, origin);
            }
        } else {
            let target = match side {
                Direction::Left => col_idx.checked_sub(1),
                _ => (col_idx + 1 < state.columns.len()).then_some(col_idx + 1),
            };
            let Some(target) = target else {
                return Vec::new();
            };
            let origin = state.fold_origins.remove(&selected).filter(|origin| {
                state.locate(origin.anchor).is_some_and(|(anchor_col, _)| anchor_col == target)
            });
            match origin {
                Some(origin) => state.restore_into_stack(selected, origin),
                None => state.move_window_to_column_end(selected, target),
            }
        }

        let back_to = state.take_kept_view(selected, side).filter(|_| !folding_out);
        state.selected = Some(selected);
        match back_to {
            Some(view) => state.return_to_view(selected, view),
            None if reveals_only => state.reveal_selected_without_direction(),
            None => state.align_scroll_to_selected(),
        }
        if let Some(view) = view_before {
            state.kept_view = Some(KeptView { window: selected, side, view });
        }
        vec![selected]
    }

    /// Give every window in the selected window's column a column of its own, in their row order.
    ///
    /// Each leaves by `split_out`, as an unfold does: the windows above to the left of the column,
    /// those below to its right, so the column stays the selected window's, with its width, and the
    /// others start at the default width. The selection stays on the selected window and is revealed
    /// as a fold reveals it. A lone window changes nothing. Returns the selected window, the one to
    /// raise, so focus stays where it was.
    pub fn explode_column_of_selection(&mut self, layout: LayoutId) -> Vec<WindowId> {
        let reveals_only = self.settings.reveals_only();
        let Some(state) = self.layout_state_mut(layout) else {
            return Vec::new();
        };
        let Some((col_idx, row_idx)) = state.selected_location() else {
            return Vec::new();
        };
        let mut others = state.columns[col_idx].windows.clone();
        if others.len() <= 1 {
            return Vec::new();
        }
        let selected = others.remove(row_idx);
        for &wid in &others[..row_idx] {
            state.split_out(wid, Direction::Left);
        }
        for &wid in others[row_idx..].iter().rev() {
            state.split_out(wid, Direction::Right);
        }
        state.selected = Some(selected);
        state.show_selection(reveals_only);
        vec![selected]
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
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use rini_core::ids::{WindowId, pid_t};
    use rustc_hash::FxHashMap as HashMap;

    use super::{Column, Pin, Place, ResizeReport, ScrollingLayoutSystem, View, rebased_offset};
    use crate::layout::domain::area::compute_tiling_area;
    use crate::layout::domain::column_width::{ColumnWidth, GivenWidth};
    use crate::layout::domain::edge_drag::{Drag, End};
    use crate::layout::domain::tile;
    use crate::layout::settings::{CenterFocusedColumn, Gaps, ScrollingLayoutSettings};
    use crate::layout::{
        Direction, LayoutId, ResizeOrientation, StripEnd, WindowLayoutConstraints,
    };

    /// `window`'s app resizing it to `frame`, with no border and no drag.
    fn resized(window: WindowId, frame: CGRect) -> ResizeReport {
        ResizeReport {
            window,
            drag: None,
            frame,
            border: 0.0,
        }
    }

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
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        screen: CGRect,
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let constraints = HashMap::default();
        system.calculate_layout(layout, screen, &constraints, &tile::Borders::default(), gaps)
    }

    fn constraints_none() -> HashMap<WindowId, WindowLayoutConstraints> { HashMap::default() }

    fn frame_for(frames: &[(WindowId, CGRect)], wid: WindowId) -> CGRect {
        frames
            .iter()
            .find(|(id, _)| *id == wid)
            .map(|(_, frame)| *frame)
            .expect("missing frame")
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

    /// What `Mod+F` asks of the layout: the toggle, on the selection.
    fn toggle_selection(system: &mut ScrollingLayoutSystem, layout: LayoutId) {
        let selected = system.selected_window(layout).expect("a selection to toggle");
        assert!(system.toggle_full_width(layout, selected));
    }

    #[test]
    fn a_preview_gives_the_frames_of_the_next_pass_and_spends_nothing() {
        let (mut system, layout, _, _) = setup_two_windows(ScrollingLayoutSettings::default());
        let (next_pass, untouched) = (system.clone_layout(layout), system.clone_layout(layout));
        let gaps = Gaps::default();
        let borders = tile::Borders::default();
        let preview = system.preview_layout(
            layout,
            screen(1000.0, 800.0),
            &constraints_none(),
            &borders,
            &gaps,
        );
        assert_eq!(system.strip_origin(layout), None, "nothing measured");
        assert_eq!(
            render(&mut system, next_pass, screen(1000.0, 800.0), &gaps),
            preview
        );

        let wider = screen(1400.0, 800.0);
        assert_eq!(
            render(&mut system, layout, wider, &gaps),
            render(&mut system, untouched, wider, &gaps),
            "the pending reveal still taken at the next pass's width"
        );
    }

    /// Two columns 700 wide on a 1000 wide screen, the strip at its start and the second column,
    /// 300 of it off the right edge, selected with the reveal the selection asks for pending.
    fn second_column_off_the_right() -> (ScrollingLayoutSystem, LayoutId, WindowId) {
        let (mut system, layout, w1, w2) = setup_two_windows(ScrollingLayoutSettings::default());
        render(&mut system, layout, screen(1000.0, 800.0), &Gaps::default());
        system.select_window(layout, w1);
        let frames = render(&mut system, layout, screen(1000.0, 800.0), &Gaps::default());
        assert_eq!(frame_for(&frames, w2).origin.x, 700.0);
        system.select_window(layout, w2);
        (system, layout, w2)
    }

    fn second_column_x(system: &mut ScrollingLayoutSystem, layout: LayoutId, w2: WindowId) -> f64 {
        let frames = render(system, layout, screen(1000.0, 800.0), &Gaps::default());
        frame_for(&frames, w2).origin.x
    }

    const REVEALED: f64 = 300.0;
    const LEFT_AS_IT_WAS: f64 = 700.0;

    #[test]
    fn a_pass_spends_the_pin_and_the_reveal_under_it() {
        let (mut system, layout, w2) = second_column_off_the_right();
        system.layout_state_mut(layout).expect("layout").pin(w2, Pin::RightEdge, 900.0);
        let frames = render(&mut system, layout, screen(1000.0, 800.0), &Gaps::default());
        assert_eq!(right_edge(&frames, w2), 900.0);
        let origin = system.strip_origin(layout);
        assert!(!system.layout_state(layout).expect("layout").holds_scroll_for(w2));

        let frames = render(&mut system, layout, screen(1400.0, 800.0), &Gaps::default());
        assert_eq!(
            system.strip_origin(layout),
            origin,
            "the strip left where the pin put it"
        );
        assert!(right_edge(&frames, w2) > 1400.0, "and no reveal taken");
    }

    #[test]
    fn a_pin_set_over_a_pending_reveal_leaves_the_reveal_pending_once_dropped() {
        let (mut system, layout, w2) = second_column_off_the_right();
        let state = system.layout_state_mut(layout).expect("layout");
        state.pin(w2, Pin::Start, 40.0);
        assert!(state.holds_scroll_for(w2));
        let pinned = system.clone_layout(layout);
        assert_eq!(second_column_x(&mut system, pinned, w2), 40.0);

        let state = system.layout_state_mut(layout).expect("layout");
        state.unpin();
        assert!(!state.holds_scroll_for(w2));
        assert_eq!(second_column_x(&mut system, layout, w2), REVEALED);
    }

    #[test]
    fn a_held_selection_s_pin_leaves_nothing_pending_once_dropped() {
        let (mut system, layout, w2) = second_column_off_the_right();
        let state = system.layout_state_mut(layout).expect("layout");
        state.return_to_view(w2, View::StartingAt(40.0));
        let held = system.clone_layout(layout);
        assert_eq!(second_column_x(&mut system, held, w2), 40.0);

        system.layout_state_mut(layout).expect("layout").unpin();
        assert_eq!(second_column_x(&mut system, layout, w2), LEFT_AS_IT_WAS);
    }

    #[test]
    fn a_renamed_window_keeps_its_pin() {
        let (mut system, layout, w2) = second_column_off_the_right();
        system.layout_state_mut(layout).expect("layout").pin(w2, Pin::Start, 40.0);
        let renamed = wid(1, 9);
        system.replace_window(w2, renamed);
        assert!(system.layout_state(layout).expect("layout").holds_scroll_for(renamed));
        assert_eq!(second_column_x(&mut system, layout, renamed), 40.0);
    }

    #[test]
    fn a_clamp_drops_only_a_pending_align() {
        type Request = fn(&mut super::LayoutState);
        let cases: [(&str, Request, f64); 3] = [
            ("align", |state| state.align_scroll_to_selected(), LEFT_AS_IT_WAS),
            (
                "reveal",
                |state| state.reveal_selected_in_direction(Direction::Left),
                REVEALED,
            ),
            ("centre", |state| state.request_center_on_selected(), 150.0),
        ];
        for (case, request, x) in cases {
            let (mut system, layout, w2) = second_column_off_the_right();
            let state = system.layout_state_mut(layout).expect("layout");
            request(state);
            state.clamp_scroll_offset();
            assert_eq!(second_column_x(&mut system, layout, w2), x, "{case}");
        }
    }

    #[test]
    fn a_sideways_reveal_brings_the_column_in_and_a_vertical_one_leaves_the_offset() {
        for (direction, x) in [
            (Direction::Left, REVEALED),
            (Direction::Right, REVEALED),
            (Direction::Up, LEFT_AS_IT_WAS),
            (Direction::Down, LEFT_AS_IT_WAS),
        ] {
            let (mut system, layout, w2) = second_column_off_the_right();
            let state = system.layout_state_mut(layout).expect("layout");
            state.reveal_selected_in_direction(direction);
            assert_eq!(second_column_x(&mut system, layout, w2), x, "{direction:?}");
        }
    }

    #[test]
    fn an_align_with_no_selection_puts_the_strip_at_its_start() {
        let (mut system, layout, w2) = second_column_off_the_right();
        assert_eq!(second_column_x(&mut system, layout, w2), REVEALED);
        system.clear_selection_for_test(layout);
        system.layout_state_mut(layout).expect("layout").align_scroll_to_selected();
        render(&mut system, layout, screen(1000.0, 800.0), &Gaps::default());
        assert_eq!(system.strip_origin(layout), Some(0.0));
    }

    #[test]
    fn a_scroll_request_makes_the_measured_share_stale_and_keeps_the_rest() {
        let (mut system, layout, _, w2) = setup_two_windows(ScrollingLayoutSettings::default());
        render(&mut system, layout, screen(1000.0, 800.0), &Gaps::default());
        let state = system.layout_state_mut(layout).expect("layout");
        assert!(matches!(state.place_of(w2), Some(Place::Share(_))));
        assert!(state.laid_out_slot_of(w2).is_some());
        state.unpin();
        assert_eq!(state.place_of(w2), None);
        assert!(state.laid_out_slot_of(w2).is_some());
        assert!(state.laid_out_view_of(w2).is_some());
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
            width: None,
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
            width: None,
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
        let frames =
            system.calculate_layout(layout, screen, &constraints, &tile::Borders::default(), &gaps);
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
        let (mut system, layout, _, _) = setup_two_windows(ScrollingLayoutSettings::default());
        let frames = render(&mut system, layout, screen(1000.0, 800.0), &Gaps::default());

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
        let frames = render(&mut system, layout, screen, &gaps);

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
        let settings = ScrollingLayoutSettings {
            column_width_ratio: 1.0 / 3.0,
            ..ScrollingLayoutSettings::default()
        };
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
        let centred = render(&mut system, layout, screen, &gaps);
        let (offset, origin) = (system.scroll_offset(layout), system.strip_origin(layout));
        let tiling = compute_tiling_area(screen, &gaps);
        let first = frame_for(&centred, windows[0]).origin.x - tiling.origin.x;
        assert!(
            (origin.unwrap() - first).abs() < 1.0,
            "{origin:?} against {first}"
        );

        assert!(system.move_selection(layout, Direction::Right));
        let moved = render(&mut system, layout, screen, &gaps);
        assert!(
            (system.scroll_offset(layout) - offset).abs() < 1e-9,
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
        let frames = render(&mut system, layout, screen, &gaps);
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
        let settings = ScrollingLayoutSettings {
            column_width_ratio: 0.45,
            min_column_width_ratio: 0.2,
            max_column_width_ratio: 0.9,
            ..ScrollingLayoutSettings::default()
        };
        let (mut system, layout, w1, w2) = setup_two_windows(settings);

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();

        // Apply the default initial alignment (selected = w2) so w1 starts off-screen.
        let _ = render(&mut system, layout, screen, &gaps);

        let _ = system.move_focus(layout, Direction::Left);
        let left_frames = render(&mut system, layout, screen, &gaps);
        let offset_after_left = system.scroll_offset(layout);

        let _ = system.move_focus(layout, Direction::Right);
        let right_frames = render(&mut system, layout, screen, &gaps);
        let offset_after_right = system.scroll_offset(layout);

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
        let _ = render(&mut system, layout, screen, &gaps);

        let _ = system.move_focus(layout, Direction::Left);
        let _ = render(&mut system, layout, screen, &gaps);
        let offset_after_left = system.scroll_offset(layout);

        let _ = system.move_focus(layout, Direction::Right);
        let _ = render(&mut system, layout, screen, &gaps);
        let offset_after_right = system.scroll_offset(layout);

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
        let frames = render(&mut system, layout, screen(1000.0, 800.0), &gaps);

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
        let settings = ScrollingLayoutSettings {
            column_width_ratio: 0.45,
            min_column_width_ratio: 0.2,
            max_column_width_ratio: 0.9,
            ..ScrollingLayoutSettings::default()
        };
        let (mut system, layout, w1, _) = setup_two_windows(settings);

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let _ = render(&mut system, layout, screen, &gaps);

        assert!(system.select_window(layout, w1));
        let frames = render(&mut system, layout, screen, &gaps);
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
        let settings = ScrollingLayoutSettings {
            column_width_ratio: 0.42,
            min_column_width_ratio: 0.2,
            max_column_width_ratio: 0.9,
            ..ScrollingLayoutSettings::default()
        };
        let (mut system, layout, w1, _) = setup_two_windows(settings);

        // Make focused-left column wider so selected widths differ across focus moves.
        let _ = system.move_focus(layout, Direction::Left);
        system.resize_selection_by(layout, 0.15, ResizeOrientation::Horizontal);

        let screen = screen(1200.0, 800.0);
        let gaps = Gaps::default();

        let frames_left = render(&mut system, layout, screen, &gaps);
        let w1_x_left = frame_for(&frames_left, w1).origin.x;

        let _ = system.move_focus(layout, Direction::Right);
        let frames_right = render(&mut system, layout, screen, &gaps);
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

    fn order(system: &ScrollingLayoutSystem, layout: LayoutId) -> Vec<Vec<WindowId>> {
        let state = system.layouts.get(layout).expect("layout state missing");
        state.columns.iter().map(|column| column.windows.clone()).collect()
    }

    /// Four lone columns, the third selected: to the start it passes two, to the end the other
    /// three, the columns between keep their order, and the selection stays on it. Already at an
    /// end, the move there changes nothing.
    #[test]
    fn a_lone_window_moves_to_either_end_of_the_strip_in_one_press() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let [w1, w2, w3, w4] = [1, 2, 3, 4].map(|idx| wid(1, idx));
        for window in [w1, w2, w3, w4] {
            system.add_window_after_selection(layout, window);
        }
        system.select_window(layout, w3);
        assert_eq!(system.columns_toward(layout, StripEnd::First), 2);
        assert_eq!(system.columns_toward(layout, StripEnd::Last), 1);

        assert!(system.move_selection_to_end(layout, StripEnd::First));
        assert_eq!(order(&system, layout), [[w3], [w1], [w2], [w4]]);
        assert_eq!(system.selected_window(layout), Some(w3));
        assert_eq!(system.columns_toward(layout, StripEnd::First), 0);
        assert!(
            !system.move_selection_to_end(layout, StripEnd::First),
            "already first"
        );
        assert_eq!(order(&system, layout), [[w3], [w1], [w2], [w4]]);

        assert_eq!(system.columns_toward(layout, StripEnd::Last), 3);
        assert!(system.move_selection_to_end(layout, StripEnd::Last));
        assert_eq!(order(&system, layout), [[w1], [w2], [w4], [w3]]);
        assert_eq!(system.selected_window(layout), Some(w3));
        assert!(
            !system.move_selection_to_end(layout, StripEnd::Last),
            "already last"
        );
    }

    /// A stacked window leaves its column as a new column at that end, the first column's own
    /// included, as a move of one place pulls it out at an edge.
    #[test]
    fn a_stacked_window_leaves_as_a_new_column_at_either_end() {
        for (end, expected) in [(StripEnd::First, [1, 0, 2]), (StripEnd::Last, [0, 2, 1])] {
            let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
            let layout = system.create_layout();
            let windows = [1, 2, 3].map(|idx| wid(1, idx));
            for window in windows {
                system.add_window_after_selection(layout, window);
            }
            system.select_window(layout, windows[1]);
            system.toggle_fold_of_selection(layout, Direction::Left);
            assert_eq!(order(&system, layout), [vec![windows[0], windows[1]], vec![
                windows[2]
            ]]);

            assert!(system.move_selection_to_end(layout, end), "{end:?}");
            let columns: Vec<Vec<WindowId>> =
                expected.iter().map(|&index| vec![windows[index]]).collect();
            assert_eq!(order(&system, layout), columns, "{end:?}");
            assert_eq!(system.selected_window(layout), Some(windows[1]), "{end:?}");
        }
    }

    /// The strip scrolls to show the window taken to its far end, as after a move of one place.
    #[test]
    fn a_move_to_an_end_scrolls_the_strip_to_show_the_window() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let windows = [1, 2, 3, 4, 5].map(|idx| wid(1, idx));
        for window in windows {
            system.add_window_after_selection(layout, window);
        }
        system.select_window(layout, windows[0]);
        let display = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let before = frame_for(&render(&mut system, layout, display, &gaps), windows[0]);
        assert!(before.max().x <= 1000.0, "{before:?}");

        assert!(system.move_selection_to_end(layout, StripEnd::Last));
        let after = frame_for(&render(&mut system, layout, display, &gaps), windows[0]);
        assert!(after.origin.x >= 0.0 && after.max().x <= 1000.0, "{after:?}");
    }

    #[test]
    fn niri_rightmost_widening_increases_visible_width() {
        let settings = ScrollingLayoutSettings {
            column_width_ratio: 0.45,
            min_column_width_ratio: 0.2,
            max_column_width_ratio: 0.95,
            ..ScrollingLayoutSettings::default()
        };
        let (mut system, layout, _, w2) = setup_two_windows(settings); // selected rightmost

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();

        let before = render(&mut system, layout, screen, &gaps);
        let before_frame = frame_for(&before, w2);

        system.resize_selection_by(layout, 0.08, ResizeOrientation::Horizontal);

        let after = render(&mut system, layout, screen, &gaps);
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
        let before = frame_for(&render(&mut system, layout, screen, &gaps), w2);

        assert!(system.select_window(layout, w2));
        assert!(system.select_window(layout, w2));

        let after = frame_for(&render(&mut system, layout, screen, &gaps), w2);

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
        let frames_before = render(&mut system, layout, screen, &gaps);
        let f1_before = frame_for(&frames_before, w1);
        let f2_before = frame_for(&frames_before, w2);
        assert!((f1_before.size.height - f2_before.size.height).abs() < 1.0);

        // 2. Select w1 and resize it
        assert!(system.select_window(layout, w1));
        let mut new_f1 = f1_before;
        new_f1.size.height = f1_before.size.height + 100.0;

        let drag = Drag::report(f1_before, new_f1, None);
        system.on_window_resized(
            layout,
            ResizeReport {
                drag: Some(drag),
                ..resized(w1, new_f1)
            },
            screen,
            &gaps,
        );

        // 3. Re-calculate layout and verify resized heights are preserved
        let frames_after = render(&mut system, layout, screen, &gaps);
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
        let before = frame_for(&render(&mut system, layout, screen, &gaps), w1);

        system.resize_selection_by(layout, 0.05, ResizeOrientation::Vertical);

        let after = frame_for(&render(&mut system, layout, screen, &gaps), w1);
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
        let before = frame_for(&render(&mut system, layout, screen, &gaps), w1);
        system.resize_selection_by(layout, 0.05, ResizeOrientation::Smart);
        let after = frame_for(&render(&mut system, layout, screen, &gaps), w1);

        assert!(after.size.height > before.size.height);
        assert!((after.size.width - before.size.width).abs() < 1.0);
    }

    #[test]
    fn niri_new_window_reveal_does_not_unnecessarily_push_offscreen() {
        let settings = ScrollingLayoutSettings {
            column_width_ratio: 0.4,
            ..ScrollingLayoutSettings::default()
        };
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);

        // Add first window
        system.add_window_after_selection(layout, w1);
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let _ = render(&mut system, layout, screen, &gaps);
        assert_eq!(system.scroll_offset(layout), 0.0);

        // Add second window
        system.add_window_after_selection(layout, w2);
        let frames = render(&mut system, layout, screen, &gaps);
        let offset = system.scroll_offset(layout);

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
            let w1_frame = frame_for(&render(&mut system, layout, screen, &gaps), w1);
            assert!((w1_frame.origin.x - 0.0).abs() < 1.0); // left-aligned

            assert!(system.select_window(layout, w2));
            let w2_frame = frame_for(&render(&mut system, layout, screen, &gaps), w2);
            assert!((w2_frame.origin.x - 300.0).abs() < 1.0); // centered

            assert!(system.select_window(layout, w3));
            let w3_frame = frame_for(&render(&mut system, layout, screen, &gaps), w3);
            assert!((w3_frame.origin.x - 600.0).abs() < 1.0); // right-aligned
        }
    }

    /// A column's width must not depend on how many OTHER columns share its workspace.
    ///
    /// A lone column expanding to the whole viewport would make a window's size a function of its
    /// neighbours: a full-size window sent from a populated workspace to an empty one would change
    /// size with nothing done to it.
    ///
    /// niri behaves as asserted here: one column keeps its preset width and leaves the rest
    /// of the strip empty. Full width is `maximize-column`
    /// (`toggle_fullscreen_within_gaps`), which is explicit and remembered per display.
    #[test]
    fn a_lone_column_keeps_its_configured_width() {
        let settings = ScrollingLayoutSettings {
            column_width_ratio: 0.4,
            ..ScrollingLayoutSettings::default()
        };
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w1 = wid(1, 1);

        system.add_window_after_selection(layout, w1);

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let frames1 = render(&mut system, layout, screen, &gaps);
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

        let frames2 = render(&mut system, layout, screen, &gaps);
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
        let settings = ScrollingLayoutSettings {
            column_width_ratio: 0.4,
            ..ScrollingLayoutSettings::default()
        };
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        system.add_window_after_selection(layout, w1);

        system.restore_window_full_width(layout, w1);
        assert!(system.is_window_full_width(layout, w1));

        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let frame = frame_for(&render(&mut system, layout, screen, &gaps), w1);
        assert!(
            (frame.size.width - 1000.0).abs() < 1.0,
            "full-width mode must fill the tiling width, got {frame:?}"
        );

        // And it round-trips back to the preset, so the mode is not a one-way door.
        assert!(system.select_window(layout, w1));
        toggle_selection(&mut system, layout);
        let frame = frame_for(&render(&mut system, layout, screen, &gaps), w1);
        assert!((frame.size.width - 400.0).abs() < 1.0, "got {frame:?}");
    }

    #[test]
    fn app_reconciliation_inserts_next_to_the_selection() {
        let mut system = ScrollingLayoutSystem::new(&ScrollingLayoutSettings::default());
        let layout = system.create_layout();
        let w1 = wid(1, 1);
        let w2 = wid(1, 2);
        let w3 = wid(1, 3);
        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);
        assert!(system.select_window(layout, w1));

        system.set_windows_for_app(layout, 1, vec![w1, w2, w3]);

        assert_eq!(system.all_windows_in_layout(layout), vec![w1, w3, w2]);
    }

    // ── full-width columns participate in the strip ─────────────────────────

    fn niri_settings(ratio: f64) -> ScrollingLayoutSettings {
        ScrollingLayoutSettings {
            column_width_ratio: ratio,
            min_column_width_ratio: 0.2,
            max_column_width_ratio: 1.0,
            ..ScrollingLayoutSettings::default()
        }
    }

    /// A full-width column must keep its strip-relative x, not be pinned to the
    /// viewport origin. Framed as `tiling`, the window would leave the strip and
    /// stay put while everything scrolls past.
    #[test]
    fn fullscreen_within_gaps_column_scrolls_with_the_strip() {
        let (mut system, layout, w1, w2) = setup_two_windows(niri_settings(0.5));
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();

        assert!(system.select_window(layout, w1));
        toggle_selection(&mut system, layout);

        let x_before = frame_for(&render(&mut system, layout, screen, &gaps), w1).origin.x;

        // Scroll the strip by focusing the other column.
        assert!(system.select_window(layout, w2));
        let x_after = frame_for(&render(&mut system, layout, screen, &gaps), w1).origin.x;

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

        let frames = render(&mut system, layout, screen, &gaps);
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
        let width_before = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;

        toggle_selection(&mut system, layout);
        let width_full = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;

        toggle_selection(&mut system, layout);
        let width_after = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;

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
    /// column goes back to: an app that puts its zoom frame back after rini's write, or a terminal
    /// snapping to its cell grid, must not leave the toggle back at the clamped maximum ratio
    /// instead of the width it had.
    #[test]
    fn a_resize_report_under_full_width_keeps_the_width_to_return_to() {
        let (mut system, layout, w1, _) = setup_two_windows(niri_settings(0.5));
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        assert!(system.select_window(layout, w1));
        let width_before = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;

        toggle_selection(&mut system, layout);
        let full = frame_for(&render(&mut system, layout, screen, &gaps), w1);
        let settled = CGRect::new(
            full.origin,
            CGSize::new(full.size.width - 6.0, full.size.height - 4.0),
        );
        system.on_window_resized(layout, resized(w1, settled), screen, &gaps);
        assert_eq!(
            frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width,
            full.size.width,
            "still full width"
        );

        toggle_selection(&mut system, layout);
        let width_after = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;
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
        let _ = render(&mut system, layout, screen, &gaps);

        toggle_selection(&mut system, layout);
        let f3 = frame_for(&render(&mut system, layout, screen, &gaps), w3);

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

    /// Mod+R cycles 1/3 -> 1/2 -> 2/3 and wraps back to 1/3.
    #[test]
    fn cycle_preset_column_width_walks_presets_and_wraps() {
        let mut settings = niri_settings(0.33333);
        settings.preset_column_widths = vec![0.33333, 0.5, 0.66667];
        let (mut system, layout, w1, _) = setup_two_windows(settings);
        let screen = screen(1200.0, 800.0);
        let gaps = Gaps::default();
        let tiling_width = compute_tiling_area(screen, &gaps).size.width;

        assert!(system.select_window(layout, w1));

        let width_of = |system: &mut ScrollingLayoutSystem| {
            frame_for(&render(system, layout, screen, &gaps), w1).size.width / tiling_width
        };

        // Starts at the first preset.
        assert!(
            (width_of(&mut system) - 0.33333).abs() < 0.02,
            "start {}",
            width_of(&mut system)
        );

        system.cycle_preset_column_width(layout);
        assert!(
            (width_of(&mut system) - 0.5).abs() < 0.02,
            "after 1st {}",
            width_of(&mut system)
        );

        system.cycle_preset_column_width(layout);
        assert!(
            (width_of(&mut system) - 0.66667).abs() < 0.02,
            "after 2nd {}",
            width_of(&mut system)
        );

        // Wraps rather than sticking at the widest.
        system.cycle_preset_column_width(layout);
        assert!(
            (width_of(&mut system) - 0.33333).abs() < 0.02,
            "after wrap {}",
            width_of(&mut system)
        );
    }

    /// The bounds the shipped config uses: a maximum well below 1.0, so that the resize keys cannot crawl
    /// to the full viewport, which only the cycle's 1.0, `Mod+F` and a drag across it give.
    fn preset_settings() -> ScrollingLayoutSettings {
        let mut settings = niri_settings(0.5);
        settings.min_column_width_ratio = 0.33333;
        settings.max_column_width_ratio = 0.66667;
        settings.preset_column_widths = vec![0.33333, 0.5, 0.66667, 1.0];
        settings
    }

    /// The reported defect. `Mod+F` maximised the window and `Mod+R` then appeared dead: it wrote a
    /// width the layout pass ignored, because a maximised column is 1.0 whatever its stored width
    /// says. The step has to UNMAXIMISE, which is the only way out of full width besides `Mod+F`.
    #[test]
    fn a_step_off_full_width_actually_resizes_the_window() {
        let (mut system, layout, w1, _) = setup_two_windows(preset_settings());
        let screen = screen(1200.0, 800.0);
        let gaps = Gaps::default();
        let tiling_width = compute_tiling_area(screen, &gaps).size.width;
        let width_of = |system: &mut ScrollingLayoutSystem| {
            frame_for(&render(system, layout, screen, &gaps), w1).size.width / tiling_width
        };

        assert!(system.select_window(layout, w1));
        toggle_selection(&mut system, layout);
        assert!(
            (width_of(&mut system) - 1.0).abs() < 0.02,
            "maximised {}",
            width_of(&mut system)
        );

        system.cycle_preset_column_width(layout);

        assert!(
            !system.is_window_full_width(layout, w1),
            "a window given a width is no longer maximised"
        );
        assert!(
            (width_of(&mut system) - 0.33333).abs() < 0.02,
            "nothing is wider than the viewport, so the cycle starts over at the narrowest; got {}",
            width_of(&mut system)
        );
    }

    /// The full viewport is reachable from the cycle even though it is above
    /// `max_column_width_ratio`, because the cycle gives it held only to the viewport. Asking for it
    /// as a bounded ratio would be clamped to two thirds and the widest preset would be unreachable.
    #[test]
    fn the_cycle_reaches_the_full_viewport_past_the_configured_maximum() {
        let (mut system, layout, w1, _) = setup_two_windows(preset_settings());
        let screen = screen(1200.0, 800.0);
        let gaps = Gaps::default();
        let tiling_width = compute_tiling_area(screen, &gaps).size.width;

        assert!(system.select_window(layout, w1));
        system.cycle_preset_column_width(layout); // 0.5 -> 0.66667
        system.cycle_preset_column_width(layout); // 0.66667 -> 1.0

        assert!(!system.is_window_full_width(layout, w1), "a width, not the mode");
        let width = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;
        assert!(
            (width / tiling_width - 1.0).abs() < 0.02,
            "the widest preset must fill the viewport, got {}",
            width / tiling_width
        );
    }

    /// Growing a maximised column writes no width, since nothing would read it. It cannot grow — there is
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
            frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width / tiling_width;
        assert!(
            (ratio - 0.66667).abs() < 0.02,
            "one step down from the viewport, clamped to the configured maximum; got {ratio}"
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

        let positions = |system: &mut ScrollingLayoutSystem| {
            let frames = render(system, layout, screen, &gaps);
            (
                frame_for(&frames, w1).origin.x.round(),
                frame_for(&frames, w2).origin.x.round(),
            )
        };

        assert!(system.select_window(layout, w1));
        // Render once so the strip settles before measuring.
        let _ = positions(&mut system);
        let first = positions(&mut system);

        // Alternate focus with move_focus, which is what Mod+J / Mod+L invoke and
        // what triggers reveal-on-demand. select_window does NOT take that path, so
        // it cannot reproduce the shift.
        for i in 0..3 {
            let _ = system.move_focus(layout, Direction::Right);
            assert_eq!(
                positions(&mut system),
                first,
                "strip shifted moving right (iter {i})"
            );
            let _ = system.move_focus(layout, Direction::Left);
            assert_eq!(
                positions(&mut system),
                first,
                "strip shifted moving left (iter {i})"
            );
        }

        // And both must genuinely be on screen, not merely stable.
        let frames = render(&mut system, layout, screen, &gaps);
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
        let before = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;

        let touched = system.cycle_preset_column_width(layout);
        let after = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;

        assert!(touched.is_empty(), "expected no windows reported changed");
        assert!(
            (before - after).abs() < 1.0,
            "width changed from {} to {}",
            before,
            after
        );
    }

    // ── exploding a column ──────────────────────────────────────────────────

    /// Whichever row is selected, the rows become the columns in their order, and the selection
    /// stays on the window it was on, the one window that comes back to be raised.
    #[test]
    fn exploding_a_column_of_three_gives_three_columns_in_row_order() {
        for row in 0..3 {
            let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);
            assert!(system.select_window(layout, w[row]));

            let raised = system.explode_column_of_selection(layout);

            assert_eq!(
                shape(&mut system, layout),
                vec![vec![w[0]], vec![w[1]], vec![w[2]]],
                "row {row}"
            );
            assert_eq!(raised, vec![w[row]], "row {row}");
            assert_eq!(system.selected_window(layout), Some(w[row]), "row {row}");
        }
    }

    /// The selected window keeps the column and the width it was given; the others start at the
    /// default width, as a window folded out does, and the selected one is on screen.
    #[test]
    fn exploding_keeps_the_column_s_width_for_the_selected_window_alone() {
        let (mut fresh, fresh_layout) = a_strip(&[&[1], &[2]]);
        let default = laid_out_xw(&mut fresh, fresh_layout, wid(1, 1)).1;
        let (mut system, layout) = a_strip(&[&[1, 2, 3]]);
        assert!(system.select_window(layout, wid(1, 2)));
        system.cycle_preset_column_width(layout);
        let wide = laid_out_xw(&mut system, layout, wid(1, 2)).1;
        assert!((wide - default).abs() > 1.0, "fixture: {wide} against {default}");

        system.explode_column_of_selection(layout);

        let (x, width) = laid_out_xw(&mut system, layout, wid(1, 2));
        assert_eq!(width, wide);
        assert!(
            x >= 0.0 && x + width <= KAI_SCREEN.size.width,
            "the selected window is on screen, at {x} and {width} wide"
        );
        for other in [1, 3] {
            assert_eq!(
                laid_out_xw(&mut system, layout, wid(1, other)).1,
                default,
                "{other}"
            );
        }
    }

    /// Between neighbours the column comes apart where it stood: the rows take its place in their
    /// order, the neighbours keep their places in the strip and their widths, and the selected
    /// window keeps the column's width.
    #[test]
    fn exploding_a_column_between_neighbours_leaves_them_as_they_were() {
        let (mut system, layout) = a_strip(&[&[1], &[2, 3, 4], &[5]]);
        for wide in [1, 5, 3] {
            assert!(system.select_window(layout, wid(1, wide)));
            system.cycle_preset_column_width(layout);
        }
        let width =
            |system: &mut ScrollingLayoutSystem, idx| laid_out_xw(system, layout, wid(1, idx)).1;
        let before = [1, 3, 5].map(|idx| width(&mut system, idx));

        system.explode_column_of_selection(layout);

        assert_eq!(shape(&mut system, layout), ids(&[&[1], &[2], &[3], &[4], &[5]]));
        assert_eq!([1, 3, 5].map(|idx| width(&mut system, idx)), before);
        assert_ne!(
            width(&mut system, 2),
            before[0],
            "fixture: the neighbours are not at the default"
        );
        assert_eq!(system.selected_window(layout), Some(wid(1, 3)));
    }

    /// A window alone in its column has nothing to explode: the strip and its frames stay as they
    /// were.
    #[test]
    fn exploding_a_lone_window_changes_nothing() {
        let (mut system, layout) = a_strip(&[&[1], &[2]]);
        assert!(system.select_window(layout, wid(1, 1)));
        let before = kai_s_frames(&mut system, layout);

        assert!(system.explode_column_of_selection(layout).is_empty());

        assert_eq!(shape(&mut system, layout), ids(&[&[1], &[2]]));
        assert_eq!(system.selected_window(layout), Some(wid(1, 1)));
        assert_same_frames(&kai_s_frames(&mut system, layout), &before);
    }

    /// Only a changed gap re-bases the offset, and only once a layout has recorded where the selected
    /// column sat.
    #[test]
    fn only_a_changed_gap_rebases_the_offset() {
        assert_eq!(rebased_offset(2.0, 4.0, -4.0, 1726.0), Some(1730.0));
        assert_eq!(rebased_offset(4.0, 4.0, -4.0, 1726.0), None, "the same gap");
        assert_eq!(
            rebased_offset(2.0, 4.0, f64::NAN, 1726.0),
            None,
            "never laid out"
        );
    }

    /// Raising the gap between windows from 2 to 4 must not leave a full-width column two columns
    /// along the strip at left 6 / right 2 instead of 4 / 4. The scroll offset is in strip
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

        let before = frame_for(&render(&mut system, layout, screen, &gaps), w3);
        assert_eq!(
            before.origin.x, 4.0,
            "the full-width column starts at the left margin"
        );

        gaps.inner = 4.0;
        let frames = render(&mut system, layout, screen, &gaps);
        let after = frame_for(&frames, w3);
        assert_eq!(after.origin.x, 4.0, "it stays at the left margin");
        assert_eq!(
            after.origin.x + after.size.width,
            1724.0,
            "and ends at the right one"
        );
        // The columns ahead of it are re-spaced with the new gap, one gap to its left.
        assert_eq!(
            frame_for(&frames, w2).origin.x + frame_for(&frames, w2).size.width,
            0.0
        );
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

        let width_of = |system: &mut ScrollingLayoutSystem| {
            frame_for(&render(system, layout, screen, &gaps), w1).size.width.round()
        };

        let before = width_of(&mut system);

        let mut wider = settings.clone();
        wider.column_width_ratio = 0.5;
        system.update_settings(&wider);

        let after = width_of(&mut system);
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
        let chosen = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;

        let mut wider = settings.clone();
        wider.column_width_ratio = 0.5;
        system.update_settings(&wider);

        let after = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;
        assert!(
            (after - chosen).abs() <= 2.0,
            "an explicitly resized column must keep its width: {chosen} -> {after}"
        );
    }

    /// Folding a lone window into another column must divide the height evenly, not let the
    /// newcomer take almost everything: as the sole window it held its own column's entire share,
    /// and carried into a column of 1.0-weighted windows that share collapses the window already
    /// there to its title bar.
    #[test]
    fn folding_into_a_column_equalises_heights() {
        let settings = niri_settings(0.5);
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let (w1, w2) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, w1);
        system.add_window_after_selection(layout, w2);

        let screen = screen(1728.0, 1117.0);
        let gaps = Gaps::default();

        assert!(system.select_window(layout, w2));
        system.toggle_fold_of_selection(layout, Direction::Left);

        let frames = render(&mut system, layout, screen, &gaps);
        let h1 = frame_for(&frames, w1).size.height;
        let h2 = frame_for(&frames, w2).size.height;

        assert!(
            (h1 - h2).abs() <= 2.0,
            "folded windows should share the height evenly, got {h1} and {h2}"
        );
        assert!(
            h1 > 100.0,
            "the pre-existing window must not collapse to a title bar, got height {h1}"
        );
    }
    /// The shape of the strip: one inner vec per column, windows top to bottom.
    fn shape(system: &mut ScrollingLayoutSystem, layout: LayoutId) -> Vec<Vec<WindowId>> {
        system
            .layout_state(layout)
            .expect("layout")
            .columns
            .iter()
            .map(|column| column.windows.clone())
            .collect()
    }

    /// One column of `rows` windows, each folded into the first.
    fn column_of(
        settings: ScrollingLayoutSettings,
        rows: usize,
    ) -> (ScrollingLayoutSystem, LayoutId, Vec<WindowId>) {
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let w: Vec<WindowId> = (1..=rows as u32).map(|idx| wid(1, idx)).collect();
        for &id in &w {
            system.add_window_after_selection(layout, id);
        }
        for &id in &w[1..] {
            assert!(system.select_window(layout, id));
            system.toggle_fold_of_selection(layout, Direction::Left);
        }
        assert_eq!(
            shape(&mut system, layout),
            vec![w.clone()],
            "fixture is one column"
        );
        (system, layout, w)
    }

    /// A maximized window left in its stack would cover the siblings it shares the column with: the
    /// tree would say three windows abreast and the screen show one.
    #[test]
    fn maximizing_a_stacked_window_pulls_it_out_of_the_stack() {
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);

        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);

        assert_eq!(
            shape(&mut system, layout),
            vec![vec![w[1]], vec![w[0], w[2]]],
            "the maximized window takes the stack's place and the stack closes up beside it"
        );
        assert!(system.is_window_full_width(layout, w[1]));
    }

    #[test]
    fn a_second_press_puts_it_back_between_the_windows_it_left() {
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);

        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);
        toggle_selection(&mut system, layout);

        assert_eq!(
            shape(&mut system, layout),
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
        let width_before = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;

        assert!(system.toggle_full_width(layout, w1));
        assert!(system.is_window_full_width(layout, w1));
        assert!(!system.is_window_full_width(layout, w2));
        assert_eq!(system.selected_window(layout), Some(w1));
        let width_full = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;
        assert_eq!(width_full, compute_tiling_area(screen, &gaps).size.width);

        assert!(system.toggle_full_width(layout, w1));
        let width_after = frame_for(&render(&mut system, layout, screen, &gaps), w1).size.width;
        assert!(!system.is_window_full_width(layout, w1));
        assert_eq!(width_after, width_before, "back to the width it had");
    }

    #[test]
    fn full_width_of_an_unselected_stacked_window_puts_it_back_in_its_stack() {
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);
        assert!(system.select_window(layout, w[0]));

        assert!(system.toggle_full_width(layout, w[1]));
        assert_eq!(shape(&mut system, layout), vec![vec![w[1]], vec![w[0], w[2]]]);

        assert!(system.toggle_full_width(layout, w[1]));
        assert_eq!(shape(&mut system, layout), vec![vec![w[0], w[1], w[2]]]);
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
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);

        assert!(system.select_window(layout, w[0]));
        toggle_selection(&mut system, layout);
        assert_eq!(shape(&mut system, layout), vec![vec![w[0]], vec![w[1], w[2]]]);

        toggle_selection(&mut system, layout);
        assert_eq!(
            shape(&mut system, layout),
            vec![vec![w[0], w[1], w[2]]],
            "back on top, not below"
        );
    }

    /// The "if available" half. Every window it could go back beside has closed, so it stays the
    /// column it became rather than being put somewhere the user never had it.
    #[test]
    fn a_window_whose_stack_is_gone_stays_its_own_column() {
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);

        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);
        system.remove_window(w[0]);
        system.remove_window(w[2]);

        toggle_selection(&mut system, layout);
        assert_eq!(shape(&mut system, layout), vec![vec![w[1]]]);
        assert!(
            !system.is_window_full_width(layout, w[1]),
            "it still stops being maximized"
        );
    }

    // The anchor closing must not leave an origin pointing at it: restoring into a window that is
    // gone is how a stale record turns into a window in the wrong place.
    #[test]
    fn closing_the_anchor_alone_leaves_the_rest_of_the_stack_reachable() {
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);

        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);
        // w0 was the anchor, being directly above w1.
        system.remove_window(w[0]);

        toggle_selection(&mut system, layout);
        assert_eq!(
            shape(&mut system, layout),
            vec![vec![w[1]], vec![w[2]]],
            "no stack to rejoin"
        );
    }

    /// A window given a width has nowhere to return to.
    ///
    /// `Mod+R` off full width leaves the window where it is, so the stack it was pulled out of stops
    /// being its destination and is forgotten. Keeping the record made the NEXT `Mod+F` pair a
    /// teleport: maximising records no new origin (the window is alone in its column by then), so the
    /// press that unmaximised it read the stale one and dropped the window into a stack the user had
    /// not touched since.
    #[test]
    fn a_window_given_a_width_does_not_rejoin_its_old_stack_later() {
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);

        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);
        system.cycle_preset_column_width(layout);
        assert_eq!(shape(&mut system, layout), vec![vec![w[1]], vec![w[0], w[2]]]);

        // A fresh maximise and a fresh press to leave it. Nothing here asked for the old stack.
        toggle_selection(&mut system, layout);
        toggle_selection(&mut system, layout);

        assert_eq!(
            shape(&mut system, layout),
            vec![vec![w[1]], vec![w[0], w[2]]],
            "it stays the ordinary column Mod+R made it"
        );
    }

    /// The other half of the same rule: a window that has NOT been given a width still round-trips.
    /// This is what stops the record being dropped too eagerly.
    #[test]
    fn a_maximised_window_still_rejoins_its_stack_on_the_second_press() {
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);

        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);
        toggle_selection(&mut system, layout);

        assert_eq!(shape(&mut system, layout), vec![vec![w[0], w[1], w[2]]]);
    }

    /// Restoring a remembered full width at startup goes through the same transition, so a window
    /// whose column is shared is pulled out of it rather than left maximised over its siblings.
    #[test]
    fn restoring_a_remembered_full_width_pulls_the_window_out_of_its_stack() {
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);

        system.restore_window_full_width(layout, w[1]);

        assert_eq!(shape(&mut system, layout), vec![vec![w[1]], vec![w[0], w[2]]]);
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
        assert_eq!(shape(&mut system, layout), vec![vec![a], vec![b]]);
        toggle_selection(&mut system, layout);
        assert_eq!(shape(&mut system, layout), vec![vec![a], vec![b]]);
    }

    /// The reservation and the frame have to agree. The strip reserved the CLAMPED width for the
    /// column while the window was handed the whole tiling width, so the next column was laid out
    /// on top of it. The column keeps the viewport and the window sits at its maximum inside it.
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
        constraints.insert(narrow, WindowLayoutConstraints {
            is_resizable: true,
            max_width: 400.0,
            ..Default::default()
        });
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let frames =
            system.calculate_layout(layout, screen, &constraints, &tile::Borders::default(), &gaps);

        let maximized = frame_for(&frames, narrow);
        let neighbour = frame_for(&frames, next);
        assert_eq!(maximized.size.width, 400.0, "it cannot be wider than it says");
        assert!(
            neighbour.origin.x >= 1000.0,
            "the column keeps the viewport, the next at {neighbour:?}"
        );
        assert!(
            maximized.origin.x + maximized.size.width <= neighbour.origin.x + 1.0,
            "maximized {maximized:?} runs into the next column at {neighbour:?}"
        );
    }
    /// Folding two windows together divides the height evenly, and it has to do that whether or not
    /// macOS happened to report a minimum height for either of them.
    ///
    /// The reported minimum is never subtracted from the weight. Weights are 1.0 in a freshly
    /// folded column and minima are pixels, so a window with a minimum would get 0.001 and one
    /// without 1.0: the first would be left at its minimum, which on screen is its title bar.
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
        constraints.insert(top, WindowLayoutConstraints {
            is_resizable: true,
            min_height: 100.0,
            ..Default::default()
        });
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let frames =
            system.calculate_layout(layout, screen, &constraints, &tile::Borders::default(), &gaps);

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
        let frames = system.calculate_layout(
            layout,
            screen,
            &constraints_none(),
            &tile::Borders::default(),
            &gaps,
        );
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
        let frames = system.calculate_layout(
            layout,
            screen,
            &constraints_none(),
            &tile::Borders::default(),
            &gaps,
        );
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
        let frames = system.calculate_layout(
            layout,
            screen,
            &constraints_none(),
            &tile::Borders::default(),
            &gaps,
        );
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
        let settings = ScrollingLayoutSettings {
            column_width_ratio: 0.5,
            ..ScrollingLayoutSettings::default()
        };
        let mut system = ScrollingLayoutSystem::new(&settings);
        let layout = system.create_layout();
        let (acme, neighbour) = (wid(1, 1), wid(1, 2));
        system.add_window_after_selection(layout, acme);
        system.add_window_after_selection(layout, neighbour);

        let mut constraints = HashMap::default();
        constraints.insert(acme, WindowLayoutConstraints {
            is_resizable: true,
            min_width: 600.0,
            ..Default::default()
        });
        let screen = screen(1000.0, 800.0);
        let gaps = Gaps::default();
        let frames =
            system.calculate_layout(layout, screen, &constraints, &tile::Borders::default(), &gaps);

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
    /// One key, both directions, and it comes back: a column of three folded out and back in is a
    /// column of three again.
    #[test]
    fn folding_a_window_in_and_out_returns_it_to_the_same_row() {
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);

        assert!(system.select_window(layout, w[1]));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(
            shape(&mut system, layout),
            vec![vec![w[0], w[2]], vec![w[1]]],
            "folded out into its own column"
        );

        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(
            shape(&mut system, layout),
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
        assert_eq!(shape(&mut system, layout), vec![vec![left, right]]);

        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(
            shape(&mut system, layout),
            vec![vec![left], vec![right]],
            "and out again"
        );
    }

    /// The first column has nothing to its left, and the left key does NOT quietly fold right
    /// instead: with a key bound per side, doing that would make the two keys agree at the ends of
    /// the strip.
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
            shape(&mut system, layout),
            vec![vec![first], vec![second]],
            "nothing on the left to fold into"
        );

        // The other key reaches the column that IS there.
        system.toggle_fold_of_selection(layout, Direction::Right);
        assert_eq!(shape(&mut system, layout), vec![vec![second, first]]);
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
        assert_eq!(shape(&mut system, layout), vec![vec![right, left]]);

        // Out again, to the left of that column, which is where it started.
        system.toggle_fold_of_selection(layout, Direction::Right);
        assert_eq!(
            shape(&mut system, layout),
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
        assert_eq!(shape(&mut system, layout), vec![vec![left, right]]);

        system.toggle_fold_of_selection(layout, Direction::Right);
        assert_eq!(
            shape(&mut system, layout),
            vec![vec![right], vec![left]],
            "the right key sends it out to the left, so it lands the other side of its neighbour"
        );
    }

    /// Folding OUT lands away from the key's own side, which is what makes pressing one key twice
    /// a round trip: a window folded into the column on the left came FROM its right.
    #[test]
    fn folding_out_lands_away_from_the_side_the_key_names() {
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);
        assert!(system.select_window(layout, w[1]));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(
            shape(&mut system, layout),
            vec![vec![w[0], w[2]], vec![w[1]]],
            "the left key sends it out to the right"
        );

        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);
        assert!(system.select_window(layout, w[1]));
        system.toggle_fold_of_selection(layout, Direction::Right);
        assert_eq!(
            shape(&mut system, layout),
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
            let (mut system, layout, _w) = column_of(ScrollingLayoutSettings::default(), 3);
            let selected = system.selected_window(layout).expect("a selection");
            let before = shape(&mut system, layout);
            system.toggle_fold_of_selection(layout, side);
            system.toggle_fold_of_selection(layout, side);
            assert_eq!(
                shape(&mut system, layout),
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
            let before = shape(&mut system, layout);
            system.toggle_fold_of_selection(layout, side);
            assert_ne!(
                shape(&mut system, layout),
                before,
                "{side:?} should have folded it in"
            );
            system.toggle_fold_of_selection(layout, side);
            assert_eq!(shape(&mut system, layout), before, "{side:?} from a lone column");
        }
    }

    /// Folding acts on the SELECTED window, never on whichever happens to be first: a fold that fell
    /// back to the top of the strip folds a window the user is not looking at.
    #[test]
    fn folding_out_takes_the_selected_window_not_the_top_of_the_stack() {
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);

        assert!(system.select_window(layout, w[2]));
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(
            shape(&mut system, layout),
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
        let (mut system, layout, w) = column_of(ScrollingLayoutSettings::default(), 3);
        let before = shape(&mut system, layout);

        system.clear_selection_for_test(layout);
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(
            shape(&mut system, layout),
            before,
            "no selection is not a licence to move w0"
        );
        let _ = w;
    }

    // ── a mouse drag on an edge ─────────────────────────────────────────────

    /// A display 1728pt wide with `gaps 3`, so the tiling area is x=3, w 1722.
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
    fn laid_out_xw(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        wid: WindowId,
    ) -> (f64, f64) {
        let frame = frame_for(&render(system, layout, KAI_SCREEN, &kai_s_gaps()), wid);
        (frame.origin.x, frame.size.width)
    }

    /// The app sizing `wid` itself, then a pass.
    fn app_sizes(system: &mut ScrollingLayoutSystem, layout: LayoutId, wid: WindowId, width: f64) {
        assert!(system.select_window(layout, wid));
        system.on_window_resized(layout, resized(wid, xw(0.0, width)), KAI_SCREEN, &kai_s_gaps());
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
            system.on_window_resized(
                layout,
                ResizeReport {
                    drag: Some(drag),
                    ..resized(wid, to)
                },
                KAI_SCREEN,
                &kai_s_gaps(),
            );
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
        app_sizes(&mut system, layout, w[0], 594.0);
        app_sizes(&mut system, layout, w[1], 882.0);
        assert_eq!(laid_out_xw(&mut system, layout, w[0]).1, 594.0);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]).1, 882.0);
        (system, layout, w)
    }

    /// Kai's columns under `never`: column 0 at [3, w 594], and column 1 at [600, w 882] with the
    /// strip at offset 0.
    fn kai_s_strip() -> (ScrollingLayoutSystem, LayoutId, [WindowId; 3]) {
        let (mut system, layout, w) = kai_s_columns(CenterFocusedColumn::Never);
        assert_eq!(laid_out_xw(&mut system, layout, w[0]), (3.0, 594.0));
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (600.0, 882.0));
        assert_eq!(system.scroll_offset(layout), 0.0);
        (system, layout, w)
    }

    /// Kai's columns under `always`, column 1 centred at [423, w 882].
    fn kai_s_centred_strip() -> (ScrollingLayoutSystem, LayoutId, [WindowId; 3]) {
        let (mut system, layout, w) = kai_s_columns(CenterFocusedColumn::Always);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (423.0, 882.0));
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

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (334.0, 1148.0));
        assert_near(system.scroll_offset(layout), 266.0, "offset");
        assert_eq!(laid_out_xw(&mut system, layout, w[0]).0, -263.0);
    }

    /// Kai's press 16, which landed back at 1148pt, `max-column-width`. 1443/1722 takes no gap
    /// share.
    #[test]
    fn kai_s_drag_to_1443_keeps_1443() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (39.0, 1443.0));
        assert_eq!(laid_out_xw(&mut system, layout, w[0]).0, -558.0);
    }

    #[test]
    fn a_left_edge_shrink_keeps_the_right_edge_while_the_strip_can_scroll() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());

        drag(&mut system, layout, w[1], xw(334.0, 1148.0), xw(434.0, 1048.0));
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (434.0, 1048.0));
        assert_near(system.scroll_offset(layout), 166.0, "offset");
    }

    /// Keeping the right edge would need offset -100: space before the first column, which the
    /// strip never shows.
    #[test]
    fn a_left_edge_shrink_at_the_strip_start_keeps_the_left_edge() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(700.0, 782.0));

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (600.0, 782.0));
        assert_eq!(system.scroll_offset(layout), 0.0);
    }

    /// Kai's press 8, which landed at 572pt, `min-column-width`. The dragged 487pt, with the left
    /// edge kept at the strip's start.
    #[test]
    fn a_drag_below_min_column_width_keeps_its_width() {
        let (mut system, layout, w) = kai_s_strip();
        app_sizes(&mut system, layout, w[1], 595.0);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (600.0, 595.0));

        drag(&mut system, layout, w[1], xw(600.0, 595.0), xw(708.0, 487.0));
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (600.0, 487.0));
        assert_eq!(system.scroll_offset(layout), 0.0);
    }

    /// A terminal settling to its cell grid after the release. Clamped, it would undo the drag.
    #[test]
    fn an_app_settling_under_a_dragged_width_keeps_it_unbounded() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));
        render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());

        system.on_window_resized(
            layout,
            resized(w[1], xw(39.0, 1440.0)),
            KAI_SCREEN,
            &kai_s_gaps(),
        );
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (39.0, 1440.0));
    }

    /// Only a drag that moves the left or right edge marks the width as dragged, so a stacked
    /// edge's drag leaves the column held to the bounds, and the app's own width with it.
    #[test]
    fn a_drag_on_a_stacked_edge_leaves_the_bounds_on_the_column() {
        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[1]));
        let from = at_yh(&mut system, layout, w[1], 374.0, 368.0);
        let to = at_yh(&mut system, layout, w[1], 274.0, 468.0);
        drag(&mut system, layout, w[1], from, to);

        let report = CGRect::new(from.origin, CGSize::new(1443.0, from.size.height));
        system.on_window_resized(layout, resized(w[1], report), KAI_SCREEN, &kai_s_gaps());
        assert_eq!(laid_out_xw(&mut system, layout, w[1]).1, 1148.0);
    }

    /// `Mod+F` gives back the width the drag started from, 860pt, whole, as a dragged width lands.
    /// A stacked window stays in its stack, a column the whole viewport wide.
    #[test]
    fn a_drag_to_the_full_viewport_makes_a_lone_window_full_width() {
        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let lone = wid(2, 1);
        system.add_window_after_selection(layout, lone);
        let start = frame_for(&render(&mut system, layout, KAI_SCREEN, &kai_s_gaps()), lone);
        let full = CGRect::new(start.origin, CGSize::new(1722.0, start.size.height));
        drag(&mut system, layout, lone, start, full);

        assert!(system.is_window_full_width(layout, lone));
        assert_eq!(
            frame_for(&render(&mut system, layout, KAI_SCREEN, &kai_s_gaps()), lone),
            full
        );
        toggle_selection(&mut system, layout);
        let back = frame_for(&render(&mut system, layout, KAI_SCREEN, &kai_s_gaps()), lone);
        assert_eq!(
            (start.size.width, back.size.width),
            (860.0, 860.0),
            "the toggle back"
        );

        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[1]));
        let from = frame_for(&render(&mut system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]);
        let across = CGRect::new(from.origin, CGSize::new(1722.0, from.size.height));
        drag(&mut system, layout, w[1], from, across);
        assert!(!system.is_window_full_width(layout, w[1]));
        assert_eq!(shape(&mut system, layout), vec![w.to_vec()]);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (3.0, 1722.0), "stacked");
    }

    /// Clamped back to `max-column-width`, the grow would shrink it to 1148pt.
    #[test]
    fn a_grow_key_on_a_column_dragged_past_max_does_nothing() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));
        render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());

        system.resize_selection_by(layout, 0.05, ResizeOrientation::Horizontal);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (39.0, 1443.0));
    }

    /// Clamped up to `min-column-width`, the shrink would grow it to 572pt.
    #[test]
    fn a_shrink_key_on_a_column_dragged_below_min_does_nothing() {
        let (mut system, layout, w) = kai_s_strip();
        app_sizes(&mut system, layout, w[1], 597.0);
        drag(&mut system, layout, w[1], xw(600.0, 595.0), xw(708.0, 487.0));
        render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());

        system.resize_selection_by(layout, -0.05, ResizeOrientation::Horizontal);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (600.0, 487.0));
    }

    /// Dragged to 0.69977 of the tiling width, one step down is 0.64977, 1117pt after the gap
    /// share; from the bound it would be 1060. From 1443pt the step lands past the bound, on it.
    #[test]
    fn a_shrink_key_on_it_starts_from_its_width() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(277.0, 1205.0));
        render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());
        system.resize_selection_by(layout, -0.05, ResizeOrientation::Horizontal);
        assert_eq!(
            laid_out_xw(&mut system, layout, w[1]).1,
            1117.0,
            "inside the bounds"
        );

        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));
        render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());
        system.resize_selection_by(layout, -0.05, ResizeOrientation::Horizontal);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]).1, 1148.0, "past max");
    }

    /// A preset below 1.0 is a width given by a key, so the bounds hold it again. 1.0 is held only
    /// to the viewport.
    #[test]
    fn mod_r_on_it_takes_the_first_preset_above() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));
        system.cycle_preset_column_width(layout);
        assert_eq!(
            laid_out_xw(&mut system, layout, w[1]).1,
            1722.0,
            "past two thirds"
        );
        assert!(!system.is_window_full_width(layout, w[1]), "past two thirds");

        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(782.0, 700.0));
        system.cycle_preset_column_width(layout);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]).1, 860.0, "a half");
        system.on_window_resized(
            layout,
            resized(w[1], xw(39.0, 1443.0)),
            KAI_SCREEN,
            &kai_s_gaps(),
        );
        assert_eq!(laid_out_xw(&mut system, layout, w[1]).1, 1148.0, "bounded again");
    }

    /// A stack's heights made its own: the top window's bottom edge dragged 100pt down, so the rows
    /// differ from the even split a fold leaves. A lone window is left as it is.
    fn heights_dragged_apart(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        w: &[WindowId],
    ) -> Vec<(f64, f64)> {
        if w.len() > 1 {
            assert!(system.select_window(layout, w[0]));
            let (y, height) = laid_out_yh(system, layout, w[0]);
            let from = at_yh(system, layout, w[0], y, height);
            let taller = at_yh(system, layout, w[0], y, height + 100.0);
            drag(system, layout, w[0], from, taller);
            assert_eq!(
                laid_out_yh(system, layout, w[0]),
                (y, height + 100.0),
                "dragged apart"
            );
        }
        w.iter().map(|&id| laid_out_yh(system, layout, id)).collect()
    }

    /// `Mod+R` from two thirds makes the whole column the tiling area wide, its stack, heights and
    /// place kept, and is no full-width mode (`specs/strip.md`, "Columns and widths").
    #[test]
    fn the_cycle_s_full_viewport_is_a_width_that_keeps_the_column_and_its_stack() {
        for rows in 1..=3 {
            for row in 0..rows {
                let (mut system, layout, w) = column_of(preset_settings(), rows);
                let heights = heights_dragged_apart(&mut system, layout, &w);
                assert!(system.select_window(layout, w[row]));
                system.cycle_preset_column_width(layout);
                assert_eq!(
                    laid_out_xw(&mut system, layout, w[row]).1,
                    1148.0,
                    "{rows}/{row}: 2/3"
                );

                system.cycle_preset_column_width(layout);

                let case = format!("{rows} rows, row {row}");
                assert_eq!(shape(&mut system, layout), vec![w.clone()], "{case}");
                for &id in &w {
                    assert_eq!(laid_out_xw(&mut system, layout, id), (3.0, 1722.0), "{case}");
                }
                let after: Vec<_> =
                    w.iter().map(|&id| laid_out_yh(&mut system, layout, id)).collect();
                assert_eq!(after, heights, "{case}");
                assert_eq!(system.scroll_offset(layout), 0.0, "{case}");
                let state = system.layout_state(layout).expect("layout");
                assert!(state.fullscreen_within_gaps.is_empty(), "{case}");
                assert!(state.full_width_origins.is_empty(), "{case}");
                assert!(!system.is_window_full_width(layout, w[row]), "{case}");
            }
        }
    }

    /// Column 1 of Kai's three, stacked two high, goes the tiling area wide in its place: its
    /// neighbours keep their order and widths, and the strip shows it from its start, as the reveal
    /// shows any column that fills the viewport.
    #[test]
    fn the_cycle_s_full_viewport_keeps_a_stacked_column_s_place_on_the_strip() {
        let (mut system, layout, w) = kai_s_strip();
        let folded = wid(1, 4);
        system.add_window_after_selection(layout, folded);
        system.toggle_fold_of_selection(layout, Direction::Left);
        let strip = vec![vec![w[0]], vec![w[1], folded], vec![w[2]]];
        assert_eq!(shape(&mut system, layout), strip, "fixture");
        let w2_width = laid_out_xw(&mut system, layout, w[2]).1;
        assert!(system.select_window(layout, folded));

        system.cycle_preset_column_width(layout);
        system.cycle_preset_column_width(layout);

        assert_eq!(shape(&mut system, layout), strip);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (3.0, 1722.0));
        assert_eq!(laid_out_xw(&mut system, layout, folded), (3.0, 1722.0));
        assert_eq!(
            laid_out_xw(&mut system, layout, w[0]),
            (-594.0, 594.0),
            "left neighbour"
        );
        assert_eq!(
            laid_out_xw(&mut system, layout, w[2]),
            (1728.0, w2_width),
            "right neighbour"
        );
        assert_eq!(system.scroll_offset(layout), 597.0);
        assert!(!system.is_window_full_width(layout, folded));
    }

    /// An app that settles 20pt short of the viewport, past the 1% the cycle reads as the same
    /// width. Read back, the column would sit at 0.988, the next `Mod+R` would find 100% above it
    /// again, and every later press would stand still.
    #[test]
    fn an_app_settling_short_of_the_full_viewport_leaves_it_and_the_cycle_still_wraps() {
        let (mut system, layout, w) = column_of(preset_settings(), 2);
        assert!(system.select_window(layout, w[1]));
        system.cycle_preset_column_width(layout);
        system.cycle_preset_column_width(layout);
        let full = frame_for(&render(&mut system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]);
        let short = CGRect::new(full.origin, CGSize::new(1702.0, full.size.height));

        system.on_window_resized(layout, resized(w[1], short), KAI_SCREEN, &kai_s_gaps());
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (3.0, 1722.0), "kept");

        system.cycle_preset_column_width(layout);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]).1, 572.0);
    }

    /// A column the whole viewport wide, by `Mod+R`'s 100% or a stacked column dragged across it,
    /// takes no width from its app however far the app resizes itself, as a full-width column takes
    /// none.
    #[test]
    fn an_app_resizing_a_column_the_viewport_wide_leaves_it_the_viewport_wide() {
        let (mut system, layout, w) = column_of(preset_settings(), 2);
        assert!(system.select_window(layout, w[1]));
        system.cycle_preset_column_width(layout);
        system.cycle_preset_column_width(layout);
        render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());
        app_sizes(&mut system, layout, w[1], 800.0);
        assert_eq!(
            laid_out_xw(&mut system, layout, w[1]),
            (3.0, 1722.0),
            "Mod+R's 100%"
        );

        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[1]));
        let from = frame_for(&render(&mut system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]);
        drag(
            &mut system,
            layout,
            w[1],
            from,
            CGRect::new(from.origin, CGSize::new(1722.0, from.size.height)),
        );
        render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());
        app_sizes(&mut system, layout, w[1], 800.0);
        assert_eq!(
            laid_out_xw(&mut system, layout, w[1]),
            (3.0, 1722.0),
            "dragged across"
        );
    }

    /// A column the viewport wide keeps the viewport when the window server reports its app cannot
    /// go past 1400pt: the capped window sits at 1400pt plus its border inside it, its stack-mate
    /// the viewport wide, so the gap shows the app can go no wider. The app answering that, settling
    /// 20pt short of it or resizing itself to 1000pt takes no width, and `Mod+R` still wraps to a
    /// third.
    #[test]
    fn a_column_the_viewport_wide_keeps_it_past_its_apps_maximum() {
        let border = 4.0;
        let borders = tile::Borders::every(border);
        let capped = |w: &[WindowId]| -> HashMap<WindowId, WindowLayoutConstraints> {
            let limit = WindowLayoutConstraints {
                is_resizable: true,
                max_width: 1400.0,
                ..Default::default()
            };
            HashMap::from_iter([(w[1], limit)])
        };
        let slot_xw = |system: &mut ScrollingLayoutSystem, layout, w: &[WindowId], id| {
            let frames =
                system.calculate_layout(layout, KAI_SCREEN, &capped(w), &borders, &kai_s_gaps());
            let slot = tile::slot_of(frame_for(&frames, id), border);
            (slot.origin.x, slot.size.width)
        };
        let reports = |system: &mut ScrollingLayoutSystem,
                       layout,
                       w: &[WindowId],
                       width: Option<f64>| {
            let frames =
                system.calculate_layout(layout, KAI_SCREEN, &capped(w), &borders, &kai_s_gaps());
            let frame = frame_for(&frames, w[1]);
            assert_eq!(frame.size.width, 1400.0, "the window at its maximum");
            let frame = CGRect::new(
                frame.origin,
                CGSize::new(width.unwrap_or(frame.size.width), frame.size.height),
            );
            system.on_window_resized(
                layout,
                ResizeReport { border, ..resized(w[1], frame) },
                KAI_SCREEN,
                &kai_s_gaps(),
            );
        };
        let held = |system: &mut ScrollingLayoutSystem, layout, w: &[WindowId], how: &str| {
            assert_eq!(
                slot_xw(system, layout, w, w[1]),
                (3.0, 1408.0),
                "{how}: the capped window"
            );
            assert_eq!(
                slot_xw(system, layout, w, w[0]),
                (3.0, 1722.0),
                "{how}: the column"
            );
            for width in [None, Some(1380.0), Some(1000.0)] {
                reports(system, layout, w, width);
                assert_eq!(
                    slot_xw(system, layout, w, w[1]),
                    (3.0, 1408.0),
                    "{how}: {width:?} taken"
                );
                assert_eq!(
                    slot_xw(system, layout, w, w[0]),
                    (3.0, 1722.0),
                    "{how}: {width:?} narrowed it"
                );
            }
            system.cycle_preset_column_width(layout);
            assert_eq!(slot_xw(system, layout, w, w[0]).1, 572.0, "{how}: Mod+R wraps");
        };

        let (mut system, layout, w) = column_of(preset_settings(), 2);
        assert!(system.select_window(layout, w[1]));
        system.cycle_preset_column_width(layout);
        system.cycle_preset_column_width(layout);
        held(&mut system, layout, &w, "Mod+R's 100%");

        let (mut system, layout, w) = column_of(preset_settings(), 2);
        assert!(system.select_window(layout, w[1]));
        let frames =
            system.calculate_layout(layout, KAI_SCREEN, &capped(&w), &borders, &kai_s_gaps());
        let from = frame_for(&frames, w[1]);
        let to = CGRect::new(from.origin, CGSize::new(1722.0 - 2.0 * border, from.size.height));
        let drag = Drag::report(from, to, None);
        system.on_window_resized(
            layout,
            ResizeReport {
                drag: Some(drag),
                border,
                ..resized(w[1], to)
            },
            KAI_SCREEN,
            &kai_s_gaps(),
        );
        held(&mut system, layout, &w, "dragged across");
    }

    /// `Mod+R`'s 100% is above `max-column-width`: a grow key does nothing, and a shrink key starts
    /// from the 1722pt the column has, landing on the bound, and keeps the stack.
    #[test]
    fn a_resize_key_on_mod_r_s_100_percent_starts_from_its_width() {
        let (mut system, layout, w) = column_of(preset_settings(), 2);
        assert!(system.select_window(layout, w[1]));
        system.cycle_preset_column_width(layout);
        system.cycle_preset_column_width(layout);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (3.0, 1722.0), "fixture");

        system.resize_selection_by(layout, 0.05, ResizeOrientation::Horizontal);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (3.0, 1722.0), "a grow");
        system.resize_selection_by(layout, -0.05, ResizeOrientation::Horizontal);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]).1, 1148.0, "a shrink");
        assert_eq!(shape(&mut system, layout), vec![w.clone()]);
    }

    #[test]
    fn the_cycle_steps_from_the_full_viewport_to_the_narrowest_preset() {
        let (mut system, layout, w) = column_of(preset_settings(), 2);
        assert!(system.select_window(layout, w[1]));
        system.cycle_preset_column_width(layout);
        system.cycle_preset_column_width(layout);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]).1, 1722.0);

        system.cycle_preset_column_width(layout);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]).1, 572.0);
        assert_eq!(laid_out_xw(&mut system, layout, w[0]).1, 572.0);
    }

    /// `Mod+F` maximises afresh from the 100% column and gives that column back: a stacked window
    /// rejoins its stack with the heights it had, the column still the viewport wide.
    #[test]
    fn mod_f_from_the_cycle_s_full_viewport_restores_to_it() {
        for rows in [1, 3] {
            let (mut system, layout, w) = column_of(preset_settings(), rows);
            let heights = heights_dragged_apart(&mut system, layout, &w);
            let row = rows / 2;
            assert!(system.select_window(layout, w[row]));
            system.cycle_preset_column_width(layout);
            system.cycle_preset_column_width(layout);
            let before: Vec<_> = w.iter().map(|&id| laid_out_xw(&mut system, layout, id)).collect();

            toggle_selection(&mut system, layout);
            assert!(
                system.is_window_full_width(layout, w[row]),
                "{rows} rows: maximised"
            );
            toggle_selection(&mut system, layout);

            assert!(!system.is_window_full_width(layout, w[row]), "{rows} rows");
            assert_eq!(shape(&mut system, layout), vec![w.clone()], "{rows} rows");
            let after: Vec<_> = w.iter().map(|&id| laid_out_xw(&mut system, layout, id)).collect();
            assert_eq!(after, before, "{rows} rows");
            assert_eq!(after[row], (3.0, 1722.0), "{rows} rows");
            let rows_after: Vec<_> =
                w.iter().map(|&id| laid_out_yh(&mut system, layout, id)).collect();
            assert_eq!(rows_after, heights, "{rows} rows: heights");
        }
    }

    /// With 1.0 the only preset, the step off a maximised window is the 100% width: the mode ends
    /// where the window is, and it stays the viewport wide.
    #[test]
    fn the_only_preset_ends_a_maximise_in_place_at_the_full_viewport() {
        let mut settings = preset_settings();
        settings.preset_column_widths = vec![1.0];
        let (mut system, layout, w) = column_of(settings, 3);
        assert!(system.select_window(layout, w[1]));
        toggle_selection(&mut system, layout);
        assert_eq!(shape(&mut system, layout), vec![vec![w[1]], vec![w[0], w[2]]]);

        system.cycle_preset_column_width(layout);

        assert!(!system.is_window_full_width(layout, w[1]));
        assert_eq!(
            shape(&mut system, layout),
            vec![vec![w[1]], vec![w[0], w[2]]],
            "in place"
        );
        assert_eq!(laid_out_xw(&mut system, layout, w[1]).1, 1722.0);
        assert!(system.layout_state(layout).expect("layout").full_width_origins.is_empty());
    }

    #[test]
    fn the_first_column_grown_from_its_left_edge_ends_grown_to_the_right() {
        let (mut system, layout, w) = kai_s_strip();
        assert!(system.select_window(layout, w[0]));
        render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());
        drag(&mut system, layout, w[0], xw(3.0, 594.0), xw(-77.0, 674.0));
        assert_eq!(laid_out_xw(&mut system, layout, w[0]), (3.0, 674.0), "3 columns");
        assert_eq!(laid_out_xw(&mut system, layout, w[1]).0, 680.0);

        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let lone = wid(2, 1);
        system.add_window_after_selection(layout, lone);
        let (x, width) = laid_out_xw(&mut system, layout, lone);
        drag(
            &mut system,
            layout,
            lone,
            xw(x, width),
            xw(x - 80.0, width + 80.0),
        );
        let (x_after, width_after) = laid_out_xw(&mut system, layout, lone);
        assert_eq!(x_after, x, "1 column");
        assert!(width_after > width + 75.0, "1 column: {width} -> {width_after}");
    }

    /// The left edge in by 200pt: 1522pt, past `max-column-width`, with the right edge kept.
    #[test]
    fn a_drag_on_a_full_width_column_ends_it_at_the_dragged_width() {
        let (mut system, layout, w) = kai_s_strip();
        toggle_selection(&mut system, layout);
        let full = frame_for(&render(&mut system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]);

        let shrunk = CGRect::new(
            CGPoint::new(full.origin.x + 200.0, full.origin.y),
            CGSize::new(full.size.width - 200.0, full.size.height),
        );
        drag(&mut system, layout, w[1], full, shrunk);
        assert!(!system.is_window_full_width(layout, w[1]));
        assert_eq!(
            frame_for(&render(&mut system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]),
            shrunk
        );
    }

    /// No width was dragged, so the window stays the full-width column it was.
    #[test]
    fn a_top_edge_drag_on_a_full_width_column_leaves_it_full_width() {
        let (mut system, layout, w) = kai_s_strip();
        toggle_selection(&mut system, layout);
        let full = frame_for(&render(&mut system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]);

        let lower = CGRect::new(
            CGPoint::new(full.origin.x, full.origin.y + 100.0),
            CGSize::new(full.size.width, full.size.height - 100.0),
        );
        drag(&mut system, layout, w[1], full, lower);
        assert!(system.is_window_full_width(layout, w[1]));
        assert_eq!(
            frame_for(&render(&mut system, layout, KAI_SCREEN, &kai_s_gaps()), w[1]),
            full
        );
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

    fn stack_of(rows: u32) -> Column {
        Column {
            windows: (1..=rows).map(|idx| wid(1, idx)).collect(),
            ..Column::default()
        }
    }

    fn a_drag(end: Option<End>) -> Option<Drag> {
        Some(Drag {
            from: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(500.0, 400.0)),
            end,
        })
    }

    #[test]
    fn a_lone_window_s_resize_leaves_its_column_without_rows_to_share() {
        let mut column = stack_of(1);
        column.resize_row(0, None, 300.0, 810.0, 10.0);
        column.resize_row(0, a_drag(Some(End::Bottom)), 300.0, 810.0, 10.0);
        assert!(column.height_weights.is_empty());
        assert!(!column.height_overridden);
    }

    #[test]
    fn a_resize_with_no_column_height_leaves_the_rows() {
        let mut column = stack_of(2);
        column.resize_row(0, None, 300.0, 0.0, 10.0);
        assert!(column.height_weights.is_empty());
        assert!(!column.height_overridden);
    }

    #[test]
    fn a_drag_that_moves_neither_end_leaves_the_rows_even() {
        let mut column = stack_of(2);
        column.resize_row(0, a_drag(None), 300.0, 810.0, 10.0);
        assert_eq!(column.height_weights, vec![1.0, 1.0]);
        assert!(!column.height_overridden);
    }

    #[test]
    fn a_dragged_end_trades_with_the_row_across_it_and_an_app_s_height_takes_from_the_rest() {
        let mut dragged = stack_of(3);
        dragged.resize_row(0, a_drag(Some(End::Bottom)), 300.0, 920.0, 10.0);
        assert_eq!(dragged.height_weights, vec![300.0, 300.0, 300.0]);
        dragged.resize_row(0, a_drag(Some(End::Bottom)), 400.0, 920.0, 10.0);
        assert_eq!(dragged.height_weights, vec![400.0, 200.0, 300.0]);
        assert!(dragged.height_overridden);

        let mut settled = stack_of(3);
        settled.resize_row(0, None, 400.0, 920.0, 10.0);
        assert_eq!(settled.height_weights, vec![400.0, 250.0, 250.0]);
        assert!(settled.height_overridden);
    }

    #[test]
    fn a_dragged_column_is_written_so_a_rini_from_before_it_reads_it() {
        let column = Column {
            windows: vec![wid(1, 1)],
            width: Some(GivenWidth::Unbounded(0.33797)),
            ..Column::default()
        };
        let written = ron::ser::to_string(&column).unwrap();
        assert!(written.contains("width_dragged:true"), "{written}");
        let old: ColumnBeforeDragged = ron::from_str(&written).expect("an older rini reads it");
        assert_eq!(old.windows, column.windows);
        assert_eq!(old.width_offset, 0.33797);
        assert!(old.width_overridden);
    }

    #[test]
    fn a_layout_file_column_has_a_width_only_where_it_is_marked_given_one() {
        let cases = [
            ("width_offset:0.2,width_overridden:false", None),
            ("width_offset:0.2,width_dragged:true", None),
            (
                "width_offset:0.2,width_overridden:true,width_dragged:false",
                Some(GivenWidth::Bounded(0.2)),
            ),
            (
                "width_offset:0.2,width_overridden:true,width_dragged:true",
                Some(GivenWidth::Unbounded(0.2)),
            ),
        ];
        for (fields, width) in cases {
            let column: Column = ron::from_str(&format!("(windows:[],{fields})")).unwrap();
            assert_eq!(column.width, width, "{fields}");
        }
    }

    #[test]
    fn a_dragged_width_survives_a_save_and_restore() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(39.0, 1443.0));
        render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());

        let mut restored: ScrollingLayoutSystem =
            ron::from_str(&ron::ser::to_string(&system).unwrap()).unwrap();
        restored.settings = system.settings.clone();
        assert_eq!(laid_out_xw(&mut restored, layout, w[1]).1, 1443.0);
    }

    /// `Mod+R`'s 100% is a width, so the layout file carries it as one: the column comes back the
    /// viewport wide and held only to it, not maximised.
    #[test]
    fn a_column_mod_r_made_the_viewport_wide_survives_a_save_and_restore_as_a_width() {
        let (mut system, layout, w) = kai_s_three_thirds();
        mod_r(&mut system, layout);
        mod_r(&mut system, layout);
        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (3.0, 1722.0));

        let mut restored: ScrollingLayoutSystem =
            ron::from_str(&ron::ser::to_string(&system).unwrap()).unwrap();
        restored.settings = system.settings.clone();
        let state = restored.layout_state(layout).expect("layout");
        let (col_idx, _) = state.locate(w[1]).expect("column");
        let column = &state.columns[col_idx];
        let Some(GivenWidth::Unbounded(offset)) = column.width else {
            panic!("an unbounded width: {column:?}");
        };
        assert_near(state.column_width_ratio + offset, 1.0, "ratio");
        assert!(!restored.is_window_full_width(layout, w[1]));
        assert_eq!(laid_out_xw(&mut restored, layout, w[1]).1, 1722.0);
    }

    #[test]
    fn a_window_resizing_itself_keeps_its_column_where_it_was() {
        let (mut system, layout, w) = kai_s_strip();
        system.on_window_resized(
            layout,
            resized(w[1], xw(500.0, 982.0)),
            KAI_SCREEN,
            &kai_s_gaps(),
        );

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (600.0, 982.0));
        assert_eq!(system.scroll_offset(layout), 0.0);
    }

    /// Pinned, column 2 would end with its right edge at 1482; revealed, it ends at the screen's
    /// edge, x 865.5 and w 859.5 before rounding.
    #[test]
    fn a_focus_change_after_a_left_edge_report_leaves_the_new_column_unpinned() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        assert!(system.select_window(layout, w[2]));

        assert_eq!(laid_out_xw(&mut system, layout, w[2]), (866.0, 860.0));
        assert_near(system.scroll_offset(layout), 885.5, "offset");
    }

    /// A request for the dragged window made after its report and before the pass wins over the
    /// pin, which would put its right edge back at 1482 and its x at 336: centring, the reveal a key
    /// or `Mod+F` asks for, and the alignment a focus move that finds no window asks for.
    #[test]
    fn a_scroll_request_after_a_left_edge_report_wins_over_the_kept_edge() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        system.center_selected_column(layout);
        assert_eq!(
            laid_out_xw(&mut system, layout, w[1]),
            (290.0, 1148.0),
            "centred"
        );

        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        system.resize_selection_by(layout, 0.05, ResizeOrientation::Horizontal);
        assert_eq!(
            laid_out_xw(&mut system, layout, w[1]),
            (577.0, 1148.0),
            "a key, revealed"
        );

        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        assert!(system.toggle_full_width(layout, w[1]));
        assert_eq!(
            laid_out_xw(&mut system, layout, w[1]),
            (3.0, 1722.0),
            "Mod+F, revealed"
        );

        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        assert_eq!(system.move_focus(layout, Direction::Down).0, None);
        assert_eq!(
            laid_out_xw(&mut system, layout, w[1]),
            (3.0, 1148.0),
            "a focus move that finds none"
        );
    }

    /// Any frame under a held button counts as the drag, rini's own write to a neighbour coming
    /// back included.
    #[test]
    fn a_drag_report_for_a_window_other_than_the_focused_one_changes_no_width() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[0], xw(3.0, 594.0), xw(3.0, 694.0));

        assert_eq!(laid_out_xw(&mut system, layout, w[0]), (3.0, 594.0));
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (600.0, 882.0));
    }

    /// The release refocuses the window it let go of before its pass, which must not cost the pin.
    #[test]
    fn refocusing_the_window_after_a_left_edge_report_keeps_its_right_edge() {
        let (mut system, layout, w) = kai_s_strip();
        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(334.0, 1148.0));
        assert!(system.select_window(layout, w[1]));

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (334.0, 1148.0));
        assert_near(system.scroll_offset(layout), 266.0, "offset");
    }

    /// The selection passes to column 2 without a scroll request, and the drag's reveal is still
    /// pending. Pinned, column 2 would end at offset 264.5; revealed, flush with the screen's edge at
    /// x 578.5 and w 1146.5 before rounding.
    #[test]
    fn a_removed_window_s_pin_does_not_move_the_column_selected_after_it() {
        let (mut system, layout, w) = kai_s_strip();
        app_sizes(&mut system, layout, w[2], 1148.0);
        app_sizes(&mut system, layout, w[0], 594.0);
        assert!(system.select_window(layout, w[1]));
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (600.0, 882.0));

        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(500.0, 982.0));
        system.remove_window(w[1]);
        assert_eq!(system.selected_window(layout), Some(w[2]));

        assert_eq!(laid_out_xw(&mut system, layout, w[2]), (579.0, 1147.0));
        assert_near(system.scroll_offset(layout), 21.5, "offset");
    }

    /// 684pt asked and laid out, 519pt from either edge of the tiling area. Undoubled it would end
    /// at [547, w 634].
    #[test]
    fn in_centred_mode_a_middle_column_dragged_out_50_from_its_left_edge_grows_100_and_stays_centred()
     {
        let (mut system, layout, w) = kai_s_centred_strip();
        app_sizes(&mut system, layout, w[1], 584.0);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (572.0, 584.0));

        drag(&mut system, layout, w[1], xw(572.0, 584.0), xw(522.0, 634.0));
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (522.0, 684.0));
    }

    /// The first column, flush left, takes a right-edge drag as dragged; the last, flush right, a
    /// left-edge one. Doubled, they would end 752.5 and 756.5pt wide.
    #[test]
    fn in_centred_mode_the_end_columns_grow_away_from_their_flush_edge() {
        let (mut system, layout, w) = kai_s_centred_strip();
        assert!(system.select_window(layout, w[0]));
        assert_eq!(laid_out_xw(&mut system, layout, w[0]), (3.0, 594.0));
        drag(&mut system, layout, w[0], xw(3.0, 594.0), xw(3.0, 674.0));
        assert_eq!(laid_out_xw(&mut system, layout, w[0]), (3.0, 674.0), "first");

        app_sizes(&mut system, layout, w[2], 598.0);
        assert_eq!(laid_out_xw(&mut system, layout, w[2]), (1127.0, 598.0));
        drag(&mut system, layout, w[2], xw(1127.0, 598.0), xw(1047.0, 678.0));
        assert_eq!(laid_out_xw(&mut system, layout, w[2]), (1047.0, 678.0), "last");
    }

    /// Out 150 asks for 1182pt, past `max-column-width`, and takes it whole. Bounded, it would
    /// end at [290, w 1148].
    #[test]
    fn in_centred_mode_a_doubled_drag_past_max_column_width_keeps_its_width() {
        let (mut system, layout, w) = kai_s_centred_strip();
        drag(&mut system, layout, w[1], xw(423.0, 882.0), xw(273.0, 1032.0));

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (273.0, 1182.0));
    }

    /// Three windows stacked on Kai's display, each `[y, h]` 368.333pt high before rounding.
    fn kai_s_stack() -> (ScrollingLayoutSystem, LayoutId, Vec<WindowId>) {
        let (mut system, layout, w) = column_of(preset_settings(), 3);
        let rows: Vec<(f64, f64)> =
            w.iter().map(|&id| laid_out_yh(&mut system, layout, id)).collect();
        assert_eq!(rows, [(3.0, 368.0), (374.0, 368.0), (746.0, 368.0)]);
        (system, layout, w)
    }

    /// `[y, h]` as `calculate_layout` returns it.
    fn laid_out_yh(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        wid: WindowId,
    ) -> (f64, f64) {
        let frame = frame_for(&render(system, layout, KAI_SCREEN, &kai_s_gaps()), wid);
        (frame.origin.y, frame.size.height)
    }

    /// `wid`'s laid-out frame with its top at `y` and `height` high.
    fn at_yh(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        wid: WindowId,
        y: f64,
        height: f64,
    ) -> CGRect {
        let frame = frame_for(&render(system, layout, KAI_SCREEN, &kai_s_gaps()), wid);
        CGRect::new(
            CGPoint::new(frame.origin.x, y),
            CGSize::new(frame.size.width, height),
        )
    }

    /// The middle window takes 99.667pt, its 468 less the 368.333 it had, from the top one alone.
    /// Spread over the column as before, the bottom window would end 318.5pt high.
    #[test]
    fn a_middle_window_s_top_edge_in_a_3_stack_leaves_the_bottom_window_alone() {
        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[1]));
        let from = at_yh(&mut system, layout, w[1], 374.0, 368.0);
        let to = at_yh(&mut system, layout, w[1], 274.0, 468.0);
        drag(&mut system, layout, w[1], from, to);

        assert_eq!(laid_out_yh(&mut system, layout, w[0]), (3.0, 269.0), "top");
        assert_eq!(laid_out_yh(&mut system, layout, w[1]), (275.0, 468.0), "middle");
        assert_eq!(laid_out_yh(&mut system, layout, w[2]), (746.0, 368.0), "bottom");
    }

    /// A window that sizes itself takes its height from the rest of its column in their
    /// proportions. Unequal first, 268.667 and 368.333 around a 468.333 middle, so sharing differs
    /// from trading with one neighbour.
    #[test]
    fn a_stacked_window_sizing_itself_takes_its_height_from_the_others_in_proportion() {
        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[1]));
        let from = at_yh(&mut system, layout, w[1], 374.0, 368.0);
        let out = at_yh(&mut system, layout, w[1], 274.0, 468.0);
        drag(&mut system, layout, w[1], from, out);

        let grown = at_yh(&mut system, layout, w[1], 275.0, 568.0);
        system.on_window_resized(layout, resized(w[1], grown), KAI_SCREEN, &kai_s_gaps());

        let rows: Vec<(f64, f64)> =
            w.iter().map(|&id| laid_out_yh(&mut system, layout, id)).collect();
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
        let from = at_yh(&mut system, layout, w[0], 3.0, 368.0);
        let reports =
            [398.0, 428.0, 468.0].map(|height| at_yh(&mut system, layout, w[0], 3.0, height));
        drag_through(&mut system, layout, w[0], from, &reports);

        assert_eq!(laid_out_yh(&mut system, layout, w[0]), (3.0, 468.0), "top");
        assert_eq!(laid_out_yh(&mut system, layout, w[1]), (474.0, 269.0), "middle");
        assert_eq!(laid_out_yh(&mut system, layout, w[2]), (746.0, 368.0), "bottom");
    }

    /// Back at its start the report moved neither edge. The column ends as it began, within the
    /// third of a point the start frame's rounding leaves, not at the 468pt the report before left.
    #[test]
    fn a_stacked_edge_dragged_back_to_where_it_started_gives_the_height_back() {
        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[1]));
        let from = at_yh(&mut system, layout, w[1], 374.0, 368.0);
        let out = at_yh(&mut system, layout, w[1], 274.0, 468.0);
        drag_through(&mut system, layout, w[1], from, &[out, from]);

        let rows: Vec<(f64, f64)> =
            w.iter().map(|&id| laid_out_yh(&mut system, layout, id)).collect();
        assert_eq!(rows, [(3.0, 369.0), (375.0, 368.0), (746.0, 368.0)]);
    }

    /// Marked as a deliberate split, the column would keep its two 552pt rows when the third window
    /// came back with the 1.0 its own column held, and that window would end 1pt high.
    #[test]
    fn a_width_drag_on_a_stack_leaves_its_heights_and_a_window_folded_back_shares_evenly() {
        let (mut system, layout, w) = kai_s_stack();
        assert!(system.select_window(layout, w[2]));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&mut system, layout), vec![vec![w[0], w[1]], vec![w[2]]]);

        assert!(system.select_window(layout, w[0]));
        let from = frame_for(&render(&mut system, layout, KAI_SCREEN, &kai_s_gaps()), w[0]);
        let wider = CGRect::new(
            from.origin,
            CGSize::new(from.size.width + 100.0, from.size.height),
        );
        drag(&mut system, layout, w[0], from, wider);
        assert_eq!(laid_out_yh(&mut system, layout, w[0]), (3.0, 554.0), "top");
        assert_eq!(laid_out_yh(&mut system, layout, w[1]), (560.0, 554.0), "bottom");

        assert!(system.select_window(layout, w[2]));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&mut system, layout), vec![vec![w[0], w[1], w[2]]]);
        let rows: Vec<(f64, f64)> =
            w.iter().map(|&id| laid_out_yh(&mut system, layout, id)).collect();
        assert_eq!(rows, [(3.0, 368.0), (374.0, 368.0), (746.0, 368.0)]);
    }

    // ── the place a width change by key keeps ───────────────────────────────

    /// A width key as the reactor runs it: the key, its pass, the raise's refocus of the same
    /// window and the pass after it.
    fn press(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        key: impl FnOnce(&mut ScrollingLayoutSystem, LayoutId),
    ) -> Vec<(WindowId, CGRect)> {
        let selected = system.selected_window(layout).expect("a selection");
        key(system, layout);
        kai_s_frames(system, layout);
        assert!(system.select_window(layout, selected));
        kai_s_frames(system, layout)
    }

    fn mod_r(system: &mut ScrollingLayoutSystem, layout: LayoutId) -> Vec<(WindowId, CGRect)> {
        press(system, layout, |system, layout| {
            system.cycle_preset_column_width(layout);
        })
    }

    fn resize_key(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        amount: f64,
    ) -> Vec<(WindowId, CGRect)> {
        press(system, layout, |system, layout| {
            system.resize_selection_by(layout, amount, ResizeOrientation::Horizontal);
        })
    }

    fn kept_place(
        system: &ScrollingLayoutSystem,
        layout: LayoutId,
    ) -> Option<(WindowId, super::Place)> {
        system.layout_state(layout).expect("layout").kept_place
    }

    fn xw_in(frames: &[(WindowId, CGRect)], wid: WindowId) -> (f64, f64) {
        let frame = frame_for(frames, wid);
        (frame.origin.x, frame.size.width)
    }

    /// Where `wid` ends on screen; each of x and width is rounded, so it can stand a point past.
    fn right_edge(frames: &[(WindowId, CGRect)], wid: WindowId) -> f64 {
        let (x, width) = xw_in(frames, wid);
        x + width
    }

    /// Columns of these widths on Kai's display, `selected` selected after the pass that shows it.
    fn kai_s_widths(
        ratios: &[f64],
        selected: usize,
    ) -> (ScrollingLayoutSystem, LayoutId, Vec<WindowId>) {
        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let w: Vec<WindowId> = (1..=ratios.len() as u32).map(|idx| wid(1, idx)).collect();
        for (&window, &ratio) in w.iter().zip(ratios) {
            system.add_window_after_selection(layout, window);
            system.set_carried_width(layout, window, ColumnWidth::Offset(ratio - 0.5));
        }
        assert!(system.select_window(layout, w[0]));
        kai_s_frames(&mut system, layout);
        assert!(system.select_window(layout, w[selected]));
        kai_s_frames(&mut system, layout);
        (system, layout, w)
    }

    /// Two thirds on the left, a third on the right and selected (`specs/strip.md`, "Columns and
    /// widths"). The strip is 1722.99pt, so the reveal leaves the third flush with the right edge.
    fn kai_s_two_thirds_and_a_third() -> (ScrollingLayoutSystem, LayoutId, Vec<WindowId>) {
        let (mut system, layout, w) = kai_s_widths(&[0.66667, 0.33333], 1);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (1153.0, 572.0));
        (system, layout, w)
    }

    /// Three thirds, the middle one selected with 575pt free on each side.
    fn kai_s_three_thirds() -> (ScrollingLayoutSystem, LayoutId, Vec<WindowId>) {
        let (mut system, layout, w) = kai_s_widths(&[0.33333, 0.33333, 0.33333], 1);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (578.0, 572.0));
        (system, layout, w)
    }

    /// Each step keeps the right edge and pushes the left column out, or pulls it back in, on its
    /// own side; the third again is the strip as it began, and the cycle goes on from there.
    #[test]
    fn kai_s_right_third_grows_and_shrinks_from_its_left_edge() {
        let (mut system, layout, w) = kai_s_two_thirds_and_a_third();
        let before = kai_s_frames(&mut system, layout);

        let mut steps = Vec::new();
        for _ in 0..5 {
            let frames = mod_r(&mut system, layout);
            let (x, _) = xw_in(&frames, w[1]);
            assert!((right_edge(&frames, w[1]) - 1725.0).abs() <= 1.0, "{frames:?}");
            let pushed = right_edge(&frames, w[0]) + 3.0 - x;
            assert!(pushed.abs() <= 1.0, "the left column, {frames:?}");
            steps.push(frames);
        }

        let widths: Vec<f64> = steps.iter().map(|frames| xw_in(frames, w[1]).1).collect();
        assert_eq!(widths, [860.0, 1148.0, 1722.0, 572.0, 860.0]);
        assert_same_frames(&steps[3], &before);
        assert_same_frames(&steps[4], &steps[0]);
    }

    #[test]
    fn kai_s_left_two_thirds_grows_and_shrinks_from_its_right_edge() {
        let (mut system, layout, w) = kai_s_two_thirds_and_a_third();
        assert!(system.select_window(layout, w[0]));
        let before = kai_s_frames(&mut system, layout);
        assert_eq!(xw_in(&before, w[0]), (3.0, 1148.0));

        for width in [1722.0, 572.0, 860.0] {
            assert_eq!(xw_in(&mod_r(&mut system, layout), w[0]), (3.0, width));
        }
        assert_same_frames(&mod_r(&mut system, layout), &before);
    }

    /// The half lands at 434.25, 143.75pt left of the third's x, and each neighbour moves 143.75pt
    /// out on its own side.
    #[test]
    fn the_middle_of_three_thirds_grows_and_shrinks_from_both_sides() {
        let (mut system, layout, w) = kai_s_three_thirds();
        let before = kai_s_frames(&mut system, layout);

        let half = mod_r(&mut system, layout);
        assert_eq!(xw_in(&half, w[1]), (434.0, 860.0));
        let moved = |wid: WindowId| xw_in(&half, wid).0 - xw_in(&before, wid).0;
        assert!(
            (moved(w[0]) + 143.75).abs() <= 0.5,
            "left neighbour {}",
            moved(w[0])
        );
        assert!(
            (moved(w[2]) - 143.75).abs() <= 0.5,
            "right neighbour {}",
            moved(w[2])
        );

        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (290.0, 1148.0));
        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (3.0, 1722.0));
        assert_same_frames(&mod_r(&mut system, layout), &before);
    }

    /// The second of a half, a third, a half and a third, with 862.5pt free on its left and 287.5pt
    /// on its right: every width keeps the free space three to one.
    #[test]
    fn an_off_centre_column_grows_and_shrinks_in_proportion_to_the_space_beside_it() {
        let (mut system, layout, w) = kai_s_widths(&[0.5, 0.33333, 0.5, 0.33333], 1);
        let before = kai_s_frames(&mut system, layout);
        assert_eq!(xw_in(&before, w[1]), (866.0, 572.0));

        for width in [860.0, 1148.0] {
            let frames = mod_r(&mut system, layout);
            let (x, got) = xw_in(&frames, w[1]);
            assert_eq!(got, width);
            let (left, right) = (x - 3.0, 1725.0 - x - width);
            assert!(
                (left - 3.0 * right).abs() <= 2.0,
                "{width}: {left} against {right}"
            );
        }
        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (3.0, 1722.0));
        assert_same_frames(&mod_r(&mut system, layout), &before);
    }

    /// A stack on the right, each of its rows selected: the column moves as one and keeps its rows.
    /// As a third the strip is narrower than the viewport, and the stack stays where the strip's
    /// start leaves it.
    #[test]
    fn a_stacked_column_grows_and_shrinks_about_its_place_as_one() {
        for rows in [2, 3] {
            for row in 0..rows as usize {
                let (mut system, layout, _, b) = a_stack_on_the_right(rows);
                assert!(system.select_window(layout, b[row]));
                let before = kai_s_frames(&mut system, layout);
                let case = format!("row {row} of {rows}");
                let heights = |frames: &[(WindowId, CGRect)]| -> Vec<(f64, f64)> {
                    b.iter()
                        .map(|&id| {
                            let frame = frame_for(frames, id);
                            (frame.origin.y, frame.size.height)
                        })
                        .collect()
                };

                for (width, right) in [(1148.0, 1725.0), (1722.0, 1725.0), (572.0, 1438.0)] {
                    let frames = mod_r(&mut system, layout);
                    for &id in &b {
                        assert_eq!(xw_in(&frames, id), xw_in(&frames, b[0]), "{case}");
                    }
                    assert_eq!(xw_in(&frames, b[row]).1, width, "{case}");
                    let off = right_edge(&frames, b[row]) - right;
                    assert!(off.abs() <= 1.0, "{case}: {frames:?}");
                    assert_eq!(heights(&frames), heights(&before), "{case}");
                }
                assert_same_frames(&mod_r(&mut system, layout), &before);
            }
        }
    }

    /// Two thirds side by side, the right one selected, leave the viewport empty past the strip's
    /// end: the third grows into it with the left column where it stands, and only the strip filling
    /// the viewport pushes the left column out. The default half and a third grow the same way by
    /// the resize keys.
    #[test]
    fn a_column_in_a_strip_narrower_than_the_viewport_grows_into_the_empty_side() {
        let (mut system, layout, w) = kai_s_widths(&[0.33333, 0.33333], 1);
        let before = kai_s_frames(&mut system, layout);
        assert_eq!(xw_in(&before, w[1]), (578.0, 572.0));

        let half = mod_r(&mut system, layout);
        assert_eq!(xw_in(&half, w[0]), (3.0, 572.0));
        assert_eq!(xw_in(&half, w[1]), (578.0, 860.0));
        let two_thirds = mod_r(&mut system, layout);
        assert_eq!(xw_in(&two_thirds, w[1]).1, 1148.0);
        assert!(
            (right_edge(&two_thirds, w[1]) - 1725.0).abs() <= 1.0,
            "{two_thirds:?}"
        );
        assert!(xw_in(&two_thirds, w[0]).0 >= 2.0, "{two_thirds:?}");
        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (3.0, 1722.0));
        assert_same_frames(&mod_r(&mut system, layout), &before);

        let (mut system, layout, w) = kai_s_widths(&[0.5, 0.33333], 1);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (866.0, 572.0));
        for _ in 0..3 {
            let frames = resize_key(&mut system, layout, 0.05);
            assert_eq!(xw_in(&frames, w[0]).0, 3.0, "{frames:?}");
            assert_eq!(xw_in(&frames, w[1]).0, 866.0, "{frames:?}");
        }
    }

    /// Two thirds, a third and two thirds: the third reached from its left lands flush right and
    /// grows left; reached from its right it lands flush left and grows right.
    #[test]
    fn a_column_grows_from_where_the_focus_that_reached_it_left_it() {
        let (mut system, layout, w) = kai_s_widths(&[0.66667, 0.33333, 0.66667], 0);
        system.move_focus(layout, Direction::Right);
        assert_eq!(
            laid_out_xw(&mut system, layout, w[1]),
            (1153.0, 572.0),
            "reached going right"
        );
        let grown = mod_r(&mut system, layout);
        assert_eq!(xw_in(&grown, w[1]), (866.0, 860.0));

        for direction in [Direction::Right, Direction::Left] {
            system.move_focus(layout, direction);
            kai_s_frames(&mut system, layout);
        }
        assert_eq!(
            laid_out_xw(&mut system, layout, w[1]),
            (3.0, 860.0),
            "reached going left"
        );
        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (3.0, 1148.0));
    }

    /// The half of the middle third with the strip snapped back to 0 stands at 578 with two thirds
    /// of the free space on its left, and two thirds is where its next step stands.
    #[test]
    fn a_strip_snap_drops_the_place_and_the_next_key_takes_it_afresh() {
        let (mut system, layout, w) = kai_s_three_thirds();
        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (434.0, 860.0));
        system.snap_to_nearest_column(layout);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (578.0, 860.0));

        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (386.0, 1148.0));
    }

    /// The first column closing leaves the half of the middle third part off the left: the next
    /// key takes it from where the reveal shows it, flush left.
    #[test]
    fn a_window_closing_drops_the_place_and_the_next_key_takes_it_afresh() {
        let (mut system, layout, w) = kai_s_three_thirds();
        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (434.0, 860.0));
        system.remove_window(w[0]);
        kai_s_frames(&mut system, layout);

        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (3.0, 1148.0));
    }

    /// The half of the middle third, then a window opening after it (focus going to the new window
    /// and coming back), a move right, a fold into the first column, or a left-edge drag: each
    /// drops the place, and the next key takes it from where the column then stands, not at the
    /// half's share, which would put two thirds at 290.
    #[test]
    fn an_opening_a_move_a_fold_or_a_drag_drops_the_place_and_the_next_key_takes_it_afresh() {
        type Event = fn(&mut ScrollingLayoutSystem, LayoutId, &[(WindowId, CGRect)], WindowId);
        type Case = (&'static str, Event, (f64, f64), (f64, f64));
        let events: [Case; 4] = [
            (
                "an opening",
                |system, layout, _, _| system.add_window_after_selection(layout, wid(1, 9)),
                (3.0, 860.0),
                (3.0, 1148.0),
            ),
            (
                "a move",
                |system, layout, _, _| assert!(system.move_selection(layout, Direction::Right)),
                (866.0, 860.0),
                (577.0, 1148.0),
            ),
            (
                "a fold",
                |system, layout, _, _| {
                    system.toggle_fold_of_selection(layout, Direction::Left);
                },
                (3.0, 572.0),
                (3.0, 860.0),
            ),
            (
                "a drag",
                |system, layout, frames, window| {
                    let from = frame_for(frames, window);
                    let to = CGRect::new(
                        CGPoint::new(from.origin.x - 100.0, from.origin.y),
                        CGSize::new(from.size.width + 100.0, from.size.height),
                    );
                    drag(system, layout, window, from, to);
                },
                (334.0, 960.0),
                (252.0, 1148.0),
            ),
        ];
        for (how, event, after, next) in events {
            let (mut system, layout, w) = kai_s_three_thirds();
            let half = mod_r(&mut system, layout);
            assert_eq!(xw_in(&half, w[1]), (434.0, 860.0), "{how}");
            event(&mut system, layout, &half, w[1]);
            kai_s_frames(&mut system, layout);
            assert_eq!(kept_place(&system, layout), None, "{how}");
            assert!(system.select_window(layout, w[1]), "{how}");
            assert_eq!(laid_out_xw(&mut system, layout, w[1]), after, "{how}");

            assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), next, "{how}");
        }
    }

    /// A viewport-wide column focus reached with no place kept takes its end of the strip: among
    /// five thirds the middle one steps to a third in the middle, the first at the left, the last
    /// at the right.
    #[test]
    fn a_viewport_wide_column_reached_by_focus_steps_about_its_end_of_the_strip() {
        for (col, third) in [(0, 3.0), (2, 578.0), (4, 1153.0)] {
            let (mut system, layout, w) = kai_s_widths(&[0.33333; 5], col);
            for _ in 0..3 {
                mod_r(&mut system, layout);
            }
            let (away, back) = if col == 4 {
                (Direction::Left, Direction::Right)
            } else {
                (Direction::Right, Direction::Left)
            };
            system.move_focus(layout, away);
            kai_s_frames(&mut system, layout);
            system.move_focus(layout, back);
            assert_eq!(
                laid_out_xw(&mut system, layout, w[col]),
                (3.0, 1722.0),
                "column {col}"
            );
            assert_eq!(kept_place(&system, layout), None, "column {col}");

            assert_eq!(
                xw_in(&mod_r(&mut system, layout), w[col]),
                (third, 572.0),
                "column {col}"
            );
        }
    }

    /// Kai's right third, centred after its first step: the next steps keep it centred.
    #[test]
    fn centring_drops_the_place_and_the_next_key_keeps_the_column_centred() {
        let (mut system, layout, w) = kai_s_two_thirds_and_a_third();
        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (866.0, 860.0));
        system.center_selected_column(layout);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (434.0, 860.0));

        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (290.0, 1148.0));
    }

    /// Kai's left two thirds centred, where no column but a centred one can stand: every step
    /// keeps it in the middle, and the two thirds again is the strip as it was centred.
    #[test]
    fn a_centred_column_stays_centred_through_the_cycle() {
        let (mut system, layout, w) = kai_s_two_thirds_and_a_third();
        assert!(system.select_window(layout, w[0]));
        system.center_selected_column(layout);
        let before = kai_s_frames(&mut system, layout);
        assert_eq!(xw_in(&before, w[0]), (290.0, 1148.0));

        for want in [(3.0, 1722.0), (578.0, 572.0), (434.0, 860.0)] {
            assert_eq!(xw_in(&mod_r(&mut system, layout), w[0]), want);
        }
        assert_same_frames(&mod_r(&mut system, layout), &before);
    }

    /// `set-column-width +5%` and `-5%` on Kai's right third keep its right edge and come back to
    /// the strip as it began; on the left two thirds, at `max-column-width`, a grow does nothing.
    #[test]
    fn the_resize_keys_grow_and_shrink_about_the_same_place() {
        let (mut system, layout, w) = kai_s_two_thirds_and_a_third();
        let before = kai_s_frames(&mut system, layout);
        for amount in [0.05, 0.05, -0.05] {
            let frames = resize_key(&mut system, layout, amount);
            assert!(
                (right_edge(&frames, w[1]) - 1725.0).abs() <= 1.0,
                "{amount}: {frames:?}"
            );
            assert!(xw_in(&frames, w[1]).1 > 572.0, "{amount}");
        }
        assert_same_frames(&resize_key(&mut system, layout, -0.05), &before);

        assert!(system.select_window(layout, w[0]));
        let left = kai_s_frames(&mut system, layout);
        assert_same_frames(&resize_key(&mut system, layout, 0.05), &left);
    }

    /// The app answering each write keeps the place, so the off-centre column comes back where it
    /// stood; an app's own width moves the column's right edge and drops the place, and the next
    /// key takes it from there.
    #[test]
    fn only_an_app_s_own_width_drops_the_place() {
        let (mut system, layout, w) = kai_s_widths(&[0.5, 0.33333, 0.5, 0.33333], 1);
        let before = kai_s_frames(&mut system, layout);
        for _ in 0..3 {
            let width = xw_in(&mod_r(&mut system, layout), w[1]).1;
            app_sizes(&mut system, layout, w[1], width);
        }
        assert_same_frames(&mod_r(&mut system, layout), &before);

        let (mut system, layout, w) = kai_s_three_thirds();
        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (434.0, 860.0));
        app_sizes(&mut system, layout, w[1], 800.0);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (434.0, 800.0));

        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (406.0, 860.0));
    }

    /// At `Mod+R`'s 100% rini holds the column to the viewport and takes no width from its app, so
    /// an app's 800pt there keeps the place and the cycle comes back where the column stood.
    #[test]
    fn an_app_s_width_on_a_column_held_to_the_viewport_keeps_the_place() {
        let (mut system, layout, w) = kai_s_widths(&[0.5, 0.33333, 0.5, 0.33333], 1);
        let before = kai_s_frames(&mut system, layout);
        for _ in 0..3 {
            mod_r(&mut system, layout);
        }
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (3.0, 1722.0));
        app_sizes(&mut system, layout, w[1], 800.0);
        assert!(kept_place(&system, layout).is_some());

        assert_same_frames(&mod_r(&mut system, layout), &before);
    }

    /// rini takes no height from a column's only window, so its app answering every width with a
    /// height of its own keeps the place through the cycle.
    #[test]
    fn an_app_s_height_on_a_column_s_only_window_keeps_the_place() {
        let (mut system, layout, w) = kai_s_widths(&[0.5, 0.33333, 0.5, 0.33333], 1);
        let before = kai_s_frames(&mut system, layout);
        for _ in 0..3 {
            let width = xw_in(&mod_r(&mut system, layout), w[1]).1;
            let report = CGRect::new(CGPoint::new(0.0, 3.0), CGSize::new(width, 1061.0));
            system.on_window_resized(layout, resized(w[1], report), KAI_SCREEN, &kai_s_gaps());
            kai_s_frames(&mut system, layout);
            assert!(kept_place(&system, layout).is_some(), "{width}");
        }

        assert_same_frames(&mod_r(&mut system, layout), &before);
    }

    /// Focus moved to the middle third with no pass since: there is nowhere it stood to keep, and
    /// the key reveals it as before.
    #[test]
    fn a_key_before_any_pass_had_the_window_selected_reveals_it() {
        let (mut system, layout, w) = kai_s_three_thirds();
        assert!(system.select_window(layout, w[0]));
        kai_s_frames(&mut system, layout);
        assert!(system.select_window(layout, w[1]));

        assert_eq!(xw_in(&mod_r(&mut system, layout), w[1]), (578.0, 860.0));
    }

    /// Under `"always"` the centring places the column at every width, as before: the first flush
    /// left, the middle centred, the last flush right.
    #[test]
    fn always_places_a_resized_column_by_the_centring() {
        for col in 0..3 {
            let (mut system, layout, w) = kai_s_centred_strip();
            assert!(system.select_window(layout, w[col]));
            kai_s_frames(&mut system, layout);
            for step in 0..5 {
                let frames = if step < 4 {
                    mod_r(&mut system, layout)
                } else {
                    resize_key(&mut system, layout, 0.05)
                };
                let (x, width) = xw_in(&frames, w[col]);
                let case = format!("column {col}, step {step}: {x}, {width}");
                assert_eq!(kept_place(&system, layout), None, "{case}");
                match col {
                    0 => assert_eq!(x, 3.0, "{case}"),
                    1 => assert!((x + width / 2.0 - 864.0).abs() <= 1.0, "{case}"),
                    _ => assert!((x + width - 1725.0).abs() <= 1.0, "{case}"),
                }
            }
        }
    }

    // ── the place full width returns to ─────────────────────────────────────

    fn kai_s_frames(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
    ) -> Vec<(WindowId, CGRect)> {
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

    /// `Mod+F` on `wid` twice, with the pass after each.
    fn maximize_and_restore(system: &mut ScrollingLayoutSystem, layout: LayoutId, wid: WindowId) {
        assert!(system.toggle_full_width(layout, wid));
        kai_s_frames(system, layout);
        assert!(system.toggle_full_width(layout, wid));
    }

    /// Z a half, A and B a third on a 1728pt display, the strip reached by walking focus right and
    /// back, so A sits at x=578 with the strip at 287.48852 (`specs/strip.md`, "Full width and
    /// height").
    fn kai_s_thirds() -> (ScrollingLayoutSystem, LayoutId, [WindowId; 3]) {
        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let w = [wid(1, 1), wid(1, 2), wid(1, 3)];
        for window in w {
            system.add_window_after_selection(layout, window);
            kai_s_frames(&mut system, layout);
        }
        system.set_carried_width(layout, w[1], ColumnWidth::Offset(0.33333 - 0.5));
        system.set_carried_width(layout, w[2], ColumnWidth::Offset(0.33333 - 0.5));
        assert!(system.select_window(layout, w[0]));
        kai_s_frames(&mut system, layout);
        for direction in [Direction::Right, Direction::Right, Direction::Left] {
            system.move_focus(layout, direction);
            kai_s_frames(&mut system, layout);
        }
        assert_eq!(system.selected_window(layout), Some(w[1]));
        assert_near(system.scroll_offset(layout), 287.48852, "offset");
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (578.0, 572.0));
        (system, layout, w)
    }

    #[test]
    fn restoring_from_full_width_puts_the_strip_back_where_it_was() {
        let (mut system, layout, w) = kai_s_thirds();
        let before = kai_s_frames(&mut system, layout);

        assert!(system.toggle_full_width(layout, w[1]));
        assert_eq!(
            laid_out_xw(&mut system, layout, w[1]),
            (3.0, 1722.0),
            "the strip scrolled"
        );
        assert!(system.toggle_full_width(layout, w[1]));

        assert_same_frames(&kai_s_frames(&mut system, layout), &before);
        assert_near(system.scroll_offset(layout), 287.48852, "offset");
    }

    /// Two halves, B on the right: full width scrolls A off screen, and the restore brings it back.
    #[test]
    fn a_right_column_restores_with_its_left_neighbour_on_screen() {
        let (mut system, layout, w1, w2) = setup_two_windows(preset_settings());
        assert!(system.select_window(layout, w2));
        let before = kai_s_frames(&mut system, layout);
        assert_eq!(laid_out_xw(&mut system, layout, w2), (866.0, 860.0));

        maximize_and_restore(&mut system, layout, w2);

        assert_same_frames(&kai_s_frames(&mut system, layout), &before);
        assert_eq!(laid_out_xw(&mut system, layout, w1), (3.0, 860.0));
    }

    /// Each row of [A][B1,B2] and [A][B1,B2,B3]: full width puts the window in the stack's place,
    /// one column along, and the restore folds it back with the strip where it was.
    #[test]
    fn a_right_stack_restores_with_its_left_neighbour_on_screen() {
        for rows in [2, 3] {
            for row in 0..rows as usize {
                let (mut system, layout, a, b) = a_stack_on_the_right(rows);
                assert!(system.select_window(layout, b[row]));
                let before = kai_s_frames(&mut system, layout);
                let case = format!("row {row} of {rows}");

                assert!(system.toggle_full_width(layout, b[row]));
                kai_s_frames(&mut system, layout);
                let mut rest = b.clone();
                rest.remove(row);
                assert_eq!(
                    shape(&mut system, layout),
                    vec![vec![a], vec![b[row]], rest],
                    "{case}"
                );
                assert_near(system.scroll_offset(layout), 862.5, &case);
                assert!(system.toggle_full_width(layout, b[row]));

                assert_same_frames(&kai_s_frames(&mut system, layout), &before);
                assert_eq!(shape(&mut system, layout), vec![vec![a], b.clone()], "{case}");
            }
        }
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
        let before = kai_s_frames(&mut system, layout);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (434.0, 860.0));

        maximize_and_restore(&mut system, layout, w[1]);

        assert_same_frames(&kai_s_frames(&mut system, layout), &before);
    }

    /// The window's own focus event, which the raise after the toggle sends.
    #[test]
    fn refocusing_the_full_width_window_keeps_the_place_it_returns_to() {
        let (mut system, layout, w) = kai_s_thirds();
        let before = kai_s_frames(&mut system, layout);

        assert!(system.toggle_full_width(layout, w[1]));
        kai_s_frames(&mut system, layout);
        assert!(system.select_window(layout, w[1]));
        kai_s_frames(&mut system, layout);
        assert!(system.toggle_full_width(layout, w[1]));

        assert_same_frames(&kai_s_frames(&mut system, layout), &before);
    }

    #[test]
    fn a_focus_change_while_full_width_restores_by_the_reveal() {
        let (mut system, layout, w) = kai_s_thirds();

        assert!(system.toggle_full_width(layout, w[1]));
        kai_s_frames(&mut system, layout);
        for window in [w[0], w[1]] {
            assert!(system.select_window(layout, window));
            kai_s_frames(&mut system, layout);
        }
        assert!(system.toggle_full_width(layout, w[1]));

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (3.0, 572.0));
    }

    #[test]
    fn a_focus_moved_by_key_while_full_width_restores_by_the_reveal() {
        let (mut system, layout, w) = kai_s_thirds();

        assert!(system.toggle_full_width(layout, w[1]));
        kai_s_frames(&mut system, layout);
        for direction in [Direction::Left, Direction::Right] {
            system.move_focus(layout, direction);
            kai_s_frames(&mut system, layout);
        }
        assert!(system.toggle_full_width(layout, w[1]));

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (3.0, 572.0));
    }

    #[test]
    fn a_centring_while_full_width_restores_by_the_reveal() {
        let (mut system, layout, w) = kai_s_thirds();

        assert!(system.toggle_full_width(layout, w[1]));
        kai_s_frames(&mut system, layout);
        system.center_selected_column(layout);
        kai_s_frames(&mut system, layout);
        assert!(system.toggle_full_width(layout, w[1]));

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (3.0, 572.0));
    }

    #[test]
    fn a_strip_snap_while_full_width_restores_by_the_reveal() {
        let (mut system, layout, w) = kai_s_thirds();

        assert!(system.toggle_full_width(layout, w[1]));
        kai_s_frames(&mut system, layout);
        system.snap_to_nearest_column(layout);
        kai_s_frames(&mut system, layout);
        assert!(system.toggle_full_width(layout, w[1]));

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (3.0, 572.0));
    }

    /// B full width and C closing: the close scrolls nothing, and still the way back reveals.
    #[test]
    fn a_window_closing_while_full_width_restores_by_the_reveal() {
        let (mut system, layout, w) = kai_s_thirds();

        assert!(system.toggle_full_width(layout, w[1]));
        kai_s_frames(&mut system, layout);
        system.remove_window(w[2]);
        kai_s_frames(&mut system, layout);
        assert!(system.toggle_full_width(layout, w[1]));

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (3.0, 572.0));
    }

    /// A title-bar zoom of B while the last pass had A selected: no place of B's column is kept to
    /// go back to.
    #[test]
    fn a_toggle_on_a_window_the_last_pass_did_not_select_restores_by_the_reveal() {
        let (mut system, layout, w) = kai_s_thirds();

        maximize_and_restore(&mut system, layout, w[2]);

        assert_eq!(laid_out_xw(&mut system, layout, w[2]), (3.0, 572.0));
    }

    /// Z scrolled off screen and maximized: its place is taken where the reveal shows it, flush left,
    /// so the restore brings it back on screen there, and refocusing it keeps it.
    #[test]
    fn a_window_maximized_off_screen_comes_back_where_the_reveal_shows_it() {
        let (mut system, layout, w) = kai_s_thirds();
        assert!(system.select_window(layout, w[0]));
        kai_s_frames(&mut system, layout);
        system.scroll_strip_to(layout, 1296.0);
        kai_s_frames(&mut system, layout);
        let off_screen = laid_out_xw(&mut system, layout, w[0]);
        assert!(off_screen.0 + off_screen.1 < 0.0, "{off_screen:?}");

        maximize_and_restore(&mut system, layout, w[0]);
        assert_eq!(laid_out_xw(&mut system, layout, w[0]), (3.0, 860.0));
        assert!(system.select_window(layout, w[0]));

        assert_eq!(laid_out_xw(&mut system, layout, w[0]), (3.0, 860.0));
    }

    /// `Mod+F` as the reactor runs it, the raise's refocus after.
    fn mod_f(system: &mut ScrollingLayoutSystem, layout: LayoutId) -> Vec<(WindowId, CGRect)> {
        press(system, layout, |system, layout| {
            let selected = system.selected_window(layout).expect("a selection");
            assert!(system.toggle_full_width(layout, selected));
        })
    }

    /// Each column through full width and back lands where it stood, as it would stepping back to
    /// its width by `Mod+R` (`specs/strip.md`, "Full width and height").
    #[test]
    fn full_width_and_back_lands_a_column_where_it_stood() {
        type Setup = fn() -> (ScrollingLayoutSystem, LayoutId, Vec<WindowId>);
        let cases: [(&str, Setup, usize); 3] = [
            ("the right third", kai_s_two_thirds_and_a_third, 1),
            ("the left two thirds", kai_s_two_thirds_and_a_third, 0),
            ("the middle of three thirds", kai_s_three_thirds, 1),
        ];
        for (case, setup, col) in cases {
            let (mut system, layout, w) = setup();
            assert!(system.select_window(layout, w[col]));
            let before = kai_s_frames(&mut system, layout);

            assert_eq!(
                xw_in(&mod_f(&mut system, layout), w[col]),
                (3.0, 1722.0),
                "{case}"
            );
            assert_same_frames(&mod_f(&mut system, layout), &before);
        }
    }

    /// Kai's case with the right third a stack: each row goes full width in the stack's place and
    /// back into its row, the stack flush right again.
    #[test]
    fn a_stacked_window_back_from_full_width_lands_its_column_where_it_stood() {
        for row in 0..2 {
            let (mut system, layout, w) = kai_s_widths(&[0.66667, 0.33333, 0.33333], 2);
            system.toggle_fold_of_selection(layout, Direction::Left);
            assert!(system.select_window(layout, w[0]));
            kai_s_frames(&mut system, layout);
            let stack = vec![w[1], w[2]];
            assert!(system.select_window(layout, stack[row]));
            let before = kai_s_frames(&mut system, layout);
            assert_eq!(shape(&mut system, layout), vec![vec![w[0]], stack.clone()]);
            assert_eq!(
                laid_out_xw(&mut system, layout, stack[row]),
                (1153.0, 572.0),
                "row {row}"
            );

            let full = mod_f(&mut system, layout);
            assert_eq!(xw_in(&full, stack[row]), (3.0, 1722.0), "row {row}");
            let rest = vec![stack[1 - row]];
            assert_eq!(
                shape(&mut system, layout),
                vec![vec![w[0]], vec![stack[row]], rest],
                "row {row}"
            );

            assert_same_frames(&mod_f(&mut system, layout), &before);
            assert_eq!(shape(&mut system, layout), vec![vec![w[0]], stack], "row {row}");
        }
    }

    /// `Mod+R` on a full-width window steps from the whole viewport about the place `Mod+F` took:
    /// back at its width the column stands where it stood. The second of four unequal columns has
    /// three times the free space on its left as on its right, which the end-of-strip share of a
    /// viewport-wide column would not keep.
    #[test]
    fn mod_r_from_full_width_steps_about_the_place_full_width_took() {
        type Setup = fn() -> (ScrollingLayoutSystem, LayoutId, Vec<WindowId>);
        let off_centre: Setup = || kai_s_widths(&[0.5, 0.33333, 0.5, 0.33333], 1);
        let cases: [(&str, Setup, (f64, f64)); 2] = [
            (
                "Kai's right third",
                kai_s_two_thirds_and_a_third,
                (1153.0, 572.0),
            ),
            ("an off-centre third", off_centre, (866.0, 572.0)),
        ];
        for (case, setup, third) in cases {
            let (mut system, layout, w) = setup();
            let before = kai_s_frames(&mut system, layout);
            assert_eq!(xw_in(&before, w[1]), third, "{case}");

            assert_eq!(xw_in(&mod_f(&mut system, layout), w[1]), (3.0, 1722.0), "{case}");
            assert_same_frames(&mod_r(&mut system, layout), &before);
            let half = mod_r(&mut system, layout);
            let (x, width) = xw_in(&half, w[1]);
            assert_eq!(width, 860.0, "{case}");
            let (left0, right0) = (third.0 - 3.0, 1725.0 - third.0 - third.1);
            let kept = 3.0 + (1722.0 - width) * left0 / (left0 + right0);
            assert!((x - kept).abs() <= 1.0, "{case}: {x}, not {kept}");
        }
    }

    /// `Mod+F` and back after `Mod+R` stepped the column: it comes back at that width where the
    /// width keys kept it, and the next step goes on about the same place.
    #[test]
    fn full_width_after_a_width_key_returns_to_the_place_the_width_keys_keep() {
        let (mut system, layout, w) = kai_s_two_thirds_and_a_third();
        let half = mod_r(&mut system, layout);
        assert_eq!(xw_in(&half, w[1]), (866.0, 860.0));

        mod_f(&mut system, layout);
        assert_same_frames(&mod_f(&mut system, layout), &half);
        let two_thirds = mod_r(&mut system, layout);
        assert_eq!(xw_in(&two_thirds, w[1]).1, 1148.0);
        assert!(
            (right_edge(&two_thirds, w[1]) - 1725.0).abs() <= 1.0,
            "{two_thirds:?}"
        );
    }

    /// Under `"always"` the centring places the column both ways, as it does for a width key.
    #[test]
    fn always_brings_a_full_width_column_back_to_the_centring() {
        for col in 0..3 {
            let (mut system, layout, w) = kai_s_centred_strip();
            assert!(system.select_window(layout, w[col]));
            let before = kai_s_frames(&mut system, layout);

            assert_eq!(
                xw_in(&mod_f(&mut system, layout), w[col]),
                (3.0, 1722.0),
                "column {col}"
            );
            assert_eq!(kept_place(&system, layout), None, "column {col}");
            assert_same_frames(&mod_f(&mut system, layout), &before);
            assert_eq!(kept_place(&system, layout), None, "column {col}");
        }
    }

    // ── the view a fold returns to ──────────────────────────────────────────

    /// [A1][B1..] on Kai's display, two halves with the stack on the right, the strip at 0 with the
    /// stack at x=866.
    fn a_stack_on_the_right(
        rows: u32,
    ) -> (ScrollingLayoutSystem, LayoutId, WindowId, Vec<WindowId>) {
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
        kai_s_frames(&mut system, layout);
        assert_eq!(shape(&mut system, layout), vec![vec![a], b.clone()]);
        assert_eq!(system.scroll_offset(layout), 0.0);
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

    /// Either fold key, out of a right stack and back. `Mod+Comma` lands the window right of it and
    /// scrolls one column, and its fold back must not leave A1 at x=-860 with half the screen empty;
    /// `Mod+Period` lands it in the stack's place and scrolls nothing.
    #[test]
    fn a_fold_round_trip_puts_the_strip_back_where_it_was() {
        for rows in [2, 3] {
            for row in 0..rows as usize {
                for side in [Direction::Left, Direction::Right] {
                    let (mut system, layout, a, b) = a_stack_on_the_right(rows);
                    assert!(system.select_window(layout, b[row]));
                    let before = kai_s_frames(&mut system, layout);
                    let shape_before = shape(&mut system, layout);

                    fold_out_and_back(&mut system, layout, b[row], side);

                    let case = format!("row {row} of {rows}, {side:?}");
                    assert_eq!(shape(&mut system, layout), shape_before, "{case}");
                    assert_same_frames(&kai_s_frames(&mut system, layout), &before);
                    assert_eq!(laid_out_xw(&mut system, layout, a), (3.0, 860.0), "{case}");
                }
            }
        }
    }

    #[test]
    fn the_fold_out_of_a_right_stack_scrolls_the_strip() {
        let (mut system, layout, _a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[1]));
        kai_s_frames(&mut system, layout);

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&mut system, layout);

        assert_near(system.scroll_offset(layout), 862.5, "offset");
        assert_eq!(laid_out_xw(&mut system, layout, b[1]), (866.0, 860.0));
    }

    #[test]
    fn a_centred_stack_comes_back_centred_from_a_fold() {
        let (mut system, layout, _a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[1]));
        system.add_window_after_selection(layout, wid(1, 9));
        assert!(system.select_window(layout, b[1]));
        system.center_selected_column(layout);
        let before = kai_s_frames(&mut system, layout);
        assert_eq!(laid_out_xw(&mut system, layout, b[1]).0, 434.0);

        fold_out_and_back(&mut system, layout, b[1], Direction::Left);

        assert_same_frames(&kai_s_frames(&mut system, layout), &before);
    }

    #[test]
    fn refocusing_the_folded_out_window_keeps_the_view_it_returns_to() {
        let (mut system, layout, a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[1]));
        let before = kai_s_frames(&mut system, layout);

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&mut system, layout);
        assert!(system.select_window(layout, b[1]));
        kai_s_frames(&mut system, layout);
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_same_frames(&kai_s_frames(&mut system, layout), &before);
        assert_eq!(laid_out_xw(&mut system, layout, a), (3.0, 860.0));
    }

    /// [A][B1,B2][C][D] scrolled until the stack is off screen, B2 folded out and back there: once
    /// back, focusing it again shows it.
    #[test]
    fn refocusing_a_window_folded_back_off_screen_reveals_it() {
        let (mut system, layout, _a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[1]));
        for idx in [9, 10] {
            system.add_window_after_selection(layout, wid(1, idx));
        }
        assert!(system.select_window(layout, b[1]));
        kai_s_frames(&mut system, layout);
        system.scroll_strip_to(layout, 2592.0);
        kai_s_frames(&mut system, layout);
        let off_screen = laid_out_xw(&mut system, layout, b[1]);
        assert!(off_screen.0 + off_screen.1 < 0.0, "{off_screen:?}");

        fold_out_and_back(&mut system, layout, b[1], Direction::Left);
        assert_eq!(laid_out_xw(&mut system, layout, b[1]), off_screen);
        assert!(system.select_window(layout, b[1]));

        assert_eq!(laid_out_xw(&mut system, layout, b[1]).0, 3.0);
    }

    #[test]
    fn a_strip_snap_between_the_folds_folds_back_by_the_reveal() {
        let (mut system, layout, _a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[1]));
        kai_s_frames(&mut system, layout);

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&mut system, layout);
        system.snap_to_nearest_column(layout);
        kai_s_frames(&mut system, layout);
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(laid_out_xw(&mut system, layout, b[1]), (3.0, 860.0));
    }

    #[test]
    fn a_focus_moved_by_key_between_the_folds_folds_back_by_the_reveal() {
        let (mut system, layout, _a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[1]));
        kai_s_frames(&mut system, layout);

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&mut system, layout);
        for direction in [Direction::Left, Direction::Right] {
            system.move_focus(layout, direction);
            kai_s_frames(&mut system, layout);
        }
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(laid_out_xw(&mut system, layout, b[1]), (3.0, 860.0));
    }

    #[test]
    fn a_centring_between_the_folds_folds_back_by_the_reveal() {
        let (mut system, layout, _a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[1]));
        kai_s_frames(&mut system, layout);

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&mut system, layout);
        system.center_selected_column(layout);
        kai_s_frames(&mut system, layout);
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(laid_out_xw(&mut system, layout, b[1]), (3.0, 860.0));
    }

    #[test]
    fn a_width_key_between_the_folds_folds_back_by_the_reveal() {
        for key in ["Mod+R", "+5%"] {
            let (mut system, layout, _a, b) = a_stack_on_the_right(2);
            assert!(system.select_window(layout, b[1]));
            kai_s_frames(&mut system, layout);

            system.toggle_fold_of_selection(layout, Direction::Left);
            kai_s_frames(&mut system, layout);
            if key == "Mod+R" {
                mod_r(&mut system, layout);
            } else {
                resize_key(&mut system, layout, 0.05);
            }
            system.toggle_fold_of_selection(layout, Direction::Left);

            assert_eq!(laid_out_xw(&mut system, layout, b[1]), (3.0, 860.0), "{key}");
        }
    }

    /// Out by `Mod+Comma`, in by `Mod+Period`: the second key undoes nothing the first did, so the
    /// window joins the column on the right and the strip only scrolls as far as it must.
    #[test]
    fn folding_back_with_the_other_key_folds_back_by_the_reveal() {
        let (mut system, layout, a, b) = a_stack_on_the_right(2);
        let c = wid(1, 9);
        assert!(system.select_window(layout, b[1]));
        system.add_window_after_selection(layout, c);
        assert!(system.select_window(layout, b[1]));
        kai_s_frames(&mut system, layout);
        assert_eq!(system.scroll_offset(layout), 0.0);
        assert_eq!(shape(&mut system, layout), vec![vec![a], b.clone(), vec![c]]);

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&mut system, layout);
        system.toggle_fold_of_selection(layout, Direction::Right);

        assert_eq!(shape(&mut system, layout), vec![vec![a], vec![b[0]], vec![
            c, b[1]
        ]]);
        assert_eq!(laid_out_xw(&mut system, layout, b[1]), (866.0, 860.0));
    }

    // ── full width in the stack's place ─────────────────────────────────────

    /// [A1..][B] on Kai's display, two halves with the stack on the left and the strip at 0.
    fn a_stack_on_the_left(
        rows: u32,
    ) -> (ScrollingLayoutSystem, LayoutId, Vec<WindowId>, WindowId) {
        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let a: Vec<WindowId> = (1..1 + rows).map(|idx| wid(1, idx)).collect();
        let b = wid(1, 9);
        for &window in a.iter().chain(std::iter::once(&b)) {
            system.add_window_after_selection(layout, window);
        }
        for &window in &a[1..] {
            assert!(system.select_window(layout, window));
            system.toggle_fold_of_selection(layout, Direction::Left);
        }
        assert!(system.select_window(layout, a[0]));
        kai_s_frames(&mut system, layout);
        assert_eq!(shape(&mut system, layout), vec![a.clone(), vec![b]]);
        assert_eq!(system.scroll_offset(layout), 0.0);
        (system, layout, a, b)
    }

    /// Landing right of the stack panned the strip a column, 0 to 862.5, the rows leaving left and
    /// B right. In the stack's place nothing scrolls and everything else moves right of it.
    #[test]
    fn maximizing_in_a_left_stack_does_not_scroll_the_strip() {
        for rows in [2, 3] {
            for row in 0..rows as usize {
                let (mut system, layout, a, b) = a_stack_on_the_left(rows);
                assert!(system.select_window(layout, a[row]));
                let before = kai_s_frames(&mut system, layout);
                let case = format!("row {row} of {rows}");

                assert!(system.toggle_full_width(layout, a[row]));
                kai_s_frames(&mut system, layout);
                let mut rest = a.clone();
                rest.remove(row);
                assert_eq!(
                    shape(&mut system, layout),
                    vec![vec![a[row]], rest.clone(), vec![b]],
                    "{case}"
                );
                assert_eq!(system.scroll_offset(layout), 0.0, "{case}");
                assert_eq!(laid_out_xw(&mut system, layout, a[row]), (3.0, 1722.0), "{case}");
                for window in rest.into_iter().chain(std::iter::once(b)) {
                    assert!(
                        laid_out_xw(&mut system, layout, window).0 >= 1728.0,
                        "{case}: {window:?}"
                    );
                }
                assert!(system.toggle_full_width(layout, a[row]));

                assert_same_frames(&kai_s_frames(&mut system, layout), &before);
                assert_eq!(shape(&mut system, layout), vec![a.clone(), vec![b]], "{case}");
            }
        }
    }

    // ── folds, full width and the stacks they left ──────────────────────────

    /// A strip on Kai's display with these columns, windows `wid(1, n)` top to bottom, each stack
    /// made by folding its rows in from the right, the last window selected and laid out.
    fn a_strip(columns: &[&[u32]]) -> (ScrollingLayoutSystem, LayoutId) {
        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        for &idx in columns.iter().flat_map(|column| column.iter()) {
            system.add_window_after_selection(layout, wid(1, idx));
        }
        for column in columns {
            for &idx in &column[1..] {
                assert!(system.select_window(layout, wid(1, idx)));
                system.toggle_fold_of_selection(layout, Direction::Left);
            }
        }
        kai_s_frames(&mut system, layout);
        let want: Vec<Vec<WindowId>> = columns
            .iter()
            .map(|column| column.iter().map(|&idx| wid(1, idx)).collect())
            .collect();
        assert_eq!(shape(&mut system, layout), want, "fixture");
        (system, layout)
    }

    fn ids(columns: &[&[u32]]) -> Vec<Vec<WindowId>> {
        columns
            .iter()
            .map(|column| column.iter().map(|&idx| wid(1, idx)).collect())
            .collect()
    }

    /// No window is full width in a column it shares, and every window of a column is its width.
    fn assert_full_width_alone(system: &mut ScrollingLayoutSystem, layout: LayoutId) {
        let frames = kai_s_frames(system, layout);
        for column in shape(system, layout) {
            if column.len() > 1 {
                for &window in &column {
                    assert!(
                        !system.is_window_full_width(layout, window),
                        "{window:?} in {column:?}"
                    );
                }
            }
            let widths: Vec<f64> =
                column.iter().map(|&window| frame_for(&frames, window).size.width).collect();
            assert!(
                widths.iter().all(|&width| width == widths[0]),
                "{column:?} at {widths:?}"
            );
        }
    }

    /// [A1,A2][B1]: A2 folded out, then maximized and restored, stays the column the fold made.
    #[test]
    fn a_full_width_toggle_does_not_send_a_folded_out_window_back_into_its_stack() {
        let (mut system, layout) = a_strip(&[&[1, 2], &[3]]);
        assert!(system.select_window(layout, wid(1, 2)));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&mut system, layout), ids(&[&[1], &[2], &[3]]));

        maximize_and_restore(&mut system, layout, wid(1, 2));

        assert_eq!(shape(&mut system, layout), ids(&[&[1], &[2], &[3]]));
        assert!(!system.is_window_full_width(layout, wid(1, 2)));
    }

    /// [A1,A2][B1][C1]: A2 folded out and moved two columns right folds into C1, the column on its
    /// left, rather than jumping back past C1 and B1 into A1.
    #[test]
    fn a_fold_in_returns_to_its_row_only_from_the_neighbour_on_the_key_s_side() {
        let (mut system, layout) = a_strip(&[&[1, 2], &[3], &[4]]);
        assert!(system.select_window(layout, wid(1, 2)));
        system.toggle_fold_of_selection(layout, Direction::Left);
        for _ in 0..2 {
            assert!(system.move_selection(layout, Direction::Right));
        }
        assert_eq!(shape(&mut system, layout), ids(&[&[1], &[3], &[4], &[2]]));

        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(shape(&mut system, layout), ids(&[&[1], &[3], &[4, 2]]));
    }

    /// [Z][A1,A2][B1] with A2 full width: either fold key ends full width where it is and folds A2
    /// into the neighbour on its side as an ordinary column.
    #[test]
    fn a_fold_on_a_full_width_window_ends_full_width_and_folds_it() {
        for (side, want) in [
            (Direction::Left, ids(&[&[1, 3], &[2], &[4]])),
            (Direction::Right, ids(&[&[1], &[2, 3], &[4]])),
        ] {
            let (mut system, layout) = a_strip(&[&[1], &[2, 3], &[4]]);
            assert!(system.toggle_full_width(layout, wid(1, 3)));
            kai_s_frames(&mut system, layout);
            assert_eq!(shape(&mut system, layout), ids(&[&[1], &[3], &[2], &[4]]));

            assert_eq!(system.toggle_fold_of_selection(layout, side), vec![wid(1, 3)]);

            assert_eq!(shape(&mut system, layout), want, "{side:?}");
            assert_full_width_alone(&mut system, layout);
            let state = system.layout_state(layout).expect("layout");
            assert!(!state.full_width_origins.contains_key(&wid(1, 3)), "{side:?}");
        }
    }

    /// [A1,A2] with A2 full width: nothing is on its left, so the left key leaves it full width with
    /// its way back into A1's stack.
    #[test]
    fn a_fold_with_no_column_on_that_side_leaves_a_full_width_window_as_it_is() {
        let (mut system, layout) = a_strip(&[&[1, 2]]);
        assert!(system.toggle_full_width(layout, wid(1, 2)));
        assert_eq!(shape(&mut system, layout), ids(&[&[2], &[1]]));

        assert!(system.toggle_fold_of_selection(layout, Direction::Left).is_empty());
        assert_eq!(shape(&mut system, layout), ids(&[&[2], &[1]]));
        assert!(system.is_window_full_width(layout, wid(1, 2)));

        assert!(system.toggle_full_width(layout, wid(1, 2)));
        assert_eq!(shape(&mut system, layout), ids(&[&[1, 2]]));
    }

    /// B1 folded into a full-width A1, by the end of its column or back into the row it was folded
    /// out of: A1's full width ends where it is and both are the column's width.
    #[test]
    fn a_window_folded_into_a_full_width_column_ends_its_full_width() {
        let (mut system, layout) = a_strip(&[&[1], &[2]]);
        assert!(system.toggle_full_width(layout, wid(1, 1)));
        assert!(system.select_window(layout, wid(1, 2)));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&mut system, layout), ids(&[&[1, 2]]));
        assert_full_width_alone(&mut system, layout);

        let (mut system, layout) = a_strip(&[&[1, 2]]);
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert!(system.toggle_full_width(layout, wid(1, 1)));
        assert!(system.select_window(layout, wid(1, 2)));
        system.toggle_fold_of_selection(layout, Direction::Left);
        assert_eq!(shape(&mut system, layout), ids(&[&[1, 2]]));
        assert_full_width_alone(&mut system, layout);
    }

    /// [A][B1,B2] with A full width, swapped with B1 either way round.
    #[test]
    fn a_swap_into_a_shared_column_ends_full_width() {
        for swapped in [(1, 2), (2, 1)] {
            let (mut system, layout) = a_strip(&[&[1], &[2, 3]]);
            assert!(system.toggle_full_width(layout, wid(1, 1)));
            assert!(system.swap_windows(layout, wid(1, swapped.0), wid(1, swapped.1)));
            assert_eq!(shape(&mut system, layout), ids(&[&[2], &[1, 3]]), "{swapped:?}");
            assert_full_width_alone(&mut system, layout);
        }
    }

    /// [1][2,3,4][5] with 1 full width and 4 folded out to the left: a swap of 1 into the stack ends
    /// its full width as a width given does, and so the fold back reveals 4 rather than returning
    /// the view the fold out kept.
    #[test]
    fn a_swap_ending_full_width_drops_the_view_a_fold_back_returns_to() {
        let (mut system, layout) = a_strip(&[&[1], &[2, 3, 4], &[5]]);
        assert!(system.toggle_full_width(layout, wid(1, 1)));
        kai_s_frames(&mut system, layout);
        assert!(system.select_window(layout, wid(1, 4)));
        kai_s_frames(&mut system, layout);
        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&mut system, layout);

        assert!(system.swap_windows(layout, wid(1, 1), wid(1, 2)));
        kai_s_frames(&mut system, layout);
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(laid_out_xw(&mut system, layout, wid(1, 4)), (3.0, 860.0));
    }

    /// B1 out of a right stack with `Mod+Comma`, `Mod+Period` from the last column does nothing,
    /// and `Mod+Comma` still folds it back on top with the strip where it was.
    #[test]
    fn a_fold_that_does_nothing_keeps_the_row_and_the_view_the_fold_back_returns_to() {
        let (mut system, layout, a, b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[0]));
        let before = kai_s_frames(&mut system, layout);

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&mut system, layout);
        assert!(system.toggle_fold_of_selection(layout, Direction::Right).is_empty());
        kai_s_frames(&mut system, layout);
        system.toggle_fold_of_selection(layout, Direction::Left);

        assert_eq!(shape(&mut system, layout), vec![vec![a], b.clone()]);
        assert_same_frames(&kai_s_frames(&mut system, layout), &before);
    }

    /// [Z][A][B1,B2][C], B2 out by `Mod+Comma` and swapped with A by a drag, which scrolls nothing,
    /// then in by `Mod+Period` into B1's row: the stack's view was the other key's to return to, so
    /// the strip scrolls only as far as it must and the stack lands at x=3, not back at x=866.
    #[test]
    fn folding_back_into_the_row_with_the_other_key_folds_back_by_the_reveal() {
        let (mut system, layout) = a_strip(&[&[1], &[2], &[3, 4], &[5]]);
        for window in [wid(1, 1), wid(1, 4)] {
            assert!(system.select_window(layout, window));
            kai_s_frames(&mut system, layout);
        }
        assert_eq!(laid_out_xw(&mut system, layout, wid(1, 4)), (866.0, 860.0));

        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&mut system, layout);
        assert!(system.swap_windows(layout, wid(1, 4), wid(1, 2)));
        kai_s_frames(&mut system, layout);
        system.toggle_fold_of_selection(layout, Direction::Right);

        assert_eq!(shape(&mut system, layout), ids(&[&[1], &[3, 4], &[2], &[5]]));
        assert_eq!(laid_out_xw(&mut system, layout, wid(1, 4)), (3.0, 860.0));
    }

    // ── a window's id replaced while away ───────────────────────────────────

    /// B1 out of [A][B1,B2] by `Mod+Comma` and back, with `renamed` given a new id in between: B1
    /// goes back on top and the strip back to 0.
    fn fold_round_trip_renaming(
        renamed: usize,
    ) -> (ScrollingLayoutSystem, LayoutId, Vec<WindowId>) {
        let (mut system, layout, a, mut b) = a_stack_on_the_right(2);
        assert!(system.select_window(layout, b[0]));
        kai_s_frames(&mut system, layout);
        system.toggle_fold_of_selection(layout, Direction::Left);
        kai_s_frames(&mut system, layout);
        let new = wid(2, 1);
        system.replace_window(b[renamed], new);
        b[renamed] = new;
        assert_eq!(system.toggle_fold_of_selection(layout, Direction::Left), vec![
            b[0]
        ]);
        assert_eq!(shape(&mut system, layout), vec![vec![a], b.clone()]);
        assert_eq!(laid_out_xw(&mut system, layout, a), (3.0, 860.0));
        (system, layout, b)
    }

    #[test]
    fn a_folded_out_window_given_a_new_id_folds_back_into_its_row() { fold_round_trip_renaming(0); }

    #[test]
    fn a_fold_back_finds_its_row_when_the_window_it_sat_above_has_a_new_id() {
        fold_round_trip_renaming(1);
    }

    /// The off-centre column of four, given a new id at the whole viewport, where its share comes
    /// from the place kept and not from the pass: the third again stands where it began.
    #[test]
    fn a_column_given_a_new_id_keeps_its_place() {
        let (mut system, layout, w) = kai_s_widths(&[0.5, 0.33333, 0.5, 0.33333], 1);
        let before = laid_out_xw(&mut system, layout, w[1]);
        for _ in 0..3 {
            mod_r(&mut system, layout);
        }
        let new = wid(2, 1);
        system.replace_window(w[1], new);

        assert_eq!(xw_in(&mod_r(&mut system, layout), new), before);
    }

    /// A on Kai's thirds, given a new id while full width, or between the toggle back and its pass.
    #[test]
    fn a_full_width_window_given_a_new_id_restores_to_the_place_it_left() {
        for before_the_pass in [false, true] {
            let (mut system, layout, w) = kai_s_thirds();
            let new = wid(2, 1);
            assert!(system.toggle_full_width(layout, w[1]));
            kai_s_frames(&mut system, layout);
            if before_the_pass {
                assert!(system.toggle_full_width(layout, w[1]));
                system.replace_window(w[1], new);
            } else {
                system.replace_window(w[1], new);
                assert!(system.toggle_full_width(layout, new));
            }

            assert_eq!(
                laid_out_xw(&mut system, layout, new),
                (578.0, 572.0),
                "{before_the_pass}"
            );
        }
    }

    /// A workspace meeting a display size it has no layout for takes a copy of its last one: a
    /// full-width window in the copy restores to the place it left in the original.
    #[test]
    fn a_full_width_window_in_a_cloned_layout_restores_to_the_place_it_left() {
        let (mut system, layout, w) = kai_s_thirds();
        assert!(system.toggle_full_width(layout, w[1]));
        kai_s_frames(&mut system, layout);
        let cloned = system.clone_layout(layout);

        assert!(system.toggle_full_width(cloned, w[1]));
        assert_eq!(laid_out_xw(&mut system, cloned, w[1]), (578.0, 572.0));
    }

    /// A layout file from before the two maps holds one `stack_origins`: it loads, its strip intact
    /// and the old origins dropped.
    #[test]
    fn a_layout_saved_with_the_shared_origins_still_loads() {
        let (mut system, layout) = a_strip(&[&[1, 2], &[3]]);
        assert!(system.select_window(layout, wid(1, 2)));
        system.toggle_fold_of_selection(layout, Direction::Left);
        let state = system.layout_state(layout).expect("layout");
        let saved = ron::ser::to_string(state).expect("serialize");
        assert!(saved.contains("full_width_origins:{},fold_origins:{"), "{saved}");
        let old = saved
            .replace("full_width_origins:{},", "")
            .replace("fold_origins:", "stack_origins:");

        let loaded: super::LayoutState = ron::from_str(&old).expect("a file from before loads");

        assert_eq!(loaded.columns.len(), 3);
        assert!(loaded.fold_origins.is_empty());
        assert!(loaded.full_width_origins.is_empty());
    }

    // ── the app answering rini's write ──────────────────────────────────────

    /// The app reporting `window`'s frame from the pass before, `short` narrower and lower, then a
    /// pass.
    fn answer(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        window: WindowId,
        short: (f64, f64),
    ) {
        answer_under(system, layout, window, short, &constraints_none());
    }

    /// `answer`, with every pass laid out under the windows' own `limits`, which the app holds its
    /// answer to as well.
    fn answer_under(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        window: WindowId,
        short: (f64, f64),
        limits: &HashMap<WindowId, WindowLayoutConstraints>,
    ) {
        let frame = frame_for(&kai_s_frames_under(system, layout, limits), window);
        let own = limits.get(&window).copied().unwrap_or_default();
        let size = CGSize::new(
            (frame.size.width - short.0).max(own.min_width),
            (frame.size.height - short.1).max(own.min_height),
        );
        let report = CGRect::new(frame.origin, size);
        system.on_window_resized(layout, resized(window, report), KAI_SCREEN, &kai_s_gaps());
        kai_s_frames_under(system, layout, limits);
    }

    fn kai_s_frames_under(
        system: &mut ScrollingLayoutSystem,
        layout: LayoutId,
        limits: &HashMap<WindowId, WindowLayoutConstraints>,
    ) -> Vec<(WindowId, CGRect)> {
        system.calculate_layout(
            layout,
            KAI_SCREEN,
            limits,
            &tile::Borders::default(),
            &kai_s_gaps(),
        )
    }

    /// `window` at least 900pt wide, more than a half column's 860.
    fn at_least_900_wide(window: WindowId) -> HashMap<WindowId, WindowLayoutConstraints> {
        let mut limits = constraints_none();
        let wide = WindowLayoutConstraints {
            is_resizable: true,
            min_width: 900.0,
            ..Default::default()
        };
        limits.insert(window, wide);
        limits
    }

    fn assert_no_size_given(system: &mut ScrollingLayoutSystem, layout: LayoutId) {
        for column in &system.layout_state(layout).expect("layout").columns {
            assert_eq!(column.width, None, "{column:?}");
            assert!(!column.height_overridden, "{column:?}");
        }
    }

    /// A1 of [A1,A2,A3] and B of [A][B]. Ghostty's 1pt short in height must not come back as a width
    /// of 859, nor the stack's even split as pixel heights of [367, 369, 369].
    #[test]
    fn a_report_of_the_frame_rini_gave_changes_nothing() {
        let shortfalls = [(0.0, 0.0), (0.0, 1.0), (1.0, 0.0), (1.0, 1.0), (-1.0, -1.0)];
        for (columns, window) in [
            (&[&[1, 2, 3][..]][..], wid(1, 1)),
            (&[&[1][..], &[2][..]][..], wid(1, 2)),
        ] {
            for short in shortfalls {
                let (mut system, layout) = a_strip(columns);
                assert!(system.select_window(layout, window));
                let before = kai_s_frames(&mut system, layout);
                let offset = system.scroll_offset(layout);

                answer(&mut system, layout, window, short);

                assert_same_frames(&kai_s_frames(&mut system, layout), &before);
                assert_eq!(system.scroll_offset(layout), offset, "{columns:?} {short:?}");
                assert_no_size_given(&mut system, layout);
            }
        }
    }

    #[test]
    fn an_axis_the_app_answered_stays_while_the_other_is_taken() {
        let (mut system, layout) = a_strip(&[&[1, 2, 3]]);
        assert!(system.select_window(layout, wid(1, 1)));

        answer(&mut system, layout, wid(1, 1), (1.0, 50.0));

        let column = &system.layout_state(layout).expect("layout").columns[0];
        assert_eq!(column.width, None);
        assert!(column.height_overridden);
        assert_eq!(laid_out_xw(&mut system, layout, wid(1, 1)), (3.0, 860.0));
        assert_eq!(laid_out_yh(&mut system, layout, wid(1, 1)), (3.0, 318.0));
    }

    /// Each row of [A][B1,B2] and [A][B1,B2,B3] folded out and back ten times by either key, the
    /// moved window answering every pass with its frame or 1pt short of it. The answer to the
    /// fold-out must keep the view the fold back returns to, and no answer narrows the column by a
    /// point or turns the even split into pixel heights.
    #[test]
    fn ten_fold_round_trips_answered_by_the_app_leave_the_stack_as_it_was() {
        for rows in [2, 3] {
            for row in 0..rows as usize {
                for side in [Direction::Left, Direction::Right] {
                    for short in [(0.0, 0.0), (0.0, 1.0)] {
                        let (mut system, layout, a, b) = a_stack_on_the_right(rows);
                        assert!(system.select_window(layout, b[row]));
                        let before = kai_s_frames(&mut system, layout);
                        let shape_before = shape(&mut system, layout);

                        for _ in 0..10 {
                            for _ in 0..2 {
                                assert_eq!(system.toggle_fold_of_selection(layout, side), vec![
                                    b[row]
                                ]);
                                answer(&mut system, layout, b[row], short);
                            }
                        }

                        let case = format!("row {row} of {rows}, {side:?}, {short:?}");
                        assert_eq!(shape(&mut system, layout), shape_before, "{case}");
                        assert_same_frames(&kai_s_frames(&mut system, layout), &before);
                        assert_eq!(laid_out_xw(&mut system, layout, a), (3.0, 860.0), "{case}");
                        assert_no_size_given(&mut system, layout);
                    }
                }
            }
        }
    }

    #[test]
    fn a_width_taken_leaves_the_height_the_app_answered() {
        let (mut system, layout) = a_strip(&[&[1, 2, 3]]);
        assert!(system.select_window(layout, wid(1, 1)));
        let rows = laid_out_yh(&mut system, layout, wid(1, 1));

        answer(&mut system, layout, wid(1, 1), (50.0, 1.0));

        let column = &system.layout_state(layout).expect("layout").columns[0];
        assert!(column.width.is_some());
        assert!(!column.height_overridden);
        assert_eq!(laid_out_xw(&mut system, layout, wid(1, 1)), (3.0, 810.0));
        assert_eq!(laid_out_yh(&mut system, layout, wid(1, 1)), rows);
    }

    /// B of [A][B][C], centred and held to 900pt by its own minimum, given 899 and answering with
    /// its 900: the slot was counted without the minimum, 860, so the answer was taken as a width
    /// and its scroll request dropped the centring.
    #[test]
    fn a_window_held_to_its_minimum_answers_by_it_and_changes_nothing() {
        let (mut system, layout) = a_strip(&[&[1], &[2], &[3]]);
        let limits = at_least_900_wide(wid(1, 2));
        assert!(system.select_window(layout, wid(1, 2)));
        system.center_selected_column(layout);
        let before = kai_s_frames_under(&mut system, layout, &limits);
        assert_eq!(frame_for(&before, wid(1, 2)).size.width, 899.0);

        answer_under(&mut system, layout, wid(1, 2), (0.0, 0.0), &limits);

        assert_same_frames(&kai_s_frames_under(&mut system, layout, &limits), &before);
        assert_no_size_given(&mut system, layout);
    }

    /// [A][B1,B2] with B2 held to 900pt, folded out and back by `Mod+Comma`, answering each pass:
    /// A1 came back at x=-860 rather than the -36 it left.
    #[test]
    fn a_fold_round_trip_of_a_window_held_to_its_minimum_puts_the_strip_back() {
        let (mut system, layout, a, b) = a_stack_on_the_right(2);
        let limits = at_least_900_wide(b[1]);
        assert!(system.select_window(layout, b[1]));
        let before = kai_s_frames_under(&mut system, layout, &limits);

        for _ in 0..2 {
            assert_eq!(system.toggle_fold_of_selection(layout, Direction::Left), vec![
                b[1]
            ]);
            answer_under(&mut system, layout, b[1], (0.0, 0.0), &limits);
        }

        assert_eq!(shape(&mut system, layout), vec![vec![a], b.clone()]);
        assert_same_frames(&kai_s_frames_under(&mut system, layout, &limits), &before);
    }

    /// B of [A][B][C] centred, maximized and restored, then the app's answer to the restore: the
    /// answer's scroll request keeps the centring, rather than sending B flush left.
    #[test]
    fn a_centred_column_keeps_its_centring_through_its_own_report() {
        let mut system = ScrollingLayoutSystem::new(&preset_settings());
        let layout = system.create_layout();
        let w = [wid(1, 1), wid(1, 2), wid(1, 3)];
        for window in w {
            system.add_window_after_selection(layout, window);
        }
        assert!(system.select_window(layout, w[1]));
        system.center_selected_column(layout);
        let before = kai_s_frames(&mut system, layout);
        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (434.0, 860.0));

        maximize_and_restore(&mut system, layout, w[1]);
        answer(&mut system, layout, w[1], (0.0, 1.0));

        assert_same_frames(&kai_s_frames(&mut system, layout), &before);
    }

    /// A drag is the user's, not an answer: a point's drag of the right edge is a point wider.
    #[test]
    fn a_one_point_drag_still_gives_the_width() {
        let (mut system, layout, w) = kai_s_strip();

        drag(&mut system, layout, w[1], xw(600.0, 882.0), xw(600.0, 883.0));

        assert_eq!(laid_out_xw(&mut system, layout, w[1]), (600.0, 883.0));
    }

    /// A border inside each tiled window's slot: the strip is laid out from the slots as with no
    /// border, and every window comes back inset (`tile.rs`).
    mod borders {
        use super::*;
        use crate::layout::domain::tile::{self, window_in};

        const BORDER: f64 = 4.0;

        fn laid_out_with(
            system: &mut ScrollingLayoutSystem,
            layout: LayoutId,
            constraints: &HashMap<WindowId, WindowLayoutConstraints>,
            border: f64,
        ) -> Vec<(WindowId, CGRect)> {
            system.calculate_layout(
                layout,
                KAI_SCREEN,
                constraints,
                &tile::Borders::every(border),
                &kai_s_gaps(),
            )
        }

        fn bordered(system: &mut ScrollingLayoutSystem, layout: LayoutId, wid: WindowId) -> CGRect {
            frame_for(&laid_out_with(system, layout, &constraints_none(), BORDER), wid)
        }

        /// A window's report, `[x, w]` inside a full-height slot.
        fn window_xw(x: f64, width: f64) -> CGRect {
            window_in(xw(x - BORDER, width + 2.0 * BORDER), BORDER)
        }

        fn drag_bordered(
            system: &mut ScrollingLayoutSystem,
            layout: LayoutId,
            wid: WindowId,
            from: CGRect,
            to: CGRect,
        ) {
            let drag = Drag::report(from, to, None);
            system.on_window_resized(
                layout,
                ResizeReport {
                    drag: Some(drag),
                    border: BORDER,
                    ..resized(wid, to)
                },
                KAI_SCREEN,
                &kai_s_gaps(),
            );
        }

        #[test]
        fn a_border_takes_its_width_out_of_every_tiled_window() {
            let (mut system, layout, w) = kai_s_strip();
            let slots = render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());
            let windows = laid_out_with(&mut system, layout, &constraints_none(), BORDER);
            assert_eq!(windows.len(), slots.len());
            for (wid, slot) in slots {
                assert_eq!(frame_for(&windows, wid), window_in(slot, BORDER), "{wid:?}");
            }
            assert_eq!(
                bordered(&mut system, layout, w[1]),
                CGRect::new(CGPoint::new(604.0, 7.0), CGSize::new(874.0, 1103.0))
            );
            assert_eq!(system.scroll_offset(layout), 0.0, "the strip as it was");
        }

        #[test]
        fn a_resize_report_with_a_border_on_reads_the_same_ratio_as_without_a_border() {
            let (mut plain, layout, w) = kai_s_strip();
            let (mut with_border, other, _) = kai_s_strip();
            plain.on_window_resized(
                layout,
                resized(w[1], xw(600.0, 700.0)),
                KAI_SCREEN,
                &kai_s_gaps(),
            );
            with_border.on_window_resized(
                other,
                ResizeReport {
                    border: BORDER,
                    ..resized(w[1], window_xw(604.0, 692.0))
                },
                KAI_SCREEN,
                &kai_s_gaps(),
            );
            assert_eq!(
                plain.carried_width(layout, w[1]),
                with_border.carried_width(other, w[1])
            );
            let slot = frame_for(&render(&mut plain, layout, KAI_SCREEN, &kai_s_gaps()), w[1]);
            assert_eq!(bordered(&mut with_border, other, w[1]), window_in(slot, BORDER));
        }

        /// b1's `a_left_edge_shrink_keeps_the_right_edge_while_the_strip_can_scroll`, the window
        /// 4pt inside each of its slots.
        #[test]
        fn a_left_edge_drag_with_a_border_on_keeps_the_right_edge() {
            let (mut system, layout, w) = kai_s_strip();
            drag_bordered(
                &mut system,
                layout,
                w[1],
                window_xw(604.0, 874.0),
                window_xw(338.0, 1140.0),
            );
            assert_eq!(bordered(&mut system, layout, w[1]), window_xw(338.0, 1140.0));

            drag_bordered(
                &mut system,
                layout,
                w[1],
                window_xw(338.0, 1140.0),
                window_xw(438.0, 1040.0),
            );
            assert_eq!(
                bordered(&mut system, layout, w[1]),
                window_xw(438.0, 1040.0),
                "the right edge kept at 1478"
            );
            assert_near(system.scroll_offset(layout), 166.0, "offset");
        }

        /// 9338f580's dragged width, whole, with the border: a slot 813pt wide, which as a bounded
        /// column would give up 1.5pt of it to the gap.
        #[test]
        fn a_release_with_a_border_on_leaves_the_window_where_the_cursor_let_go() {
            let (mut system, layout, w) = kai_s_strip();
            drag_bordered(
                &mut system,
                layout,
                w[1],
                window_xw(604.0, 874.0),
                window_xw(604.0, 805.0),
            );
            assert_eq!(bordered(&mut system, layout, w[1]), window_xw(604.0, 805.0));
        }

        #[test]
        fn a_stacked_resize_with_a_border_on_splits_the_height_as_without() {
            let (mut plain, layout, w) = kai_s_stack();
            let (mut with_border, other, _) = kai_s_stack();
            assert!(plain.select_window(layout, w[1]) && with_border.select_window(other, w[1]));
            let from = at_yh(&mut plain, layout, w[1], 374.0, 368.0);
            let to = at_yh(&mut plain, layout, w[1], 274.0, 468.0);
            drag(&mut plain, layout, w[1], from, to);
            let reported = |slot: CGRect| window_in(slot, BORDER);
            let drag = Drag::report(reported(from), reported(to), None);
            with_border.on_window_resized(
                other,
                ResizeReport {
                    drag: Some(drag),
                    border: BORDER,
                    ..resized(w[1], reported(to))
                },
                KAI_SCREEN,
                &kai_s_gaps(),
            );

            let slots = render(&mut plain, layout, KAI_SCREEN, &kai_s_gaps());
            for window in w {
                assert_eq!(
                    bordered(&mut with_border, other, window),
                    window_in(frame_for(&slots, window), BORDER),
                    "{window:?}"
                );
            }
        }

        fn limited(
            wid: WindowId,
            limits: WindowLayoutConstraints,
        ) -> HashMap<WindowId, WindowLayoutConstraints> {
            let mut constraints = constraints_none();
            constraints.insert(wid, limits);
            constraints
        }

        /// Counted without the border, the row's slot would be 700pt and the window 692.
        #[test]
        fn a_stacked_windows_minimum_height_counts_its_border() {
            let (mut system, layout, w) = kai_s_stack();
            let tall = WindowLayoutConstraints {
                is_resizable: true,
                min_height: 700.0,
                ..Default::default()
            };
            let frames = laid_out_with(&mut system, layout, &limited(w[0], tall), BORDER);
            let top = frame_for(&frames, w[0]);
            assert_eq!((top.origin.y, top.size.height), (7.0, 700.0));
            assert_eq!(
                frame_for(&frames, w[1]).origin.y,
                3.0 + 708.0 + 3.0 + BORDER,
                "below the whole slot and the gap"
            );
        }

        /// A column as wide as its widest minimum, about 808pt with the border, holds a window that
        /// may be at most 500pt wide at its slot's left edge, inside the border.
        #[test]
        fn a_window_held_to_its_maximum_is_clamped_inside_its_slot() {
            let (mut system, layout, w) = kai_s_stack();
            let mut constraints = limited(w[0], WindowLayoutConstraints {
                is_resizable: true,
                max_width: 500.0,
                ..Default::default()
            });
            constraints.insert(w[1], WindowLayoutConstraints {
                is_resizable: true,
                min_width: 800.0,
                ..Default::default()
            });
            let frames = laid_out_with(&mut system, layout, &constraints, BORDER);
            let slot = tile::slot_of(frame_for(&frames, w[1]), BORDER);
            assert!(slot.size.width > 500.0 + 2.0 * BORDER, "{slot:?}");
            let narrow = frame_for(&frames, w[0]);
            assert_eq!(
                (narrow.origin.x, narrow.size.width),
                (slot.origin.x + BORDER, 500.0)
            );
        }

        /// A rule can give one window of a stack another border: each is laid inside its own slot by
        /// its own width, and the slots are the strip's as before.
        #[test]
        fn each_window_of_a_stack_is_inset_by_its_own_border() {
            let (mut system, layout, w) = kai_s_stack();
            let slots = render(&mut system, layout, KAI_SCREEN, &kai_s_gaps());
            let borders = tile::Borders {
                every: BORDER,
                windows: [(w[0], 0.0)].into_iter().collect(),
            };
            let frames = system.calculate_layout(
                layout,
                KAI_SCREEN,
                &constraints_none(),
                &borders,
                &kai_s_gaps(),
            );
            assert_eq!(
                frame_for(&frames, w[0]),
                frame_for(&slots, w[0]),
                "no border, the whole slot"
            );
            assert_eq!(
                frame_for(&frames, w[1]),
                window_in(frame_for(&slots, w[1]), BORDER)
            );
        }
    }
}
