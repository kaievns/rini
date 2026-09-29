//! Which glyph stands for an application on the bar.
//!
//! The glyphs are ligatures in sketchybar-app-font: the string `:ghostty:` set in that face draws
//! Ghostty's mark. `app_glyphs.tsv` maps an application's name to its token, and was generated from
//! sketchybar-app-font's own table (`helpers/app_icons.lua` in `~/.config/sketchybar`). Localised
//! names are separate rows, as they are there.

use std::collections::HashMap;
use std::sync::OnceLock;

const TABLE: &str = include_str!("app_glyphs.tsv");

/// What an application with no row of its own is drawn as.
pub const DEFAULT: &str = ":default:";

fn table() -> &'static HashMap<&'static str, &'static str> {
    static PARSED: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    PARSED.get_or_init(|| TABLE.lines().filter_map(|line| line.split_once('\t')).collect())
}

/// The ligature token for `app`, by its localised name.
pub fn token(app: &str) -> &'static str {
    table().get(app).copied().unwrap_or(DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_applications_have_their_own_glyph() {
        assert_eq!(token("Ghostty"), ":ghostty:");
        assert_eq!(token("Code"), ":code:");
        assert_eq!(token("Zen"), ":zen_browser:");
        assert_eq!(token("zoom.us"), ":zoom:");
    }

    /// A localised name is its own row.
    #[test]
    fn a_localised_name_finds_the_same_glyph() {
        assert_eq!(token("Aktivitätsanzeige"), token("Activity Monitor"));
    }

    #[test]
    fn anything_else_gets_the_default() {
        assert_eq!(token("Some App Nobody Has"), DEFAULT);
        assert_eq!(token(""), DEFAULT);
    }

    /// Every row is a name and a `:token:`, so a bad regeneration fails here and not as a blank
    /// glyph on the bar.
    #[test]
    fn every_row_is_well_formed() {
        for line in TABLE.lines() {
            let (name, token) = line.split_once('\t').expect("a tab in every row");
            assert!(!name.is_empty(), "{line:?}");
            assert!(token.len() > 2 && token.starts_with(':') && token.ends_with(':'), "{line:?}");
        }
        assert!(table().len() > 800);
    }
}
