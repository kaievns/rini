//! What a swipe means once the strip has run out.
//!
//! A horizontal swipe scrolls the strip. When the strip has no more columns in that direction the
//! gesture has somewhere else to go: the workspace stack. Turning the edge that was hit into the
//! workspace step it implies is two nested matches with an inversion flag, which is where a
//! left/right mix-up hides.

use rini_ipc::protocol::{LayoutCommand, WorkspaceSelector};

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

/// The strip edge a blocked command ran into, or `None` when the direction is not a strip axis.
///
/// One answer to two questions, because they are the same question. `Some` means the command ran out
/// of strip: it stops here, the view bounces, and it MUST NOT continue onto the next display. `None`
/// means the direction is not along a strip at all, so there is no edge to report and the adjacent
/// display is a legitimate destination.
///
/// **Each display is its own strip. Always.** Going right from the rightmost column stops, whether it
/// is focus moving or a window. This was a setting (`isolate_displays`, default off) and is not any
/// more: the behaviour it turned off — horizontal movement silently hopping displays mid-strip — is
/// not one anybody wanted, and having the option meant two code paths where one of them was never
/// used. Moving a window did not consult the setting at all, so a window pushed past the last column
/// teleported to the other monitor while focus in the same direction stopped.
///
/// Up and down are deliberately NOT strip axes. They move through the workspace stack, so a column's
/// top is not an edge the surface can give against, and bouncing the strip vertically for one would
/// claim the stack had stopped. Vertical movement between displays keeps working; treating all four
/// directions alike would silently disable it.
pub fn strip_edge(direction: Direction) -> Option<Direction> {
    matches!(direction, Direction::Left | Direction::Right).then_some(direction)
}

/// Which way along the workspace stack a `next`/`prev` request goes, for anything that has to name
/// the edge it ran into. `None` for a selector that names a workspace outright, because failing to
/// find one is not an edge of anything.
///
/// Workspaces stack downward: `next` is below, `prev` above. The same orientation
/// `handle_virtual_workspace_command` reports when a switch has nowhere to go.
pub fn workspace_stack_direction(selector: &WorkspaceSelector) -> Option<Direction> {
    match selector {
        WorkspaceSelector::Name(name) if name == "next" => Some(Direction::Down),
        WorkspaceSelector::Name(name) if name == "prev" => Some(Direction::Up),
        WorkspaceSelector::Name(_) | WorkspaceSelector::Index(_) => None,
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

    /// Every display is its own strip, with nothing to configure: a horizontal end is always an end,
    /// so it always stops and always bounces.
    #[test]
    fn a_horizontal_end_is_always_an_end() {
        assert_eq!(strip_edge(Direction::Left), Some(Direction::Left));
        assert_eq!(strip_edge(Direction::Right), Some(Direction::Right));
    }

    /// The other half, and the one worth guarding: up and down must stay crossable. Answering `Some`
    /// for them would make every vertical command an edge and cut the displays apart entirely.
    #[test]
    fn a_vertical_command_is_not_a_strip_end() {
        assert_eq!(strip_edge(Direction::Up), None);
        assert_eq!(strip_edge(Direction::Down), None);
    }

    /// Workspaces stack downward, so `next` is an edge at the BOTTOM and `prev` one at the top.
    #[test]
    fn a_relative_workspace_request_names_the_edge_it_can_run_into() {
        assert_eq!(
            workspace_stack_direction(&WorkspaceSelector::Name("next".to_owned())),
            Some(Direction::Down)
        );
        assert_eq!(
            workspace_stack_direction(&WorkspaceSelector::Name("prev".to_owned())),
            Some(Direction::Up)
        );
    }

    /// Asking for workspace 7 when there are four is a request that cannot be honoured, not a push
    /// against the end of the stack. Bouncing for it would say the stack has an edge in a direction
    /// the user never named.
    #[test]
    fn naming_a_workspace_outright_is_not_a_direction() {
        assert_eq!(workspace_stack_direction(&WorkspaceSelector::Index(7)), None);
        assert_eq!(
            workspace_stack_direction(&WorkspaceSelector::Name("scratch".to_owned())),
            None
        );
    }
}
