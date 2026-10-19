use serde::{Deserialize, Serialize};

use super::selectors::RelativeWorkspace;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// An end of the strip: its first column or its last.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StripEnd {
    First,
    Last,
}

impl StripEnd {
    /// The way along the strip this end lies.
    pub fn direction(self) -> Direction {
        match self {
            StripEnd::First => Direction::Left,
            StripEnd::Last => Direction::Right,
        }
    }
}

/// Up or down: the way along a column, and on through the workspace stack where the column ends.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vertical {
    Up,
    Down,
}

impl Vertical {
    /// The way along the column.
    pub fn direction(self) -> Direction {
        match self {
            Vertical::Up => Direction::Up,
            Vertical::Down => Direction::Down,
        }
    }

    /// The step through the stack: the workspace above is the previous one.
    pub fn workspace(self) -> RelativeWorkspace {
        match self {
            Vertical::Up => RelativeWorkspace::Prev,
            Vertical::Down => RelativeWorkspace::Next,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeOrientation {
    #[default]
    Horizontal,
    Vertical,
    Smart,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutKind {
    #[default]
    Horizontal,
    Vertical,
}
