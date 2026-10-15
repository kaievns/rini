//! The ends of the strip.
//!
//! Which direction names which end. A command that runs out of strip stops at its end, rather than
//! carrying on onto the next display, and the view bounces.

use crate::layout::Direction;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
