//! The strip is one z-order group.
//!
//! A scrolling workspace is a single surface, so its windows belong together in front-to-back order as
//! well as in position: focusing any window on the strip brings the whole strip in front of the windows
//! that are not on it, and focusing one of those puts it in front of the whole strip.
//!
//! macOS has no such notion. It raises the one window that was clicked, which leaves a floating window
//! sandwiched between two columns that sit side by side on screen — so one half of a 50/50 pair is in
//! front of it and the other half behind.
//!
//! The same rule decides two different things: which tiles the animation overlay draws in front, and
//! which real windows have to be raised to put the order back.

/// Which z-order group a window belongs to.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum StackGroup {
    /// On the strip, and therefore part of the group that moves as one.
    Strip,
    /// Off the strip: floating, or otherwise not part of the scrolling surface.
    Floating,
}

/// Room for every window of one group before the next group starts, so no member of the group behind can
/// ever be drawn in front of a member of the group in front.
///
/// Public because the overlay derives its backdrop depth from it: the deepest possible tile is just
/// short of two strides, and the backdrop has to sit behind THAT, not behind some smaller constant.
/// The floating group's tiles used to land at zPosition about -(1<<20) while the backdrop sat at
/// -10000, so every floating tile was drawn behind the desktop picture — present in every
/// composition and visible in none.
pub const GROUP_STRIDE: usize = 1 << 20;

/// The deepest depth `tile_depth` can produce: the unreported-window fallback of the back group.
pub const MAX_TILE_DEPTH: usize = 2 * GROUP_STRIDE - 1;

/// Front-to-back position for a tile, 0 being frontmost.
///
/// Three bands: the window gaining focus, then the rest of ITS group, then the other group. Within a band
/// the window server's own order is kept, since that is right for windows that really do overlap.
///
/// A window the server did not report sorts to the back of its own band rather than the back of everything:
/// a tile drawn too far back inside its group is invisible, while one drawn in the wrong group is the bug
/// this exists to prevent.
pub fn tile_depth(
    server_order: Option<usize>,
    gaining_focus: bool,
    group: StackGroup,
    focused_group: StackGroup,
) -> usize {
    if gaining_focus {
        return 0;
    }
    // Saturating: the server's order is untrusted input, and `usize::MAX + 1` is a debug-build
    // abort for a value that only needed to mean "the back of the band".
    let within = server_order
        .map(|order| order.saturating_add(1))
        .unwrap_or(GROUP_STRIDE - 1)
        .min(GROUP_STRIDE - 1);
    if group == focused_group { within } else { GROUP_STRIDE + within }
}

/// Whether the real window order breaks the rule, given the groups front to back.
///
/// It is broken as soon as something off the strip sits in front of something on it. Checked before doing
/// anything about it, because putting it back costs one Accessibility raise per window on screen, and a
/// click that lands on an order which is already grouped should cost nothing.
pub fn strip_is_behind(front_to_back: &[StackGroup]) -> bool {
    let first_floating = front_to_back.iter().position(|group| *group == StackGroup::Floating);
    match first_floating {
        Some(floating) => front_to_back[floating..].contains(&StackGroup::Strip),
        None => false,
    }
}

/// The windows to raise to put the strip back in front, back to front.
///
/// Empty when the order already obeys the rule, which is the common case: putting it back costs one
/// Accessibility raise per window on screen, so it is only worth doing when something is actually wrong.
///
/// Back to front because everything raised in one sequence ends up in front of everything not raised, and
/// within the sequence the last one raised is the frontmost. The windows that are NOT on the strip are left
/// out entirely rather than raised first: their order relative to each other is not this rule's business,
/// and leaving them alone is what puts them behind.
pub fn strip_regroup<T: Copy>(front_to_back: &[(T, StackGroup)]) -> Vec<T> {
    let groups: Vec<StackGroup> = front_to_back.iter().map(|(_, group)| *group).collect();
    if !strip_is_behind(&groups) {
        return Vec::new();
    }
    front_to_back
        .iter()
        .rev()
        .filter(|(_, group)| *group == StackGroup::Strip)
        .map(|(window, _)| *window)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::StackGroup::{Floating, Strip};
    use super::*;

    /// The measured order after clicking the left half of a 50/50 pair: the clicked terminal, then the
    /// floating Settings window, then the rest of the strip. Every strip window has to be raised, and the
    /// clicked one has to end up last so it stays in front.
    #[test]
    fn regrouping_raises_the_whole_strip_back_to_front() {
        let order = [(90, Strip), (5830, Floating), (89, Strip), (91, Strip)];
        assert_eq!(strip_regroup(&order), vec![91, 89, 90]);
    }

    #[test]
    fn an_order_that_already_obeys_the_rule_is_left_alone() {
        assert!(strip_regroup(&[(90, Strip), (91, Strip), (5830, Floating)]).is_empty());
        assert!(strip_regroup(&[(90, Strip), (91, Strip)]).is_empty());
        assert!(strip_regroup::<i32>(&[]).is_empty());
    }

    /// The floating windows are left out rather than raised first. Raising them in the same sequence would
    /// leave their order against the strip up to whichever app answered first.
    #[test]
    fn regrouping_never_raises_a_window_off_the_strip() {
        let order = [(5830, Floating), (1350, Floating), (90, Strip)];
        assert_eq!(strip_regroup(&order), vec![90]);
    }

    /// The measured case: clicking the left half of a 50/50 pair left the floating Settings window between
    /// the two terminals, in front of one and behind the other.
    #[test]
    fn a_floating_window_in_front_of_any_strip_window_breaks_the_rule() {
        assert!(strip_is_behind(&[Strip, Floating, Strip, Strip]));
        assert!(strip_is_behind(&[Floating, Strip]));
        assert!(strip_is_behind(&[Strip, Strip, Floating, Strip]));
    }

    #[test]
    fn the_whole_strip_in_front_of_the_floating_windows_is_the_rule_kept() {
        assert!(!strip_is_behind(&[Strip, Strip, Strip, Floating, Floating]));
        assert!(!strip_is_behind(&[Strip, Floating]));
    }

    #[test]
    fn an_order_with_only_one_kind_of_window_is_never_broken() {
        assert!(!strip_is_behind(&[Strip, Strip, Strip]));
        assert!(!strip_is_behind(&[Floating, Floating]));
        assert!(!strip_is_behind(&[]));
    }

    /// Focusing either half of a 50/50 pair has to lift BOTH of them over the floating window, which is the
    /// whole point: they sit side by side on screen and cannot be on opposite sides of it.
    #[test]
    fn focusing_one_strip_window_puts_its_whole_group_in_front() {
        let focused = tile_depth(Some(0), true, Strip, Strip);
        let partner = tile_depth(Some(3), false, Strip, Strip);
        let settings = tile_depth(Some(1), false, Floating, Strip);
        assert!(focused < partner, "the focused window leads its group");
        assert!(partner < settings, "and its partner still beats the floating window");
    }

    /// The converse, which macOS already does: a floating window that takes focus goes in front of the
    /// entire strip, not just the column it happens to overlap.
    #[test]
    fn focusing_a_floating_window_puts_it_in_front_of_the_whole_strip() {
        let settings = tile_depth(Some(0), true, Floating, Floating);
        let nearest_column = tile_depth(Some(1), false, Strip, Floating);
        let far_column = tile_depth(Some(9), false, Strip, Floating);
        assert!(settings < nearest_column);
        assert!(nearest_column < far_column, "the strip keeps its own order behind it");
    }

    #[test]
    fn within_a_group_the_window_servers_order_is_kept() {
        assert!(tile_depth(Some(0), false, Strip, Strip) < tile_depth(Some(1), false, Strip, Strip));
        assert!(tile_depth(Some(1), false, Strip, Strip) < tile_depth(Some(17), false, Strip, Strip));
        assert!(
            tile_depth(Some(0), false, Floating, Strip) < tile_depth(Some(1), false, Floating, Strip)
        );
    }

    /// A window the server did not report must not fall out of its group: behind its own kind, still in
    /// front of the group that is meant to be behind.
    #[test]
    fn an_unreported_window_stays_inside_its_own_group() {
        let unknown_strip = tile_depth(None, false, Strip, Strip);
        let known_strip = tile_depth(Some(50), false, Strip, Strip);
        let nearest_floating = tile_depth(Some(0), false, Floating, Strip);
        assert!(known_strip < unknown_strip, "behind the windows the server did report");
        assert!(unknown_strip < nearest_floating, "but still in front of the other group");
    }

    /// The stride has to outrun any plausible window count, or a deep window in the front group would wrap
    /// past a shallow one in the back group and the grouping would silently invert.
    #[test]
    fn no_window_count_can_make_the_groups_overlap() {
        let deepest_in_front = tile_depth(Some(usize::MAX), false, Strip, Strip);
        let shallowest_behind = tile_depth(Some(0), false, Floating, Strip);
        assert!(deepest_in_front < shallowest_behind);
    }
}
