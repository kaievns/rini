use serde::{Deserialize, Serialize};

use crate::Direction;

/// Which workspace a command means.
///
/// `Relative` MUST come before `Name`, for the reason `DisplaySelector` gives: untagged, `Name`
/// accepts any string, so `"next"` would be read as the name of a workspace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkspaceSelector {
    Index(usize),
    Relative(RelativeWorkspace),
    Name(String),
}

/// Stepping through the workspace stack rather than naming a workspace. Neither step wraps.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelativeWorkspace {
    /// The workspace below this one.
    Next,
    /// The workspace above this one.
    Prev,
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
/// parses as a display whose UUID is literally "next", finds nothing, and the command does nothing
/// at all.
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

    /// `Uuid(String)` accepts anything, so the keyword has to be tried first or it is read as the
    /// name of a display nobody has.
    #[test]
    fn next_is_a_keyword_and_not_the_name_of_a_display() {
        let parsed: DisplaySelector = serde_json::from_str("\"next\"").expect("parses");
        assert_eq!(parsed, DisplaySelector::Relative(RelativeDisplay::Next));
    }

    /// Everything else parses as what it looks like: the keyword first shadows none of the other
    /// three.
    #[test]
    fn the_other_selectors_are_unaffected() {
        let uuid: DisplaySelector = serde_json::from_str("\"37D8832A-2D66\"").expect("parses");
        assert_eq!(uuid, DisplaySelector::Uuid("37D8832A-2D66".to_owned()));

        let index: DisplaySelector = serde_json::from_str("1").expect("parses");
        assert_eq!(index, DisplaySelector::Index(1));

        let direction: DisplaySelector = serde_json::from_str("\"right\"").expect("parses");
        assert_eq!(direction, DisplaySelector::Direction(Direction::Right));
    }

    /// On the wire a step is a plain string, so a client sending `"next"` reaches the step and any
    /// other string, "Next" included, reaches a name.
    #[test]
    fn a_workspace_step_is_a_keyword_and_anything_else_is_a_name() {
        let parse = |json: &str| serde_json::from_str::<WorkspaceSelector>(json).expect("parses");
        assert_eq!(
            parse("\"next\""),
            WorkspaceSelector::Relative(RelativeWorkspace::Next)
        );
        assert_eq!(
            parse("\"prev\""),
            WorkspaceSelector::Relative(RelativeWorkspace::Prev)
        );
        assert_eq!(parse("\"Next\""), WorkspaceSelector::Name("Next".to_owned()));
        assert_eq!(parse("\"web\""), WorkspaceSelector::Name("web".to_owned()));
        assert_eq!(parse("2"), WorkspaceSelector::Index(2));
        for selector in [
            WorkspaceSelector::Relative(RelativeWorkspace::Next),
            WorkspaceSelector::Relative(RelativeWorkspace::Prev),
            WorkspaceSelector::Name("web".to_owned()),
            WorkspaceSelector::Index(0),
        ] {
            let json = serde_json::to_string(&selector).expect("serialises");
            assert_eq!(parse(&json), selector, "{json}");
        }
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
