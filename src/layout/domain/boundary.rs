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

/// Whether a command stops at the end of this display's strip instead of continuing onto the next
/// one.
///
/// `isolate_displays` makes each display its own scrollable strip, which is what a user with two
/// monitors usually wants: going right from the rightmost column should stop, not jump.
///
/// It governs MOVING a window as well as moving focus. Only focus consulted it at first, so a window
/// pushed past the last column teleported to the other monitor while focus in the same direction
/// stopped — the same key, two answers. What the strip's end means cannot depend on whether a window
/// is coming along.
///
/// It applies to the horizontal axis only. Up and down are not strip axes — they move through the
/// workspace stack — so there is no strip to isolate and the setting has nothing to say. Applying it
/// to all four directions would silently disable vertical navigation between displays.
pub fn stays_on_this_display(isolate_displays: bool, direction: Direction) -> bool {
    isolate_displays && matches!(direction, Direction::Left | Direction::Right)
}

/// The strip edge a blocked horizontal command ran into, or `None` for a vertical one.
///
/// Up and down are not strip axes: a column's top is not an edge the strip surface can give against,
/// and bouncing the whole strip vertically for one would claim the workspace stack had stopped. Both
/// the focus path and the move path ask this, so the two cannot answer differently about the same key.
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

    #[test]
    fn isolating_displays_stops_horizontal_movement_at_the_strip_s_end() {
        assert!(stays_on_this_display(true, Direction::Left));
        assert!(stays_on_this_display(true, Direction::Right));
    }

    /// Up and down move through the workspace stack, not along a strip, so there is nothing to
    /// isolate. Applying the setting to all four directions would disable vertical navigation
    /// between displays without anybody asking for that.
    #[test]
    fn isolating_displays_never_stops_vertical_movement() {
        assert!(!stays_on_this_display(true, Direction::Up));
        assert!(!stays_on_this_display(true, Direction::Down));
    }

    #[test]
    fn nothing_is_held_back_when_displays_are_not_isolated() {
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            assert!(!stays_on_this_display(false, direction));
        }
    }

    #[test]
    fn only_the_horizontal_ends_are_strip_edges() {
        assert_eq!(strip_edge(Direction::Left), Some(Direction::Left));
        assert_eq!(strip_edge(Direction::Right), Some(Direction::Right));
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
