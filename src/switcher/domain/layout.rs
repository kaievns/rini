//! Where the switcher's rows go on screen, and which row a click landed on.
//!
//! Pure arithmetic, kept out of the panel for the usual reason: a wrong row rect and a wrong hit test
//! look identical from the outside — the click selects the neighbour — and neither can be debugged by
//! eye. Here they are two functions over numbers with tests, and the panel does nothing but place
//! layers at the rects it is handed.
//!
//! Two decisions worth naming. The strip SCROLLS rather than capping the list, because the ask was
//! every window and a capped list silently hides the tail the switcher exists to reach. And the panel
//! is sized to its content up to a fraction of the screen, so three windows get a small panel instead
//! of an empty band.

use objc2_core_foundation::{CGPoint, CGRect, CGSize};

/// The sizes the strip is built from, in points.
///
/// Not configurable yet. They are here as a struct rather than as constants so the tests can state
/// what they are assuming, and so a setting can be threaded in later without touching the arithmetic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Every tile is this tall. Only the width varies, so the row of captions lines up.
    pub tile_height: f64,
    /// A tile is as wide as its window is, in proportion — so a full-width window reads as wide and a
    /// third-width column reads as narrow, which is most of what tells two terminals apart at a glance.
    ///
    /// Clamped, because the proportions in a real strip are extreme: a third-width column beside a
    /// maximised window is 86pt against 260pt at this height, and 86pt has no room for an icon badge
    /// and a caption.
    pub min_tile_width: f64,
    pub max_tile_width: f64,
    /// Between rows.
    pub gap: f64,
    /// Inside the panel's edge, at the sides and above the strip.
    pub padding: f64,
    /// Below the captions, and SMALLER than `padding` on purpose.
    ///
    /// The eye anchors on the tiles, not on the captions: a tile is a bright slab and a caption is two
    /// thin lines of small text that read as part of the surrounding space. So equal numbers above and
    /// below put the tile 22pt from the top edge and 54pt from the bottom one, and the panel looks
    /// bottom-heavy however the arithmetic is written. Reported twice, the second time after the caption
    /// band had already been tightened, which is what ruled out the band being the whole of it.
    pub bottom_padding: f64,
    /// Height below each tile for the title and app name.
    ///
    /// Sized to the TEXT, not chosen for looks: two lines at 11pt with default leading is about 27pt,
    /// and a taller box leaves slack under the text — a caption layer draws from its top, so any surplus
    /// lands at the bottom. A row with no window title draws one line and leaves the second line's worth
    /// empty, which is why the band cannot be trusted to read as full.
    pub caption: f64,
    /// Between a tile and its caption.
    pub caption_gap: f64,
    /// The most of the screen's width the panel may take.
    pub max_screen_fraction: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            tile_height: 163.0,
            min_tile_width: 104.0,
            max_tile_width: 340.0,
            gap: 14.0,
            padding: 22.0,
            bottom_padding: 9.0,
            caption: 27.0,
            caption_gap: 5.0,
            max_screen_fraction: 0.9,
        }
    }
}

impl Metrics {
    /// How wide a tile showing a window of `size` should be.
    ///
    /// A window with no usable size — zero or negative, which is what a window still opening reports —
    /// gets the widest allowed rather than a sliver, because a sliver reads as a broken row.
    pub fn tile_width(&self, size: CGSize) -> f64 {
        if size.width <= 0.0 || size.height <= 0.0 {
            return self.max_tile_width;
        }
        (self.tile_height * (size.width / size.height))
            .clamp(self.min_tile_width, self.max_tile_width)
    }

    fn row_height(&self) -> f64 {
        self.tile_height + self.caption_gap + self.caption
    }

    /// The panel's height: the row, with `padding` above it and `bottom_padding` below.
    fn panel_height(&self) -> f64 {
        self.padding + self.row_height() + self.bottom_padding
    }
}

/// Where everything goes for one open switch.
#[derive(Debug, Clone, PartialEq)]
pub struct Strip {
    /// The panel itself, in the screen's coordinates.
    pub panel: CGRect,
    /// One rect per row, in the PANEL's coordinates, so the panel can place layers without knowing
    /// where it sits on screen. Rows scrolled out of view are still present, with rects outside the
    /// panel's bounds; the caller clips.
    pub rows: Vec<CGRect>,
    /// How far the strip is scrolled, in points. Zero when everything fits.
    pub scroll: f64,
}

impl Strip {
    /// The row a point in PANEL coordinates lands on, or `None` for the gaps and the padding.
    ///
    /// Exact rather than nearest: a click in the gap between two rows is not a request to select
    /// either of them, and guessing produces the switcher selecting a window the user did not point at.
    pub fn row_at(&self, point: CGPoint) -> Option<usize> {
        self.rows.iter().position(|row| {
            point.x >= row.origin.x
                && point.x < row.origin.x + row.size.width
                && point.y >= row.origin.y
                && point.y < row.origin.y + row.size.height
        })
    }
}

/// Lay out one row per window in `windows`, with `selected` highlighted, centred on `screen`.
///
/// Each row is as wide as its window is in proportion, so the strip reads as a map of the layout rather
/// than as a row of identical squares. The panel is as wide as its content up to
/// `max_screen_fraction` of the screen, then the strip scrolls inside it so the selection is always
/// visible. Scrolling rather than capping: the ask was every window, and a cap hides exactly the tail
/// that a global switcher exists to reach.
pub fn lay_out(
    windows: &[CGSize],
    selected: usize,
    screen: CGRect,
    metrics: Metrics,
) -> Option<Strip> {
    if windows.is_empty() {
        return None;
    }
    let widths: Vec<f64> = windows.iter().map(|size| metrics.tile_width(*size)).collect();
    // Left edge of each row within the strip, before scrolling.
    let mut lefts = Vec::with_capacity(widths.len());
    let mut cursor = 0.0;
    for width in &widths {
        lefts.push(cursor);
        cursor += width + metrics.gap;
    }
    let strip = cursor - metrics.gap;

    let widest = (screen.size.width * metrics.max_screen_fraction) - metrics.padding * 2.0;
    let visible = strip.min(widest.max(metrics.max_tile_width));

    let panel_size = CGSize::new(visible + metrics.padding * 2.0, metrics.panel_height());
    let panel = CGRect::new(
        CGPoint::new(
            screen.origin.x + ((screen.size.width - panel_size.width) / 2.0).round(),
            screen.origin.y + ((screen.size.height - panel_size.height) / 2.0).round(),
        ),
        panel_size,
    );

    let selected = selected.min(widths.len() - 1);
    let scroll = scroll_for(lefts[selected], widths[selected], strip, visible);
    let rows = lefts
        .iter()
        .zip(&widths)
        .map(|(left, width)| {
            CGRect::new(
                CGPoint::new(metrics.padding + left - scroll, metrics.padding),
                CGSize::new(*width, metrics.row_height()),
            )
        })
        .collect();

    Some(Strip { panel, rows, scroll })
}

/// How far to scroll so the selected row is fully inside `visible` points of strip.
///
/// Centred on the selection when the strip does not fit, which keeps a neighbour visible on each side,
/// and clamped at both ends so the first and last rows sit against the padding rather than leaving a
/// gap the strip has scrolled past.
fn scroll_for(left: f64, width: f64, strip: f64, visible: f64) -> f64 {
    let centred = left - (visible - width) / 2.0;
    centred.clamp(0.0, (strip - visible).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> CGRect {
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1728.0, 1117.0))
    }

    fn metrics() -> Metrics {
        Metrics::default()
    }

    /// A maximised window on the built-in display, and a third-width column beside it.
    fn wide() -> CGSize {
        CGSize::new(1720.0, 1081.0)
    }

    fn third() -> CGSize {
        CGSize::new(573.0, 1081.0)
    }

    fn square() -> CGSize {
        CGSize::new(800.0, 800.0)
    }

    #[test]
    fn nothing_to_show_is_no_strip() {
        assert_eq!(lay_out(&[], 0, screen(), metrics()), None);
    }

    /// The point of variable widths: a full-width window reads as wide and a third-width column reads as
    /// narrow, which is most of what tells two terminals apart at a glance.
    #[test]
    fn a_tile_is_as_wide_as_its_window_is_in_proportion() {
        let m = metrics();
        assert!(m.tile_width(wide()) > m.tile_width(square()));
        assert!(m.tile_width(square()) > m.tile_width(third()));
    }

    /// The proportions in a real strip are extreme, and a sliver has no room for an icon badge and a
    /// caption.
    #[test]
    fn tile_widths_are_clamped_at_both_ends() {
        let m = metrics();
        let sliver = CGSize::new(40.0, 1081.0);
        let panorama = CGSize::new(5120.0, 600.0);

        assert_eq!(m.tile_width(sliver), m.min_tile_width);
        assert_eq!(m.tile_width(panorama), m.max_tile_width);
    }

    /// A window still opening reports a zero size. A sliver there reads as a broken row.
    #[test]
    fn a_window_with_no_usable_size_gets_the_widest_tile() {
        let m = metrics();
        assert_eq!(m.tile_width(CGSize::new(0.0, 0.0)), m.max_tile_width);
        assert_eq!(m.tile_width(CGSize::new(-10.0, 100.0)), m.max_tile_width);
    }

    /// The reported asymmetry. A caption layer draws from its top, so a box taller than its text leaves
    /// the surplus at the bottom, where it reads as extra padding under the strip. The band is sized to
    /// two lines at 11pt.
    #[test]
    fn a_row_is_the_tile_plus_exactly_the_caption_band() {
        let m = metrics();
        let strip = lay_out(&[wide()], 0, screen(), m).expect("strip");

        assert_eq!(
            strip.rows[0].size.height,
            m.tile_height + m.caption_gap + m.caption
        );
    }

    /// The panel's inset below the captions is SMALLER than the one above the tiles, and the tile is what
    /// the eye measures from. Equal insets read as bottom-heavy, because the caption band between the
    /// tile and the bottom edge reads as space rather than as content.
    #[test]
    fn the_inset_below_the_captions_is_tighter_than_the_one_above_the_tiles() {
        let m = metrics();
        let strip = lay_out(&[wide()], 0, screen(), m).expect("strip");
        let row = strip.rows[0];

        assert_eq!(row.origin.y, m.padding, "the strip hangs from the top inset");
        let below = strip.panel.size.height - (row.origin.y + row.size.height);
        assert!(
            below < m.padding,
            "{below} below the captions is not tighter than {} above the tiles",
            m.padding
        );

        // What the eye actually compares: tile top to the panel's top edge, against tile bottom to its
        // bottom edge. The caption band can never make these equal, so the test pins the gap it is
        // allowed to leave rather than pretending to symmetry.
        let above_tile = m.padding;
        let below_tile = strip.panel.size.height - (row.origin.y + m.tile_height);
        assert!(
            below_tile < above_tile * 2.0,
            "tile sits {above_tile} from the top and {below_tile} from the bottom"
        );
    }

    #[test]
    fn every_tile_is_the_same_height_so_the_captions_line_up() {
        let strip = lay_out(&[wide(), third(), square()], 0, screen(), metrics()).expect("strip");
        let first = strip.rows[0];
        for row in &strip.rows {
            assert_eq!(row.origin.y, first.origin.y);
            assert_eq!(row.size.height, first.size.height);
        }
    }

    #[test]
    fn rows_are_laid_out_left_to_right_with_a_gap() {
        let m = metrics();
        let windows = [wide(), third(), square()];
        let strip = lay_out(&windows, 0, screen(), m).expect("strip");

        assert_eq!(strip.rows[0].origin.x, m.padding);
        for pair in strip.rows.windows(2) {
            let gap = pair[1].origin.x - (pair[0].origin.x + pair[0].size.width);
            assert!((gap - m.gap).abs() < 0.001, "gap was {gap}");
        }
    }

    /// Three windows should get a small panel, not an empty band across the screen.
    #[test]
    fn the_panel_is_sized_to_its_content() {
        let three = lay_out(&[third(), third(), third()], 0, screen(), metrics()).expect("strip");
        let many: Vec<CGSize> = std::iter::repeat(wide()).take(10).collect();
        let ten = lay_out(&many, 0, screen(), metrics()).expect("strip");

        assert!(three.panel.size.width < ten.panel.size.width);
        assert!(three.panel.size.width < screen().size.width / 2.0);
    }

    #[test]
    fn the_panel_is_centred_on_the_screen() {
        let strip = lay_out(&[wide(), third()], 0, screen(), metrics()).expect("strip");
        let centre = strip.panel.origin.x + strip.panel.size.width / 2.0;
        assert!((centre - screen().size.width / 2.0).abs() <= 1.0);
    }

    /// The panel is placed in screen coordinates, so a display that does not start at the origin still
    /// gets the panel on IT rather than on the primary.
    #[test]
    fn the_panel_lands_on_the_screen_it_was_given() {
        let second = CGRect::new(CGPoint::new(1728.0, 0.0), CGSize::new(2560.0, 1440.0));
        let strip = lay_out(&[wide(), third()], 0, second, metrics()).expect("strip");

        assert!(strip.panel.origin.x > 1728.0);
        let centre = strip.panel.origin.x + strip.panel.size.width / 2.0;
        assert!((centre - (1728.0 + 2560.0 / 2.0)).abs() <= 1.0);
    }

    #[test]
    fn the_panel_never_exceeds_its_share_of_the_screen() {
        let m = metrics();
        let many: Vec<CGSize> = std::iter::repeat(wide()).take(60).collect();
        let strip = lay_out(&many, 0, screen(), m).expect("strip");

        assert!(
            strip.panel.size.width <= screen().size.width * m.max_screen_fraction + 1.0,
            "got {}",
            strip.panel.size.width
        );
    }

    /// The ask was every window, so a list too long to fit scrolls rather than being cut short.
    #[test]
    fn a_list_too_long_to_fit_keeps_every_row() {
        let many: Vec<CGSize> = std::iter::repeat(wide()).take(60).collect();
        let strip = lay_out(&many, 0, screen(), metrics()).expect("strip");
        assert_eq!(strip.rows.len(), 60, "no row is dropped");
    }

    /// The selection has to be on screen, or holding the key walks off the edge of the panel and the
    /// user cannot see what they are about to commit to. Mixed widths, because uniform ones would not
    /// exercise the cumulative arithmetic.
    #[test]
    fn the_selection_is_always_inside_the_panel() {
        let m = metrics();
        let windows: Vec<CGSize> = (0..60)
            .map(|i| match i % 3 {
                0 => wide(),
                1 => third(),
                _ => square(),
            })
            .collect();

        for selected in [0usize, 1, 7, 30, 58, 59] {
            let strip = lay_out(&windows, selected, screen(), m).expect("strip");
            let row = strip.rows[selected];
            let inner_right = strip.panel.size.width - m.padding;

            assert!(
                row.origin.x >= m.padding - 1.0,
                "row {selected} starts at {} inside padding {}",
                row.origin.x,
                m.padding
            );
            assert!(
                row.origin.x + row.size.width <= inner_right + 1.0,
                "row {selected} ends at {} past {}",
                row.origin.x + row.size.width,
                inner_right
            );
        }
    }

    /// The first and last rows sit against the padding rather than leaving a gap the strip scrolled
    /// past.
    #[test]
    fn the_ends_of_the_list_are_flush() {
        let m = metrics();
        let windows: Vec<CGSize> = std::iter::repeat(wide()).take(60).collect();

        let first = lay_out(&windows, 0, screen(), m).expect("strip");
        assert_eq!(first.scroll, 0.0);
        assert_eq!(first.rows[0].origin.x, m.padding);

        let last = lay_out(&windows, 59, screen(), m).expect("strip");
        let inner_right = last.panel.size.width - m.padding;
        let end = last.rows[59].origin.x + last.rows[59].size.width;
        assert!(
            (end - inner_right).abs() <= 1.0,
            "last row ends at {end} not {inner_right}"
        );
    }

    #[test]
    fn a_selection_past_the_end_does_not_panic() {
        let strip = lay_out(&[wide(), third()], 99, screen(), metrics()).expect("strip");
        assert_eq!(strip.rows.len(), 2);
    }

    /// A click lands on the row it is inside, in panel coordinates.
    #[test]
    fn a_click_inside_a_row_selects_it() {
        let strip = lay_out(&[wide(), third(), square()], 0, screen(), metrics()).expect("strip");

        let middle = strip.rows[1];
        let point = CGPoint::new(
            middle.origin.x + middle.size.width / 2.0,
            middle.origin.y + middle.size.height / 2.0,
        );

        assert_eq!(strip.row_at(point), Some(1));
    }

    /// A click in the gap is not a request to select either neighbour. Guessing at the nearest would
    /// select a window the user did not point at.
    #[test]
    fn a_click_in_the_gap_selects_nothing() {
        let m = metrics();
        let strip = lay_out(&[wide(), third(), square()], 0, screen(), m).expect("strip");

        let gap_x = strip.rows[0].origin.x + strip.rows[0].size.width + m.gap / 2.0;
        assert_eq!(strip.row_at(CGPoint::new(gap_x, m.padding + 10.0)), None);
    }

    #[test]
    fn a_click_in_the_padding_selects_nothing() {
        let strip = lay_out(&[wide(), third()], 0, screen(), metrics()).expect("strip");
        assert_eq!(strip.row_at(CGPoint::new(2.0, 2.0)), None);
    }

    /// A row scrolled out of view keeps its rect, outside the panel, and must not be clickable through
    /// the panel's edge.
    #[test]
    fn a_row_scrolled_out_of_view_is_not_hit_by_a_click_inside_the_panel() {
        let m = metrics();
        let windows: Vec<CGSize> = std::iter::repeat(wide()).take(60).collect();
        let strip = lay_out(&windows, 40, screen(), m).expect("strip");

        let offscreen: Vec<usize> = strip
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.origin.x + row.size.width < m.padding)
            .map(|(index, _)| index)
            .collect();
        assert!(!offscreen.is_empty(), "the fixture must actually scroll");

        for index in offscreen {
            let row = strip.rows[index];
            let point = CGPoint::new(m.padding + 1.0, row.origin.y + 1.0);
            assert_ne!(strip.row_at(point), Some(index));
        }
    }
}
