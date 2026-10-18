//! How far the overview zooms out, and where its rows sit down the display.
//!
//! Every row is the usable frame at one depth. The depth fits every row into the usable height,
//! held between `DEEPEST` and `SHALLOWEST`; below `DEEPEST` the rows do not fit and the stack
//! scrolls the least it can to keep the selected row in view.

/// The smallest a row is drawn at. A thumb much smaller does not read.
pub const DEEPEST: f64 = 0.2;
/// The largest a row is drawn at, however few rows there are: niri's default overview zoom.
pub const SHALLOWEST: f64 = 0.5;
/// On screen between the stack and the usable frame's top and bottom.
pub const MARGIN: f64 = 24.0;
/// On screen between one row and the next.
pub const ROW_GAP: f64 = 24.0;

/// The depth `rows` rows of the usable frame fit `usable_height` at, between `DEEPEST` and
/// `SHALLOWEST`, with `MARGIN` above and below and `ROW_GAP` between.
pub fn depth(rows: usize, usable_height: f64) -> f64 {
    if rows == 0 {
        return SHALLOWEST;
    }
    let n = rows as f64;
    let fit = (usable_height - 2.0 * MARGIN - (n - 1.0) * ROW_GAP) / (n * usable_height);
    if !fit.is_finite() {
        return DEEPEST;
    }
    fit.clamp(DEEPEST, SHALLOWEST)
}

/// Where the rows of a stack go down the display: each row's top, and how far the stack is
/// scrolled.
#[derive(Debug, Clone, PartialEq)]
pub struct RowTops {
    pub tops: Vec<f64>,
    /// How far the stack is scrolled up from its first row at the top margin: 0 when it fits.
    pub scroll: f64,
}

/// The vertical extent the stack is laid out in: the usable frame's top and bottom on the overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub top: f64,
    pub bottom: f64,
}

/// The tops of `rows` rows `row_height` tall in `span`.
///
/// A stack that fits is centred. One that does not starts from `scroll`, the scroll it was last
/// laid out at, and moves it the least that brings the `selected` row inside the margins, never past
/// either end of the stack.
pub fn row_tops(rows: usize, row_height: f64, span: Span, scroll: f64, selected: usize) -> RowTops {
    if rows == 0 {
        return RowTops { tops: Vec::new(), scroll: 0.0 };
    }
    let pitch = row_height + ROW_GAP;
    let height = rows as f64 * pitch - ROW_GAP;
    let room = span.bottom - span.top - 2.0 * MARGIN;
    if height <= room + 1e-9 {
        let first = span.top + (span.bottom - span.top - height) / 2.0;
        return RowTops {
            tops: (0..rows).map(|row| first + row as f64 * pitch).collect(),
            scroll: 0.0,
        };
    }
    let most = height - room;
    let mut scroll = if scroll.is_finite() {
        scroll.clamp(0.0, most)
    } else {
        0.0
    };
    let first = span.top + MARGIN;
    let top = first + selected.min(rows - 1) as f64 * pitch - scroll;
    if top < first {
        scroll -= first - top;
    } else if top + row_height > span.bottom - MARGIN {
        scroll += top + row_height - (span.bottom - MARGIN);
    }
    RowTops {
        tops: (0..rows).map(|row| first + row as f64 * pitch - scroll).collect(),
        scroll,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in display's usable height: 1117pt less the 32pt bar.
    const LAPTOP: f64 = 1085.0;

    const SPAN: Span = Span {
        top: 32.0,
        bottom: 32.0 + LAPTOP,
    };

    #[test]
    fn four_workspaces_fit_the_laptop_at_the_depth_their_rows_need() {
        let s = depth(4, LAPTOP);
        assert!((s - 965.0 / 4340.0).abs() < 1e-12, "{s}");
        let placed = row_tops(4, LAPTOP * s, SPAN, 0.0, 0);
        assert_eq!(placed.scroll, 0.0);
        assert!((placed.tops[0] - (SPAN.top + MARGIN)).abs() < 1e-9, "{placed:?}");
        let last_bottom = placed.tops[3] + LAPTOP * s;
        assert!((last_bottom - (SPAN.bottom - MARGIN)).abs() < 1e-9, "{placed:?}");
        for pair in placed.tops.windows(2) {
            assert!((pair[1] - pair[0] - (LAPTOP * s + ROW_GAP)).abs() < 1e-9);
        }
    }

    #[test]
    fn depth_never_passes_half_nor_falls_below_a_fifth() {
        assert_eq!(depth(1, LAPTOP), SHALLOWEST);
        assert!(depth(2, LAPTOP) < SHALLOWEST);
        assert_eq!(depth(5, LAPTOP), DEEPEST);
        assert_eq!(depth(40, LAPTOP), DEEPEST);
        assert_eq!(depth(0, LAPTOP), SHALLOWEST);
        assert_eq!(depth(3, 0.0), DEEPEST);
        for rows in 1..12 {
            let s = depth(rows, LAPTOP);
            assert!((DEEPEST..=SHALLOWEST).contains(&s), "{rows}: {s}");
        }
    }

    #[test]
    fn a_stack_that_fits_is_centred() {
        let s = depth(2, LAPTOP);
        let placed = row_tops(2, LAPTOP * s, SPAN, 0.0, 1);
        let above = placed.tops[0] - SPAN.top;
        let below = SPAN.bottom - (placed.tops[1] + LAPTOP * s);
        assert!((above - below).abs() < 1e-9, "{placed:?}");
        assert_eq!(placed.scroll, 0.0);
    }

    #[test]
    fn past_the_floor_the_stack_scrolls_the_selected_row_into_view() {
        let h = LAPTOP * DEEPEST;
        let at_top = row_tops(6, h, SPAN, 0.0, 0);
        assert_eq!(at_top.scroll, 0.0);
        assert!((at_top.tops[0] - (SPAN.top + MARGIN)).abs() < 1e-9);
        assert!(at_top.tops[5] + h > SPAN.bottom, "the last row is out of view");

        let down = row_tops(6, h, SPAN, at_top.scroll, 5);
        assert!(
            (down.tops[5] + h - (SPAN.bottom - MARGIN)).abs() < 1e-9,
            "the last row sits on the bottom margin: {down:?}"
        );
        let shift = down.scroll - at_top.scroll;
        assert!((shift - (6.0 * (h + ROW_GAP) - ROW_GAP - (LAPTOP - 2.0 * MARGIN))).abs() < 1e-9);

        let still = row_tops(6, h, SPAN, down.scroll, 4);
        assert_eq!(still, down, "a row already in view scrolls nothing");

        let back = row_tops(6, h, SPAN, down.scroll, 0);
        assert_eq!(back, at_top, "back at the first row the stack is at its top");
    }

    #[test]
    fn the_stack_never_scrolls_past_either_end() {
        let h = LAPTOP * DEEPEST;
        let height = 6.0 * (h + ROW_GAP) - ROW_GAP;
        let most = height - (LAPTOP - 2.0 * MARGIN);
        assert_eq!(row_tops(6, h, SPAN, -500.0, 0).scroll, 0.0);
        assert!((row_tops(6, h, SPAN, 9000.0, 5).scroll - most).abs() < 1e-9);
        assert_eq!(row_tops(6, h, SPAN, f64::NAN, 0).scroll, 0.0);
        assert_eq!(
            row_tops(2, h, SPAN, 300.0, 1).scroll,
            0.0,
            "a stack that fits never scrolls"
        );
    }

    #[test]
    fn a_display_too_short_for_the_margins_still_lays_out() {
        let h = LAPTOP * DEEPEST;
        let cramped = Span { top: 0.0, bottom: 30.0 };
        assert!(row_tops(0, h, cramped, 0.0, 0).tops.is_empty());
        let placed = row_tops(3, h, cramped, 0.0, 9);
        assert_eq!(placed.tops.len(), 3);
        assert!((placed.scroll - (3.0 * h + 2.0 * ROW_GAP + 18.0)).abs() < 1e-9);
    }
}
