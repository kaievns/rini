//! Telling an app's title-bar zoom from any other resize.
//!
//! A double-click on a title bar has macOS zoom the window, and Accessibility reports that as
//! ordinary resizes. The double-click, seen by the input tap, is the only evidence, so it counts only
//! for the one window whose title bar it landed on, and only for a second. The requirement is "Full
//! width and height" in `specs/strip.md`; the choices behind the numbers are in
//! `src/layout/docs/strip.md`.
use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::WindowId;
use rini_geometry::{CGRectExt, SameAs};

/// How far below a window's top edge a double-click still lands on its title bar.
pub const TITLE_BAR_BAND: f64 = 60.0;

/// The one window whose title bar `point` is on, of `windows` and where they are.
///
/// None when the point is below every title bar, or on more than one.
pub fn title_bar_under(
    point: CGPoint,
    windows: impl IntoIterator<Item = (WindowId, CGRect)>,
) -> Option<WindowId> {
    let mut hits = windows
        .into_iter()
        .filter(|&(_, frame)| frame.contains(point) && point.y < frame.origin.y + TITLE_BAR_BAND);
    let (window, _) = hits.next()?;
    hits.next().is_none().then_some(window)
}

/// A double-click on a tiled window's title bar, which its next resize answers.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    window: WindowId,
    at: Instant,
    toggled: bool,
    /// The last of the app's frames rini put its own frame back over.
    overruled: Option<CGRect>,
}

/// A frame report of a tiled window that rini did not ask for.
#[derive(Debug, Clone, Copy)]
pub struct SizeReport {
    /// Where rini last knew the window to be.
    pub from: CGRect,
    pub to: CGRect,
    /// A frame rini wrote that has not come back yet.
    pub pending: Option<CGRect>,
    /// The whole display the window's strip is on, menu bar included.
    pub display: Option<CGRect>,
    /// Whether the window is a column of a strip, rather than floating or unmanaged.
    pub column: bool,
}

/// What a report means while its window's double-click is believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    /// The zoom itself: rini's full width toggles, whatever size the app chose.
    Toggle,
    /// The rest of the zoom after the toggle: no width of it is the column's, and rini's frame goes
    /// back over it.
    Rest,
}

enum Reading {
    Zoom(Zoom),
    NotZoom,
    /// Expired, or answered by something other than a zoom: no later report can be one.
    Spent,
}

impl Evidence {
    /// Long enough to cover a zoom's animation; a resize later than this is not the zoom's.
    const BELIEVED_FOR: Duration = Duration::from_secs(1);

    pub fn new(window: WindowId, at: Instant) -> Self {
        Self {
            window,
            at,
            toggled: false,
            overruled: None,
        }
    }

    pub fn window(&self) -> WindowId {
        self.window
    }

    fn is_believed(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.at) < Self::BELIEVED_FOR
    }

    fn read(&mut self, report: &SizeReport, now: Instant) -> Reading {
        if !self.is_believed(now) {
            return Reading::Spent;
        }
        let echo = report.pending.is_some_and(|pending| pending.same_as(report.to));
        if !report.column || report.from.size.same_as(report.to.size) || echo {
            return Reading::NotZoom;
        }
        // Native fullscreen covers the menu bar; a zoom stops below it.
        if report.display.is_some_and(|display| display.same_as(report.to)) {
            return Reading::Spent;
        }
        if !self.toggled {
            if stretches_top_edge(report.from, report.to) {
                return Reading::Spent;
            }
            self.toggled = true;
            return Reading::Zoom(Zoom::Toggle);
        }
        // The app holding a frame rini already overruled once; why it is let be is in the docs.
        if self.overruled.is_some_and(|frame| frame.same_as(report.to)) {
            return Reading::NotZoom;
        }
        self.overruled = Some(report.to);
        Reading::Zoom(Zoom::Rest)
    }
}

/// What `report` of `window` means, given the last title-bar double-click. Spends the evidence when
/// it has expired or the window answered it some other way.
pub fn read(
    evidence: &mut Option<Evidence>,
    window: WindowId,
    report: &SizeReport,
    now: Instant,
) -> Option<Zoom> {
    let click = evidence.as_mut().filter(|click| click.window == window)?;
    match click.read(report, now) {
        Reading::Zoom(zoom) => Some(zoom),
        Reading::NotZoom => None,
        Reading::Spent => {
            *evidence = None;
            None
        }
    }
}

/// Whether only the top edge moved, and up: macOS stretching the edge a double-click landed on to
/// the top of the screen.
fn stretches_top_edge(from: CGRect, to: CGRect) -> bool {
    let bottom_left = |frame: CGRect| CGPoint::new(frame.origin.x, frame.max().y);
    to.origin.y < from.origin.y
        && bottom_left(to).same_as(bottom_left(from))
        && (to.size.width - from.size.width).abs() < 0.1
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn window(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    const COLUMN: CGRect = CGRect {
        origin: CGPoint { x: 8.0, y: 33.0 },
        size: CGSize { width: 997.0, height: 859.0 },
    };
    const DISPLAY: CGRect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize { width: 1440.0, height: 900.0 },
    };

    fn report(to: CGRect) -> SizeReport {
        SizeReport {
            from: COLUMN,
            to,
            pending: None,
            display: Some(DISPLAY),
            column: true,
        }
    }

    fn clicked(at: Instant) -> Option<Evidence> {
        Some(Evidence::new(window(1), at))
    }

    #[test]
    fn a_double_click_in_the_top_band_is_on_that_window_s_title_bar() {
        let windows = [
            (window(1), COLUMN),
            (window(2), rect(1013.0, 33.0, 419.0, 859.0)),
        ];
        let at = |x, y| title_bar_under(CGPoint::new(x, y), windows);
        assert_eq!(at(500.0, 33.0), Some(window(1)), "the top edge");
        assert_eq!(at(500.0, 60.0), Some(window(1)), "a unified toolbar");
        assert_eq!(at(500.0, 92.9), Some(window(1)), "the band's last point");
        assert_eq!(at(1200.0, 45.0), Some(window(2)));
        assert_eq!(at(500.0, 93.0), None, "content");
        assert_eq!(at(1009.0, 45.0), None, "the gap between two columns");
        assert_eq!(at(500.0, 20.0), None, "above every window");
    }

    #[test]
    fn a_point_on_two_title_bars_is_on_neither() {
        let overlapping = [
            (window(1), COLUMN),
            (window(2), rect(400.0, 40.0, 600.0, 300.0)),
        ];
        assert_eq!(title_bar_under(CGPoint::new(500.0, 50.0), overlapping), None);
    }

    #[test]
    fn the_first_size_change_is_the_zoom_whatever_size_the_app_chose() {
        let at = Instant::now();
        let to_the_screen = rect(0.0, 25.0, 1440.0, 875.0);
        let to_its_content = rect(140.0, 40.0, 1150.0, 700.0);
        let smaller = rect(8.0, 33.0, 600.0, 500.0);
        for zoomed in [to_the_screen, to_its_content, smaller] {
            let mut evidence = clicked(at);
            assert_eq!(
                read(&mut evidence, window(1), &report(zoomed), at),
                Some(Zoom::Toggle),
                "{zoomed:?}"
            );
        }
    }

    #[test]
    fn after_the_toggle_every_new_frame_is_the_rest_of_the_zoom_until_the_app_holds_one() {
        let at = Instant::now();
        let mut evidence = clicked(at);
        let screen = rect(0.0, 25.0, 1440.0, 875.0);
        assert_eq!(
            read(&mut evidence, window(1), &report(screen), at),
            Some(Zoom::Toggle)
        );

        let full = rect(8.0, 33.0, 1424.0, 859.0);
        let rest = SizeReport { from: full, ..report(screen) };
        assert_eq!(read(&mut evidence, window(1), &rest, at), Some(Zoom::Rest));
        assert_eq!(
            read(&mut evidence, window(1), &rest, at),
            None,
            "put back after rini overruled it: the app holds it"
        );
        let other = SizeReport {
            to: rect(0.0, 25.0, 1430.0, 870.0),
            ..rest
        };
        assert_eq!(read(&mut evidence, window(1), &other, at), Some(Zoom::Rest));
        assert!(evidence.is_some());
    }

    #[test]
    fn a_double_click_is_believed_for_a_second_from_the_event_and_no_longer() {
        let at = Instant::now();
        let zoomed = report(rect(0.0, 25.0, 1440.0, 875.0));
        let mut evidence = clicked(at);
        assert_eq!(
            read(
                &mut evidence,
                window(1),
                &zoomed,
                at + Duration::from_millis(999)
            ),
            Some(Zoom::Toggle)
        );
        let mut evidence = clicked(at);
        assert_eq!(
            read(&mut evidence, window(1), &zoomed, at + Duration::from_secs(1)),
            None
        );
        assert_eq!(evidence, None, "spent");
    }

    #[test]
    fn a_report_of_another_window_leaves_the_evidence_alone() {
        let at = Instant::now();
        let mut evidence = clicked(at);
        let zoomed = report(rect(0.0, 25.0, 1440.0, 875.0));
        assert_eq!(read(&mut evidence, window(2), &zoomed, at), None);
        assert_eq!(evidence, clicked(at));
        assert_eq!(read(&mut None, window(1), &zoomed, at), None);
    }

    #[test]
    fn what_is_not_a_zoom_leaves_it_to_come() {
        let at = Instant::now();
        let zoomed = rect(0.0, 25.0, 1440.0, 875.0);
        let moved = rect(100.0, 33.0, 997.0, 859.0);
        let cases = [
            (
                "floating",
                SizeReport {
                    column: false,
                    ..report(zoomed)
                },
            ),
            ("moved, not resized", report(moved)),
            (
                "rini's own write",
                SizeReport {
                    pending: Some(zoomed),
                    ..report(zoomed)
                },
            ),
        ];
        for (case, report) in cases {
            let mut evidence = clicked(at);
            assert_eq!(read(&mut evidence, window(1), &report, at), None, "{case}");
            assert_eq!(evidence, clicked(at), "{case}");
        }
    }

    #[test]
    fn native_fullscreen_and_a_stretched_top_edge_spend_it() {
        let at = Instant::now();
        let stretched = rect(8.0, 25.0, 997.0, 867.0);
        for (case, to) in [("native fullscreen", DISPLAY), ("the top edge", stretched)] {
            let mut evidence = clicked(at);
            assert_eq!(read(&mut evidence, window(1), &report(to), at), None, "{case}");
            assert_eq!(evidence, None, "{case}");
        }
    }
}
