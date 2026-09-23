//! What a query answers about when it was not told, and what the diagnostics dump counts as wrong.
//!
//! Three rules, all of which were written out inline in `query.rs` against live stores. The reads stay
//! there; the decisions are here, where they can be exercised without a reactor.

use rini_core::ids::{SpaceId, WindowId};

/// Which space a query means when it names none.
///
/// The workspace command space first, then the active display's, then the raw command space. That
/// order is the one a user expects: the space their last command acted on, then the one they are
/// looking at, then whatever macOS last called current.
///
/// The consequence of getting this wrong is recorded on `query_diagnostics`: a `query windows` with no
/// space answers about ONE space, so windows on the other display read as absent. That produced three
/// wrong conclusions in a row — that windows were missing, then dead, then unmanaged — when every one
/// of them was present and correctly placed. `query diagnostics` exists because of it, and this rule
/// is why it had to.
pub(crate) fn default_query_space(
    workspace_command: Option<SpaceId>,
    active_display: Option<SpaceId>,
    raw_command: Option<SpaceId>,
) -> Option<SpaceId> {
    workspace_command.or(active_display).or(raw_command)
}

/// Windows a space owns that its layout tree does not hold.
///
/// Such a window is reachable by cmd-tab and unreachable by scrolling, which is what "a second
/// invisible strip" actually looks like from the user's side.
///
/// Floating windows are excluded because they are SUPPOSED to be outside the tree — that is what
/// floating means. Counting them would make every space with a floating window look broken, and the
/// signal would be worth nothing.
pub(crate) fn orphaned_windows(
    owned: &[WindowId],
    is_floating: impl Fn(WindowId) -> bool,
    in_layout_tree: impl Fn(WindowId) -> bool,
) -> Vec<WindowId> {
    owned
        .iter()
        .copied()
        .filter(|window| !is_floating(*window) && !in_layout_tree(*window))
        .collect()
}

/// Display homes recorded for windows that no longer exist.
///
/// Harmless on their own — a home is a small record — but they are how a leak shows itself: a window
/// that died without its home being forgotten means some path removes a window without going through
/// `forget_window`, and the count growing across a session is the evidence.
pub(crate) fn stale_display_homes(
    homed: &[WindowId],
    exists: impl Fn(WindowId) -> bool,
) -> Vec<WindowId> {
    homed.iter().copied().filter(|window| !exists(*window)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(id: u64) -> SpaceId {
        SpaceId::new(id)
    }

    fn window(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    #[test]
    fn the_workspace_command_space_is_preferred_over_everything() {
        assert_eq!(
            default_query_space(Some(space(1)), Some(space(2)), Some(space(3))),
            Some(space(1))
        );
    }

    #[test]
    fn the_active_displays_space_comes_next() {
        assert_eq!(
            default_query_space(None, Some(space(2)), Some(space(3))),
            Some(space(2))
        );
    }

    #[test]
    fn the_raw_command_space_is_the_last_resort() {
        assert_eq!(default_query_space(None, None, Some(space(3))), Some(space(3)));
    }

    /// Answering about nothing is better than answering about the wrong space. A query with no space
    /// and no way to pick one returns nothing, which reads as "ask me again with a space".
    #[test]
    fn no_space_anywhere_is_no_answer() {
        assert_eq!(default_query_space(None, None, None), None);
    }

    /// Owned, tiled, and not in the tree: cmd-tab reachable but unreachable by scrolling.
    #[test]
    fn a_tiled_window_outside_the_layout_tree_is_orphaned() {
        let owned = [window(1), window(2)];
        let orphans = orphaned_windows(&owned, |_| false, |w| w == window(1));
        assert_eq!(orphans, [window(2)]);
    }

    /// A floating window is SUPPOSED to be outside the tree. Counting it would make every space with
    /// one look broken and the signal would be worth nothing.
    #[test]
    fn a_floating_window_outside_the_tree_is_not_orphaned() {
        let owned = [window(1)];
        assert!(orphaned_windows(&owned, |_| true, |_| false).is_empty());
    }

    #[test]
    fn a_window_in_the_tree_is_never_orphaned() {
        let owned = [window(1)];
        assert!(orphaned_windows(&owned, |_| false, |_| true).is_empty());
    }

    #[test]
    fn a_space_owning_nothing_has_no_orphans() {
        assert!(orphaned_windows(&[], |_| false, |_| false).is_empty());
    }

    #[test]
    fn a_home_for_a_window_that_is_gone_is_stale() {
        let homed = [window(1), window(2)];
        assert_eq!(stale_display_homes(&homed, |w| w == window(1)), [window(2)]);
    }

    #[test]
    fn a_home_for_a_live_window_is_not_stale() {
        assert!(stale_display_homes(&[window(1)], |_| true).is_empty());
    }

    #[test]
    fn nothing_homed_is_nothing_stale() {
        assert!(stale_display_homes(&[], |_| false).is_empty());
    }
}
