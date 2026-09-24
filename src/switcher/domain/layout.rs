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
    /// One row's picture area.
    pub tile: CGSize,
    /// Between rows.
    pub gap: f64,
    /// Inside the panel's edge, around the whole strip.
    pub padding: f64,
    /// Height below each tile for the title and app name.
    pub caption: f64,
    /// The most of the screen's width the panel may take.
    pub max_screen_fraction: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            tile: CGSize::new(200.0, 125.0),
            gap: 12.0,
            padding: 20.0,
            caption: 34.0,
            max_screen_fraction: 0.9,
        }
    }
}

impl Metrics {
    fn row_width(&self) -> f64 {
        self.tile.width
    }

    fn row_height(&self) -> f64 {
        self.tile.height + self.caption
    }

    /// Width of `count` rows including the gaps between them, with no padding.
    fn strip_width(&self, count: usize) -> f64 {
        if count == 0 {
            return 0.0;
        }
        let count = count as f64;
        count * self.row_width() + (count - 1.0) * self.gap
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

/// Lay out `count` rows for a switch with `selected` highlighted, centred on `screen`.
///
/// The panel is as wide as its content up to `max_screen_fraction` of the screen, then the strip
/// scrolls inside it so the selection is always visible. Scrolling rather than capping: the ask was
/// every window, and a cap hides exactly the tail that a global switcher exists to reach.
pub fn lay_out(count: usize, selected: usize, screen: CGRect, metrics: Metrics) -> Option<Strip> {
    if count == 0 {
        return None;
    }
    let strip = metrics.strip_width(count);
    let widest = (screen.size.width * metrics.max_screen_fraction) - metrics.padding * 2.0;
    let visible = strip.min(widest.max(metrics.row_width()));

    let panel_size = CGSize::new(
        visible + metrics.padding * 2.0,
        metrics.row_height() + metrics.padding * 2.0,
    );
    let panel = CGRect::new(
        CGPoint::new(
            screen.origin.x + ((screen.size.width - panel_size.width) / 2.0).round(),
            screen.origin.y + ((screen.size.height - panel_size.height) / 2.0).round(),
        ),
        panel_size,
    );

    let scroll = scroll_for(selected.min(count - 1), visible, metrics);
    let rows = (0..count)
        .map(|index| {
            CGRect::new(
                CGPoint::new(
                    metrics.padding + index as f64 * (metrics.row_width() + metrics.gap) - scroll,
                    metrics.padding,
                ),
                CGSize::new(metrics.row_width(), metrics.row_height()),
            )
        })
        .collect();

    Some(Strip { panel, rows, scroll })
}

/// How far to scroll so the selected row is fully inside `visible` points of strip.
///
/// Clamped at both ends, so the first and last rows sit against the padding rather than leaving a gap
/// the strip has scrolled past.
fn scroll_for(selected: usize, visible: f64, metrics: Metrics) -> f64 {
    let step = metrics.row_width() + metrics.gap;
    let left = selected as f64 * step;
    // Centre the selection when it does not fit, which keeps a neighbour visible on each side.
    let centred = left - (visible - metrics.row_width()) / 2.0;
    let max_scroll = (step * selected as f64 + metrics.row_width() - visible).max(0.0);
    centred.clamp(0.0, max_scroll).max(0.0)
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

    #[test]
    fn nothing_to_show_is_no_strip() {
        assert_eq!(lay_out(0, 0, screen(), metrics()), None);
    }

    #[test]
    fn one_row_gets_a_panel_the_size_of_one_row() {
        let strip = lay_out(1, 0, screen(), metrics()).expect("strip");
        let m = metrics();

        assert_eq!(strip.panel.size.width, m.tile.width + m.padding * 2.0);
        assert_eq!(
            strip.panel.size.height,
            m.tile.height + m.caption + m.padding * 2.0
        );
        assert_eq!(strip.rows.len(), 1);
        assert_eq!(strip.scroll, 0.0);
    }

    /// Three windows should get a small panel, not an empty band across the screen.
    #[test]
    fn the_panel_is_sized_to_its_content() {
        let three = lay_out(3, 0, screen(), metrics()).expect("strip");
        let ten = lay_out(10, 0, screen(), metrics()).expect("strip");

        assert!(three.panel.size.width < ten.panel.size.width);
        assert!(three.panel.size.width < screen().size.width / 2.0);
    }

    #[test]
    fn the_panel_is_centred_on_the_screen() {
        let strip = lay_out(4, 0, screen(), metrics()).expect("strip");
        let centre = strip.panel.origin.x + strip.panel.size.width / 2.0;
        assert!((centre - screen().size.width / 2.0).abs() <= 1.0);
    }

    /// The panel is placed in screen coordinates, so a display that does not start at the origin still
    /// gets the panel on IT rather than on the primary.
    #[test]
    fn the_panel_lands_on_the_screen_it_was_given() {
        let second = CGRect::new(CGPoint::new(1728.0, 0.0), CGSize::new(2560.0, 1440.0));
        let strip = lay_out(4, 0, second, metrics()).expect("strip");

        assert!(strip.panel.origin.x > 1728.0);
        let centre = strip.panel.origin.x + strip.panel.size.width / 2.0;
        assert!((centre - (1728.0 + 2560.0 / 2.0)).abs() <= 1.0);
    }

    #[test]
    fn the_panel_never_exceeds_its_share_of_the_screen() {
        let m = metrics();
        let strip = lay_out(60, 0, screen(), m).expect("strip");

        assert!(
            strip.panel.size.width <= screen().size.width * m.max_screen_fraction + 1.0,
            "got {}",
            strip.panel.size.width
        );
    }

    /// The ask was every window, so a list too long to fit scrolls rather than being cut short.
    #[test]
    fn a_list_too_long_to_fit_keeps_every_row() {
        let strip = lay_out(60, 0, screen(), metrics()).expect("strip");
        assert_eq!(strip.rows.len(), 60, "no row is dropped");
    }

    #[test]
    fn rows_are_laid_out_left_to_right_with_a_gap() {
        let m = metrics();
        let strip = lay_out(3, 0, screen(), m).expect("strip");

        assert_eq!(strip.rows[0].origin.x, m.padding);
        assert_eq!(strip.rows[1].origin.x, m.padding + m.tile.width + m.gap);
        assert_eq!(strip.rows[2].origin.x, m.padding + 2.0 * (m.tile.width + m.gap));
    }

    /// The selection has to be on screen, or holding the key walks off the edge of the panel and the
    /// user cannot see what they are about to commit to.
    #[test]
    fn the_selection_is_always_inside_the_panel() {
        let m = metrics();
        let count = 60;
        for selected in [0usize, 1, 7, 30, 58, 59] {
            let strip = lay_out(count, selected, screen(), m).expect("strip");
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
        let first = lay_out(60, 0, screen(), m).expect("strip");
        assert_eq!(first.scroll, 0.0);
        assert_eq!(first.rows[0].origin.x, m.padding);

        let last = lay_out(60, 59, screen(), m).expect("strip");
        let inner_right = last.panel.size.width - m.padding;
        assert!(
            (last.rows[59].origin.x + last.rows[59].size.width - inner_right).abs() <= 1.0,
            "last row ends at {} not {}",
            last.rows[59].origin.x + last.rows[59].size.width,
            inner_right
        );
    }

    #[test]
    fn a_selection_past_the_end_does_not_panic() {
        let strip = lay_out(3, 99, screen(), metrics()).expect("strip");
        assert_eq!(strip.rows.len(), 3);
    }

    /// A click lands on the row it is inside, in panel coordinates.
    #[test]
    fn a_click_inside_a_row_selects_it() {
        let m = metrics();
        let strip = lay_out(3, 0, screen(), m).expect("strip");

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
        let strip = lay_out(3, 0, screen(), m).expect("strip");

        let gap_x = strip.rows[0].origin.x + strip.rows[0].size.width + m.gap / 2.0;
        let point = CGPoint::new(gap_x, m.padding + 10.0);

        assert_eq!(strip.row_at(point), None);
    }

    #[test]
    fn a_click_in_the_padding_selects_nothing() {
        let strip = lay_out(3, 0, screen(), metrics()).expect("strip");
        assert_eq!(strip.row_at(CGPoint::new(2.0, 2.0)), None);
    }

    /// A row scrolled out of view keeps its rect, outside the panel, and must not be clickable through
    /// the panel's edge.
    #[test]
    fn a_row_scrolled_out_of_view_is_not_hit_by_a_click_inside_the_panel() {
        let m = metrics();
        let strip = lay_out(60, 40, screen(), m).expect("strip");

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

    /// Every row's rect has the same height, so a click near the bottom of one row cannot fall into
    /// another.
    #[test]
    fn rows_share_one_band() {
        let strip = lay_out(5, 0, screen(), metrics()).expect("strip");
        let first = strip.rows[0];
        for row in &strip.rows {
            assert_eq!(row.origin.y, first.origin.y);
            assert_eq!(row.size.height, first.size.height);
        }
    }
}
