//! How each piece of the bar is set: face, size and colour, from the Okibi tokens.
//!
//! Every state is a named step on the neutral spine. Ember buys exactly one thing, the workspace a
//! display shows.

use super::layout::Piece;
use super::model::{DisplayBar, Row};
use super::palette::{Colour, EMBER, N6, N7, N8, N9, N10, N11};

/// A font by PostScript name, at a size in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Face {
    pub name: &'static str,
    pub size: f64,
}

/// Ioskeley Mono Term, the face the terminal uses, so bar and terminal share one. Its real weights
/// are named because the design system specifies weights by number: 400, 500, 600.
const REGULAR: &str = "IoskeleyMonoTermNF";
const MEDIUM: &str = "IoskeleyMonoTermNF-Medium";
const SEMIBOLD: &str = "IoskeleyMonoTermNF-SemiBold";
/// sketchybar-app-font, whose ligatures draw application marks. See `glyphs.rs`.
const APP_FONT: &str = "sketchybar-app-font";

/// The disclosure glyph while the tray is open. Font Awesome's chevron-right, in Ioskeley's Nerd Font
/// set: it points the way the icons go when the tray closes.
pub const CHEVRON_OPEN: &str = "\u{f054}";
/// While the tray is closed: chevron-left, the way it will open.
pub const CHEVRON_CLOSED: &str = "\u{f053}";
/// Between the application and its window's title.
pub const FOCUS_DOT: &str = "·";

pub fn face(piece: Piece, bar: &DisplayBar) -> Face {
    let (name, size) = match piece {
        Piece::Numeral(row) if bar.rows.get(row) == Some(&Row::Shown) => (SEMIBOLD, 14.0),
        Piece::Numeral(_) => (MEDIUM, 14.0),
        Piece::Glyph(_) => (APP_FONT, 14.0),
        Piece::More => (REGULAR, 14.0),
        Piece::FocusApp => (MEDIUM, 12.0),
        Piece::FocusDot | Piece::FocusTitle => (REGULAR, 12.0),
        Piece::Time => (SEMIBOLD, 13.0),
        Piece::Date => (REGULAR, 13.0),
        Piece::Chevron => (REGULAR, 12.0),
        Piece::PlaceDivider | Piece::ClockDivider | Piece::Vital(_) | Piece::Tray(_) => (REGULAR, 12.0),
    };
    Face { name, size }
}

pub fn colour(piece: Piece, bar: &DisplayBar) -> Colour {
    match piece {
        Piece::Numeral(row) => match bar.rows.get(row) {
            Some(Row::Shown) => EMBER,
            Some(Row::Occupied) => N10,
            _ => N7,
        },
        Piece::Glyph(index) if bar.glyphs.get(index).is_some_and(|glyph| glyph.lit) => N11,
        Piece::Glyph(_) | Piece::More | Piece::FocusDot | Piece::Chevron => N8,
        Piece::FocusApp => N10,
        Piece::FocusTitle | Piece::Date => N9,
        Piece::Time => N11,
        // n6 rather than the spec's --line: --line is sized for a hairline on a content surface and
        // is nearly invisible on the bar's chrome.
        Piece::PlaceDivider | Piece::ClockDivider => N6,
        Piece::Vital(_) | Piece::Tray(_) => N11,
    }
}

/// The underline under the shown workspace.
pub const UNDERLINE: Colour = EMBER;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::domain::model::Glyph;
    use rini_core::ids::WindowId;

    fn bar() -> DisplayBar {
        DisplayBar {
            uuid: "d".into(),
            screen: 1,
            rows: vec![Row::Occupied, Row::Shown, Row::Empty],
            glyphs: vec![
                Glyph { app: "Code".into(), window: WindowId::new(1, 1), lit: true },
                Glyph { app: "Zen".into(), window: WindowId::new(2, 2), lit: false },
            ],
            focus: None,
        }
    }

    /// Ember and the heavier weight mark the shown workspace, and nothing else.
    #[test]
    fn the_shown_workspace_alone_is_ember() {
        let bar = bar();
        assert_eq!(colour(Piece::Numeral(1), &bar), EMBER);
        assert_eq!(face(Piece::Numeral(1), &bar).name, SEMIBOLD);
        assert_eq!(colour(Piece::Numeral(0), &bar), N10);
        assert_eq!(colour(Piece::Numeral(2), &bar), N7);
        assert_eq!(face(Piece::Numeral(0), &bar).name, MEDIUM);
    }

    #[test]
    fn the_focused_application_is_the_bright_glyph() {
        let bar = bar();
        assert_eq!(colour(Piece::Glyph(0), &bar), N11);
        assert_eq!(colour(Piece::Glyph(1), &bar), N8);
        assert_eq!(face(Piece::Glyph(0), &bar).name, APP_FONT);
    }

    /// The time is what you look for; the date is context.
    #[test]
    fn the_time_outranks_the_date() {
        let bar = bar();
        assert_eq!((colour(Piece::Time, &bar), face(Piece::Time, &bar).name), (N11, SEMIBOLD));
        assert_eq!((colour(Piece::Date, &bar), face(Piece::Date, &bar).name), (N9, REGULAR));
    }
}
