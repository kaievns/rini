//! Where the strip sits, and how wide its columns are.
//!
//! The arithmetic `ScrollingLayoutSystem::calculate_layout` runs before it assigns any frame. It was
//! inline, so none of it could be exercised without a whole layout: these are the same decisions with
//! their inputs named. See "Column width" and "Navigation" in `src/layout/docs/strip.md`.

use objc2_core_foundation::CGRect;

use crate::layout::settings::{ScrollingAlignment, ScrollingFocusNavigationStyle};

/// Where the strip is pinned, in display coordinates: the x the selected column is drawn at before
/// the scroll offset is subtracted.
///
/// Niri navigation pins the strip to the left edge and lets the selection move within it, so a focus
/// change never shifts an unrelated column. Anchored navigation moves the strip instead, and then the
/// first and last columns are special: there is nothing beyond them to scroll to, so they sit flush
/// rather than at the alignment the middle columns get. A centre override suspends all of that, since
/// something has explicitly asked for one window to be centred.
pub fn anchor_x(
    tiling: CGRect,
    selected_width: f64,
    selected_col_idx: usize,
    column_count: usize,
    alignment: ScrollingAlignment,
    navigation: ScrollingFocusNavigationStyle,
    has_center_override: bool,
) -> f64 {
    let left = tiling.origin.x;
    let right = tiling.origin.x + tiling.size.width - selected_width;
    let centre = tiling.origin.x + (tiling.size.width - selected_width) / 2.0;
    let niri = matches!(navigation, ScrollingFocusNavigationStyle::Niri);

    if niri && !has_center_override {
        return left;
    }
    // Anchored navigation, more than one column, no override: the ends sit flush.
    let anchored_strip = !niri && !has_center_override && column_count > 1;
    let first = selected_col_idx == 0;
    let last = selected_col_idx == column_count.saturating_sub(1);

    match alignment {
        ScrollingAlignment::Left if anchored_strip && last => right,
        ScrollingAlignment::Left => left,
        ScrollingAlignment::Center if anchored_strip && first => left,
        ScrollingAlignment::Center if anchored_strip && last => right,
        ScrollingAlignment::Center => centre,
        ScrollingAlignment::Right if anchored_strip && first => left,
        ScrollingAlignment::Right => right,
    }
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

    use ScrollingAlignment as Align;
    use ScrollingFocusNavigationStyle as Nav;

    /// A 1000pt-wide viewport starting at x=0.
    fn tiling() -> CGRect {
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 800.0))
    }

    fn anchor(idx: usize, count: usize, align: Align, nav: Nav, override_: bool) -> f64 {
        anchor_x(tiling(), 400.0, idx, count, align, nav, override_)
    }

    #[test]
    fn niri_navigation_pins_the_strip_to_the_left_whatever_the_alignment() {
        for align in [Align::Left, Align::Center, Align::Right] {
            for idx in 0..3 {
                assert_eq!(
                    anchor(idx, 3, align, Nav::Niri, false),
                    0.0,
                    "{align:?} col {idx}"
                );
            }
        }
    }

    #[test]
    fn a_centre_override_puts_the_selection_in_the_middle_even_under_niri() {
        assert_eq!(anchor(1, 3, Align::Center, Nav::Niri, true), 300.0);
    }

    #[test]
    fn anchored_navigation_sits_the_ends_flush_and_the_middle_at_its_alignment() {
        assert_eq!(
            anchor(0, 3, Align::Center, Nav::Anchored, false),
            0.0,
            "first is flush left"
        );
        assert_eq!(
            anchor(1, 3, Align::Center, Nav::Anchored, false),
            300.0,
            "middle is centred"
        );
        assert_eq!(
            anchor(2, 3, Align::Center, Nav::Anchored, false),
            600.0,
            "last is flush right"
        );
    }

    #[test]
    fn left_alignment_only_makes_an_exception_for_the_last_column() {
        assert_eq!(anchor(0, 3, Align::Left, Nav::Anchored, false), 0.0);
        assert_eq!(anchor(1, 3, Align::Left, Nav::Anchored, false), 0.0);
        assert_eq!(anchor(2, 3, Align::Left, Nav::Anchored, false), 600.0);
    }

    #[test]
    fn right_alignment_only_makes_an_exception_for_the_first_column() {
        assert_eq!(anchor(0, 3, Align::Right, Nav::Anchored, false), 0.0);
        assert_eq!(anchor(1, 3, Align::Right, Nav::Anchored, false), 600.0);
        assert_eq!(anchor(2, 3, Align::Right, Nav::Anchored, false), 600.0);
    }

    // With one column there is nothing to scroll to, so the end-of-strip exceptions do not apply and
    // the alignment is taken at face value.
    #[test]
    fn a_lone_column_takes_its_alignment_literally() {
        assert_eq!(anchor(0, 1, Align::Left, Nav::Anchored, false), 0.0);
        assert_eq!(anchor(0, 1, Align::Center, Nav::Anchored, false), 300.0);
        assert_eq!(anchor(0, 1, Align::Right, Nav::Anchored, false), 600.0);
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
