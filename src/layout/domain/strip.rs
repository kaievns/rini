//! Where the strip sits, and how wide its columns are.
//!
//! The arithmetic `ScrollingLayoutSystem::calculate_layout` runs before it assigns any frame. It was
//! inline, so none of it could be exercised without a whole layout: these are the same decisions with
//! their inputs named. See "Column width" and "Navigation" in `src/layout/docs/strip.md`.

use objc2_core_foundation::CGRect;

use crate::layout::settings::CenterFocusedColumn;

/// Where the strip is pinned, in display coordinates: the x the selected column is drawn at before
/// the scroll offset is subtracted. A column `centres` puts in the middle; otherwise the strip is
/// pinned to the left edge, or under `always` the last column sits flush with the right.
pub fn anchor_x(
    tiling: CGRect,
    selected_width: f64,
    selected_col_idx: usize,
    column_count: usize,
    center: CenterFocusedColumn,
    has_center_override: bool,
) -> f64 {
    let last = selected_col_idx + 1 == column_count;
    match () {
        _ if centres(selected_col_idx, column_count, center, has_center_override) => {
            tiling.origin.x + (tiling.size.width - selected_width) / 2.0
        }
        _ if center == CenterFocusedColumn::Always && last => {
            tiling.origin.x + tiling.size.width - selected_width
        }
        _ => tiling.origin.x,
    }
}

/// Whether the selected column sits in the middle of the viewport.
///
/// `never` pins the strip to the left edge and lets the selection move within it, so a focus change
/// never shifts an unrelated column. `always` moves the strip instead to centre the selection, and
/// then the first and last columns are special: there is nothing beyond them to scroll to, so they
/// sit flush. A centre override suspends all of that, since something has explicitly asked for one
/// window to be centred.
pub fn centres(
    selected_col_idx: usize,
    column_count: usize,
    center: CenterFocusedColumn,
    has_center_override: bool,
) -> bool {
    if has_center_override {
        return true;
    }
    let first = selected_col_idx == 0;
    let last = selected_col_idx + 1 == column_count;
    center == CenterFocusedColumn::Always && (column_count <= 1 || !(first || last))
}

/// Where each column begins along the strip, and the offset that puts the last column at the anchor.
///
/// Positions accumulate width plus one inner gap, so the strip is one continuous run however the
/// individual column widths differ.
pub fn column_starts(column_widths: &[f64], gap_x: f64) -> (Vec<f64>, f64) {
    let mut starts = Vec::with_capacity(column_widths.len());
    let mut cursor = 0.0;
    for width in column_widths {
        starts.push(cursor);
        cursor += *width + gap_x;
    }
    let max_offset = starts.last().copied().unwrap_or(0.0);
    (starts, max_offset)
}

/// How far a column's width shrinks so that `1/ratio` of them fit abreast with gaps between.
///
/// Without this, three columns at ratio 1/3 need three full widths plus two gaps and the third falls
/// off the display. Each column gives up `(N-1)/N` of a gap, with N inferred from the ratio the user
/// asked for. Table in "Column width", `src/layout/docs/strip.md`.
pub fn gap_share(ratio: f64, gap_x: f64) -> f64 {
    if gap_x <= 0.0 || ratio <= 0.0 {
        return 0.0;
    }
    let columns_abreast = (1.0 / ratio).round().max(1.0);
    gap_x * (columns_abreast - 1.0) / columns_abreast
}

/// The scroll offset that puts the selected column's right edge at `right_x`, in display
/// coordinates: where a drag on its left edge leaves the edge it did not move.
pub fn offset_keeping_right_edge(
    anchor_x: f64,
    selected_start: f64,
    selected_width: f64,
    right_x: f64,
) -> f64 {
    anchor_x + selected_start + selected_width - right_x
}

/// Which way the strip is being scrolled to bring the selected column into view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reveal {
    /// Coming from the right: prefer correcting a column that has fallen off the left.
    FromRight,
    /// Coming from the left: prefer correcting a column that has fallen off the right.
    FromLeft,
    /// No direction; correct whichever edge is wrong, left first.
    Either,
}

/// The scroll offset that brings the selected column fully into the viewport.
///
/// `None` means it is already visible and the offset stands. Which edge is checked first is the only
/// thing `reveal` changes, and it matters when a column is wider than the viewport: then both edges
/// are wrong at once, and the direction decides which one the user sees.
pub fn reveal_offset(
    reveal: Reveal,
    tiling: CGRect,
    anchor_x: f64,
    selected_start: f64,
    selected_width: f64,
    offset: f64,
) -> Option<f64> {
    let selected_x = anchor_x + selected_start - offset;
    let visible_left = tiling.origin.x;
    let visible_right = tiling.origin.x + tiling.size.width;
    let off_left = selected_x < visible_left;
    let off_right = selected_x + selected_width > visible_right;

    let to_left_edge = anchor_x + selected_start - visible_left;
    let to_right_edge = anchor_x + selected_start + selected_width - visible_right;

    match reveal {
        Reveal::FromRight | Reveal::Either if off_left => Some(to_left_edge),
        Reveal::FromRight | Reveal::Either if off_right => Some(to_right_edge),
        Reveal::FromLeft if off_right => Some(to_right_edge),
        Reveal::FromLeft if off_left => Some(to_left_edge),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    use CenterFocusedColumn::{Always, Never};

    /// A 1000pt-wide viewport starting at x=0.
    fn tiling() -> CGRect {
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 800.0))
    }

    fn anchor(idx: usize, count: usize, center: CenterFocusedColumn, override_: bool) -> f64 {
        anchor_x(tiling(), 400.0, idx, count, center, override_)
    }

    #[test]
    fn never_pins_the_strip_to_the_left() {
        for idx in 0..3 {
            assert_eq!(anchor(idx, 3, Never, false), 0.0, "col {idx}");
        }
    }

    #[test]
    fn a_centre_override_puts_the_selection_in_the_middle_even_under_never() {
        assert_eq!(anchor(1, 3, Never, true), 300.0);
    }

    #[test]
    fn always_sits_the_ends_flush_and_centres_the_middle() {
        assert_eq!(anchor(0, 3, Always, false), 0.0, "first is flush left");
        assert_eq!(anchor(1, 3, Always, false), 300.0, "middle is centred");
        assert_eq!(anchor(2, 3, Always, false), 600.0, "last is flush right");
    }

    // With one column there is nothing to scroll to, so the end-of-strip exceptions do not apply.
    #[test]
    fn a_lone_column_is_centred_under_always() {
        assert_eq!(anchor(0, 1, Always, false), 300.0);
    }

    #[test]
    fn centres_is_what_anchor_x_centres_on() {
        for count in 1..=3 {
            for idx in 0..count {
                for center in [Never, Always] {
                    for override_ in [false, true] {
                        assert_eq!(
                            centres(idx, count, center, override_),
                            anchor(idx, count, center, override_) == 300.0,
                            "col {idx} of {count}, {center:?}, override {override_}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn column_starts_accumulate_width_and_one_gap_each() {
        let (starts, max) = column_starts(&[400.0, 200.0, 400.0], 10.0);
        assert_eq!(starts, vec![0.0, 410.0, 620.0]);
        assert_eq!(
            max, 620.0,
            "scrolling to the last column's start is the end of the strip"
        );
    }

    #[test]
    fn an_empty_strip_has_nowhere_to_scroll() {
        assert_eq!(column_starts(&[], 10.0), (Vec::new(), 0.0));
    }

    // Three columns at a third each need 3 widths and 2 gaps in a 1000pt viewport. Giving up 2/3 of a
    // gap apiece is what makes them fit.
    #[test]
    fn columns_give_up_enough_gap_to_fit_abreast() {
        let gap = 12.0;
        for (ratio, abreast) in [(1.0, 1.0), (0.5, 2.0), (1.0 / 3.0, 3.0), (0.25, 4.0)] {
            let share = gap_share(ratio, gap);
            assert!(
                (share - gap * (abreast - 1.0) / abreast).abs() < 1e-9,
                "ratio {ratio}"
            );
            let total = (1000.0 * ratio - share) * abreast + gap * (abreast - 1.0);
            assert!(
                (total - 1000.0).abs() < 1e-9,
                "{abreast} columns must fill the viewport"
            );
        }
    }

    #[test]
    fn a_single_full_width_column_gives_up_no_gap() {
        assert_eq!(gap_share(1.0, 12.0), 0.0);
        assert_eq!(gap_share(0.5, 0.0), 0.0, "no gaps, nothing to absorb");
        assert_eq!(gap_share(0.0, 12.0), 0.0, "a zero ratio is not a column count");
    }

    #[test]
    fn offset_keeping_right_edge_puts_the_selected_right_edge_back_at_right_x() {
        let (anchor, start, width) = (3.0, 597.0, 1146.5);
        let offset = offset_keeping_right_edge(anchor, start, width, 1482.0);
        assert_eq!(offset, 264.5);
        assert_eq!(anchor + start - offset + width, 1482.0);
    }

    #[test]
    fn a_column_already_in_view_needs_no_scroll() {
        for reveal in [Reveal::FromLeft, Reveal::FromRight, Reveal::Either] {
            assert_eq!(
                reveal_offset(reveal, tiling(), 0.0, 100.0, 400.0, 0.0),
                None,
                "{reveal:?}"
            );
        }
    }

    #[test]
    fn a_column_off_the_left_scrolls_back_to_the_left_edge() {
        // start 0, offset 200: the column is drawn at x=-200.
        let got = reveal_offset(Reveal::Either, tiling(), 0.0, 0.0, 400.0, 200.0);
        assert_eq!(got, Some(0.0));
    }

    #[test]
    fn a_column_off_the_right_scrolls_until_its_right_edge_lands() {
        // start 800, offset 0: drawn at 800..1200, and the viewport ends at 1000.
        let got = reveal_offset(Reveal::Either, tiling(), 0.0, 800.0, 400.0, 0.0);
        assert_eq!(got, Some(200.0));
    }

    // A column wider than the viewport is off BOTH edges, and only the direction decides which edge
    // the user is shown. This is the one case the reveal direction exists for.
    #[test]
    fn an_oversized_column_shows_the_edge_it_is_approached_from() {
        let wide = 1400.0;
        let from_right = reveal_offset(Reveal::FromRight, tiling(), 0.0, 0.0, wide, 100.0);
        assert_eq!(
            from_right,
            Some(0.0),
            "coming from the right, correct the left edge"
        );
        let from_left = reveal_offset(Reveal::FromLeft, tiling(), 0.0, 0.0, wide, 100.0);
        assert_eq!(
            from_left,
            Some(400.0),
            "coming from the left, correct the right edge"
        );
    }
}
