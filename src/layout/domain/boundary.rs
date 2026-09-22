//! What a swipe means once the strip has run out.
//!
//! A horizontal swipe scrolls the strip. When the strip has no more columns in that direction the
//! gesture has somewhere else to go: the workspace stack. Turning the edge that was hit into the
//! workspace step it implies is two nested matches with an inversion flag, which is where a
//! left/right mix-up hides.

use rini_ipc::protocol::LayoutCommand;

use crate::layout::Direction;

/// The workspace step a strip edge implies, or `None` if the edge is not a horizontal one.
///
/// Left means the previous workspace and right the next, which `invert_horizontal` swaps. Vertical
/// edges produce nothing: the workspace stack is what a vertical swipe already moves through, so
/// propagating one would fight the gesture that caused it.
pub fn workspace_step_at_boundary(
    direction: Direction,
    invert_horizontal: bool,
    skip_empty: bool,
) -> Option<LayoutCommand> {
    let previous = LayoutCommand::PrevWorkspace(Some(skip_empty));
    let next = LayoutCommand::NextWorkspace(Some(skip_empty));
    match (direction, invert_horizontal) {
        (Direction::Left, false) | (Direction::Right, true) => Some(previous),
        (Direction::Right, false) | (Direction::Left, true) => Some(next),
        (Direction::Up | Direction::Down, _) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_left_edge_steps_to_the_previous_workspace() {
        assert_eq!(
            workspace_step_at_boundary(Direction::Left, false, false),
            Some(LayoutCommand::PrevWorkspace(Some(false)))
        );
        assert_eq!(
            workspace_step_at_boundary(Direction::Right, false, false),
            Some(LayoutCommand::NextWorkspace(Some(false)))
        );
    }

    #[test]
    fn inverting_the_horizontal_axis_swaps_the_two() {
        assert_eq!(
            workspace_step_at_boundary(Direction::Left, true, false),
            Some(LayoutCommand::NextWorkspace(Some(false)))
        );
        assert_eq!(
            workspace_step_at_boundary(Direction::Right, true, false),
            Some(LayoutCommand::PrevWorkspace(Some(false)))
        );
    }

    /// A vertical swipe already moves through the workspace stack. Propagating a vertical edge would
    /// step it a second time.
    #[test]
    fn a_vertical_edge_propagates_nothing() {
        for invert in [false, true] {
            assert_eq!(workspace_step_at_boundary(Direction::Up, invert, false), None);
            assert_eq!(workspace_step_at_boundary(Direction::Down, invert, false), None);
        }
    }

    #[test]
    fn skip_empty_is_carried_through_unchanged() {
        assert_eq!(
            workspace_step_at_boundary(Direction::Left, false, true),
            Some(LayoutCommand::PrevWorkspace(Some(true)))
        );
    }

    /// Every horizontal case answers, and the two directions never answer the same way.
    #[test]
    fn the_two_horizontal_edges_never_agree() {
        for invert in [false, true] {
            let left = workspace_step_at_boundary(Direction::Left, invert, false);
            let right = workspace_step_at_boundary(Direction::Right, invert, false);
            assert!(left.is_some() && right.is_some());
            assert_ne!(left, right, "invert_horizontal = {invert}");
        }
    }
}
