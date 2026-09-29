//! Where each picture goes on a bar, so its ink lands on the span `layout` gave the piece.
//!
//! A string is drawn into a picture of its own ink plus a margin, and the picture is placed so the
//! ink's left edge lands on the span's start. Baseline text keeps its baseline on a whole pixel, so
//! a row of numerals shares one baseline however their ink heights differ; everything else is
//! snapped to the pixel grid, so no picture is resampled between pixels.

use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use super::layout::{
    GLYPH_CENTRE, HAIRLINE_HEIGHT, HAIRLINE_TOP, HEIGHT, NUMERAL_BASELINE, Piece, Span,
    TEXT_BASELINE, UNDERLINE_HEIGHT,
};

/// Room around a string's ink in its picture, for anti-aliasing that runs past the measured ink.
const MARGIN: f64 = 2.0;

/// A string's ink as AppKit measures it with device metrics: from the pen on the baseline, y up, so
/// `y` is the ink's lowest point, negative for a descender.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ink {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A picture of a string, in points from its top-left, y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sheet {
    /// Whole pixels at the scale it was made for.
    pub size: CGSize,
    /// Where the pen starts, on the baseline.
    pub pen: CGPoint,
    pub ink: CGRect,
}

pub fn sheet(ink: Ink, scale: f64) -> Sheet {
    let above = ink.y + ink.height;
    let baseline = whole_pixels(MARGIN + above, scale);
    Sheet {
        size: CGSize::new(
            whole_pixels(ink.width + 2.0 * MARGIN, scale),
            whole_pixels(baseline - ink.y + MARGIN, scale),
        ),
        pen: CGPoint::new(MARGIN - ink.x, baseline),
        ink: CGRect::new(
            CGPoint::new(MARGIN, baseline - above),
            CGSize::new(ink.width, ink.height),
        ),
    }
}

/// How a piece's ink is set vertically, in points from the bar's top edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    Baseline(f64),
    /// The ink's own middle, for marks that are not letters.
    Centre(f64),
}

pub fn anchor(piece: Piece) -> Anchor {
    match piece {
        Piece::Numeral(_) => Anchor::Baseline(NUMERAL_BASELINE),
        Piece::Glyph(_) | Piece::More => Anchor::Centre(GLYPH_CENTRE),
        _ => Anchor::Baseline(TEXT_BASELINE),
    }
}

/// Where a string's picture goes so its ink starts at `span.x0`. A span narrower than the ink is a
/// cut, and the picture stops at `span.x1`.
pub fn text_frame(sheet: &Sheet, span: Span, anchor: Anchor, scale: f64) -> CGRect {
    let x = snap(span.x0 - sheet.ink.origin.x, scale);
    let y = match anchor {
        Anchor::Baseline(baseline) => snap(baseline - sheet.pen.y, scale),
        Anchor::Centre(centre) => snap(
            centre - (sheet.ink.origin.y + sheet.ink.size.height / 2.0),
            scale,
        ),
    };
    let cut = span.width() < sheet.ink.size.width - 1e-6;
    let width = if cut {
        (snap(span.x1, scale) - x).max(0.0)
    } else {
        sheet.size.width
    };
    CGRect::new(CGPoint::new(x, y), CGSize::new(width, sheet.size.height))
}

/// Where a menu extra's picture goes: its ink from `span.x0`, the picture centred on the bar. The
/// pictures are about 33pt tall on the 32pt bar, and their ink sits well inside that.
pub fn extra_frame(size: CGSize, ink_start: f64, span: Span, scale: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(
            snap(span.x0 - ink_start, scale),
            snap((HEIGHT - size.height) / 2.0, scale),
        ),
        size,
    )
}

/// How wide a hairline between zones is, which is the ink `layout` is given for one.
pub const RULE_WIDTH: f64 = 1.0;

/// A hairline between zones.
pub fn rule_frame(span: Span, scale: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(snap(span.x0, scale), HAIRLINE_TOP),
        CGSize::new(snap(span.width(), scale), HAIRLINE_HEIGHT),
    )
}

/// The shown workspace's rule, flush with the bottom edge.
pub fn underline_frame(span: Span, scale: f64) -> CGRect {
    let (x0, x1) = (snap(span.x0, scale), snap(span.x1, scale));
    CGRect::new(
        CGPoint::new(x0, HEIGHT - UNDERLINE_HEIGHT),
        CGSize::new(x1 - x0, UNDERLINE_HEIGHT),
    )
}

/// What the tray is drawn through, full bar height: from `left`, its first picture's left edge, to
/// where the chevron's ink starts. Anything slid past its right edge is gone into the chevron.
pub fn tray_window(left: f64, chevron: Span, scale: f64) -> CGRect {
    let right = snap(chevron.x0, scale);
    CGRect::new(
        CGPoint::new(left, 0.0),
        CGSize::new((right - left).max(0.0), HEIGHT),
    )
}

/// The tray window's bounds: its content in place while open, and slid a whole window's width to
/// the right, past the chevron, while closed.
pub fn tray_bounds(open: bool, window: CGRect) -> CGRect {
    let shift = if open { 0.0 } else { -window.size.width };
    CGRect::new(CGPoint::new(shift, 0.0), window.size)
}

/// A bar's frame in CoreGraphics coordinates: the top strip of its display.
pub fn strip(display: CGRect) -> CGRect {
    CGRect::new(display.origin, CGSize::new(display.size.width, HEIGHT))
}

fn snap(value: f64, scale: f64) -> f64 {
    (value * scale).round() / scale
}

/// Rounded up to whole pixels, but not past one it already is on by a rounding error.
fn whole_pixels(value: f64, scale: f64) -> f64 {
    (value * scale - 1e-6).ceil() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALE: f64 = 2.0;

    /// Measured 2026-09-29: "1" and "2" in Ioskeley Mono Term Medium 14, "gy" in Regular 12.
    const ONE: Ink = Ink {
        x: 1.484,
        y: 0.0,
        width: 3.808,
        height: 9.660,
    };
    const TWO: Ink = Ink {
        x: 1.162,
        y: 0.0,
        width: 6.104,
        height: 9.772,
    };
    const GY: Ink = Ink {
        x: 0.924,
        y: -2.736,
        width: 12.660,
        height: 9.072,
    };
    /// `:ghostty:` in sketchybar-app-font 14.
    const GHOSTTY: Ink = Ink {
        x: 0.0,
        y: 0.0,
        width: 11.410,
        height: 13.902,
    };

    fn on_grid(value: f64) -> bool {
        (value * SCALE - (value * SCALE).round()).abs() < 1e-9
    }

    fn span(x0: f64, width: f64) -> Span {
        Span { x0, x1: x0 + width }
    }

    /// The pen is placed so the ink starts a margin in from the top-left, and the picture is whole
    /// pixels with room for the descender.
    #[test]
    fn a_sheet_holds_its_ink_inside_a_margin() {
        let sheet = sheet(GY, SCALE);
        assert_eq!(sheet.ink.origin.x, MARGIN);
        assert!(sheet.ink.origin.y >= MARGIN);
        assert!((sheet.pen.x + GY.x - sheet.ink.origin.x).abs() < 1e-9);
        assert!((sheet.pen.y - (sheet.ink.origin.y + GY.y + GY.height)).abs() < 1e-9);
        assert!(sheet.ink.max().y + MARGIN <= sheet.size.height);
        assert!(sheet.ink.max().x + MARGIN <= sheet.size.width);
        assert!(on_grid(sheet.size.width) && on_grid(sheet.size.height) && on_grid(sheet.pen.y));
    }

    /// Two numerals of different ink heights land on one baseline, on a whole pixel.
    #[test]
    fn numerals_share_one_baseline() {
        for ink in [ONE, TWO] {
            let sheet = sheet(ink, SCALE);
            let frame = text_frame(&sheet, span(16.0, ink.width), anchor(Piece::Numeral(0)), SCALE);
            assert_eq!(frame.origin.y + sheet.pen.y, NUMERAL_BASELINE);
        }
    }

    /// The ink's left edge lands on the span's start, to within half a pixel.
    #[test]
    fn the_ink_starts_where_its_span_does() {
        let sheet = sheet(TWO, SCALE);
        for x0 in [16.0, 41.3, 57.808] {
            let frame = text_frame(
                &sheet,
                span(x0, TWO.width),
                Anchor::Baseline(TEXT_BASELINE),
                SCALE,
            );
            assert!(
                (frame.origin.x + sheet.ink.origin.x - x0).abs() <= 0.5 / SCALE,
                "{x0}"
            );
            assert!(on_grid(frame.origin.x));
            assert_eq!(frame.size, sheet.size);
        }
    }

    /// Application marks are centred on their own ink rather than set on a baseline.
    #[test]
    fn a_glyph_is_centred_on_its_ink() {
        let sheet = sheet(GHOSTTY, SCALE);
        let frame = text_frame(&sheet, span(60.0, GHOSTTY.width), anchor(Piece::Glyph(0)), SCALE);
        let centre = frame.origin.y + sheet.ink.origin.y + GHOSTTY.height / 2.0;
        assert!((centre - GLYPH_CENTRE).abs() <= 0.5 / SCALE);
        assert!(on_grid(frame.origin.y));
        assert_eq!(anchor(Piece::More), Anchor::Centre(GLYPH_CENTRE));
        assert_eq!(anchor(Piece::Time), Anchor::Baseline(TEXT_BASELINE));
    }

    /// A title the layout cut short stops at the end of its span rather than running on under the
    /// right zone.
    #[test]
    fn a_cut_title_stops_at_its_span() {
        let ink = Ink {
            x: 0.5,
            y: -2.7,
            width: 300.0,
            height: 12.0,
        };
        let sheet = sheet(ink, SCALE);
        let frame = text_frame(
            &sheet,
            span(200.0, 120.0),
            Anchor::Baseline(TEXT_BASELINE),
            SCALE,
        );
        assert_eq!(frame.max().x, 320.0);
        let whole = text_frame(
            &sheet,
            span(200.0, 300.0),
            Anchor::Baseline(TEXT_BASELINE),
            SCALE,
        );
        assert_eq!(whole.size.width, sheet.size.width);
    }

    /// A 33pt picture on the 32pt bar hangs half a point over each edge, and its ink, not its
    /// picture, starts on the span.
    #[test]
    fn an_extra_is_centred_and_placed_by_its_ink() {
        let frame = extra_frame(CGSize::new(38.0, 33.0), 11.5, span(1400.0, 17.0), SCALE);
        assert_eq!(frame.origin, CGPoint::new(1388.5, -0.5));
        assert_eq!(frame.size, CGSize::new(38.0, 33.0));
    }

    #[test]
    fn hairlines_and_the_underline_keep_their_bands() {
        let rule = rule_frame(span(120.3, RULE_WIDTH), SCALE);
        assert_eq!(
            rule,
            CGRect::new(
                CGPoint::new(120.5, HAIRLINE_TOP),
                CGSize::new(RULE_WIDTH, HAIRLINE_HEIGHT)
            )
        );
        let underline = underline_frame(Span { x0: 36.1, x1: 98.8 }, SCALE);
        assert_eq!(underline.origin, CGPoint::new(36.0, HEIGHT - UNDERLINE_HEIGHT));
        assert_eq!(underline.max().y, HEIGHT);
        assert_eq!(underline.size.width, 63.0);
    }

    /// Closed, the tray's content sits a whole window to the right, so none of it shows; open, it
    /// is back in place. The window ends where the chevron's ink starts.
    #[test]
    fn the_tray_slides_a_whole_window_into_the_chevron() {
        let window = tray_window(1200.0, span(1330.2, 5.5), SCALE);
        assert_eq!(
            window,
            CGRect::new(CGPoint::new(1200.0, 0.0), CGSize::new(130.0, HEIGHT))
        );
        assert_eq!(tray_bounds(true, window).origin.x, 0.0);
        let closed = tray_bounds(false, window);
        assert_eq!(closed.origin.x, -130.0);
        assert_eq!(closed.size, window.size);
    }

    #[test]
    fn a_bar_is_the_top_strip_of_its_display() {
        let display = CGRect::new(CGPoint::new(-1920.0, -300.0), CGSize::new(1920.0, 1080.0));
        assert_eq!(
            strip(display),
            CGRect::new(display.origin, CGSize::new(1920.0, HEIGHT))
        );
    }
}
