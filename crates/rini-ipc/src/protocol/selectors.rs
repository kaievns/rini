use serde::{Deserialize, Serialize};

use crate::Direction;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkspaceSelector {
    Index(usize),
    Name(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreScope {
    Workspace,
    Space,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreSource {
    #[default]
    SavedActiveSpace,
    CurrentSpace,
}

/// Stepping through the displays rather than naming one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelativeDisplay {
    /// The display after this one in physical order, wrapping to the first.
    ///
    /// Wrapping is the point: with two displays one key goes back and forth, so there is no
    /// `Previous` until somebody has three.
    Next,
}

/// Which display a command means.
///
/// `Relative` MUST come before `Uuid`. The enum is untagged, so serde tries the variants in
/// declaration order and `Uuid(String)` accepts ANY string: with `Uuid` first, `selector = "next"`
/// parsed as a display whose UUID is literally "next", found nothing, and the command did nothing at
/// all. That is how the `move_window_to_display` binding sat dead.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DisplaySelector {
    Direction(Direction),
    Relative(RelativeDisplay),
    Index(usize),
    Uuid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect this variant exists for. `Uuid(String)` accepts anything, so the keyword has to be
    /// tried first or it is read as the name of a display nobody has.
    #[test]
    fn next_is_a_keyword_and_not_the_name_of_a_display() {
        let parsed: DisplaySelector = serde_json::from_str("\"next\"").expect("parses");
        assert_eq!(parsed, DisplaySelector::Relative(RelativeDisplay::Next));
    }

    /// Everything else still parses as what it looks like, so putting the keyword first has not
    /// shadowed the other three.
    #[test]
    fn the_other_selectors_are_unaffected() {
        let uuid: DisplaySelector = serde_json::from_str("\"37D8832A-2D66\"").expect("parses");
        assert_eq!(uuid, DisplaySelector::Uuid("37D8832A-2D66".to_owned()));

        let index: DisplaySelector = serde_json::from_str("1").expect("parses");
        assert_eq!(index, DisplaySelector::Index(1));

        let direction: DisplaySelector = serde_json::from_str("\"right\"").expect("parses");
        assert_eq!(direction, DisplaySelector::Direction(Direction::Right));
    }

    #[test]
    fn a_selector_survives_a_round_trip() {
        for selector in [
            DisplaySelector::Relative(RelativeDisplay::Next),
            DisplaySelector::Index(0),
            DisplaySelector::Uuid("abc".to_owned()),
            DisplaySelector::Direction(Direction::Left),
        ] {
            let json = serde_json::to_string(&selector).expect("serialises");
            let back: DisplaySelector = serde_json::from_str(&json).expect("parses");
            assert_eq!(back, selector, "{json}");
        }
    }
}
