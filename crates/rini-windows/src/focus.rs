//! Telling the user's focus changes from the ones rini's own raises and macOS's activation picks
//! produce.
use std::time::{Duration, Instant};

use crate::ids::WindowId;

/// The focus reports rini's own raises are about to produce.
///
/// macOS reports a focus change for every window a raise touches, and a raise walks the whole workspace.
/// The window meant to end up focused is never swallowed. Cascade measured in
/// `crates/rini-animation/docs/capture-overlay-research.md`, "The offset is honest, and it still moved eight times per press".
#[derive(Debug, Default)]
pub struct RaiseEcho {
    windows: Vec<WindowId>,
    since: Option<Instant>,
}

impl RaiseEcho {
    /// Long enough to outlast the cascade, which measured 276ms, and short enough not to swallow a click
    /// that follows the keystroke.
    const WINDOW: Duration = Duration::from_millis(400);

    /// Records the windows a raise is about to touch, superseding the previous raise.
    pub fn expect(
        &mut self,
        raised: impl Iterator<Item = WindowId>,
        target: Option<WindowId>,
        now: Instant,
    ) {
        self.windows = raised.filter(|window| Some(*window) != target).collect();
        self.since = Some(now);
    }

    /// Whether this focus report is rini's own raise coming back, rather than the user going somewhere.
    pub fn swallows(&self, window: WindowId, now: Instant) -> bool {
        self.since.is_some_and(|since| now.duration_since(since) < Self::WINDOW)
            && self.windows.contains(&window)
    }
}

/// Which window an app activation should really focus, or `None` to accept macOS's choice.
///
/// macOS picks the window on cmd-tab, and it can pick one rini has parked off screen for a workspace it is
/// not showing. Following that costs a workspace switch on the parked window's display, even when the app
/// has a perfectly visible window that the user was last in. Redirecting only ever AVOIDS a switch: it
/// applies when the pick is parked and the remembered window is not, so it can never cause one.
pub fn activation_focus_target(
    picked: WindowId,
    picked_is_visible: bool,
    remembered: Option<WindowId>,
    remembered_is_visible: bool,
) -> Option<WindowId> {
    if picked_is_visible {
        return None;
    }
    let remembered = remembered?;
    if remembered == picked || !remembered_is_visible {
        return None;
    }
    Some(remembered)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod raise_echo {
        use std::time::{Duration, Instant};

        use super::RaiseEcho;
        use crate::ids::WindowId;

        fn wid(idx: u32) -> WindowId {
            WindowId::new(1, idx)
        }

        /// The measured cascade: one press raised eleven windows, and each raise came back as a focus
        /// report that moved the layout's selection and scrolled the strip to that window.
        #[test]
        fn a_raised_window_reporting_focus_is_rinis_own_echo() {
            let now = Instant::now();
            let mut echo = RaiseEcho::default();
            echo.expect([wid(68), wid(92), wid(58)].into_iter(), Some(wid(58)), now);
            assert!(echo.swallows(wid(68), now));
            assert!(echo.swallows(wid(92), now));
        }

        /// The one report that matters. Swallowing the target too would leave the layout's selection
        /// behind wherever it was, so the press would do nothing at all.
        #[test]
        fn the_window_meant_to_end_up_focused_is_never_swallowed() {
            let now = Instant::now();
            let mut echo = RaiseEcho::default();
            echo.expect([wid(68), wid(58)].into_iter(), Some(wid(58)), now);
            assert!(!echo.swallows(wid(58), now));
        }

        #[test]
        fn a_window_this_raise_never_touched_is_the_user_going_somewhere() {
            let now = Instant::now();
            let mut echo = RaiseEcho::default();
            echo.expect([wid(68)].into_iter(), Some(wid(58)), now);
            assert!(!echo.swallows(wid(120), now));
        }

        /// A click that lands well after the cascade has finished is the user, whatever it lands on.
        #[test]
        fn the_echo_stops_being_believed_once_the_cascade_is_over() {
            let now = Instant::now();
            let mut echo = RaiseEcho::default();
            echo.expect([wid(68)].into_iter(), Some(wid(58)), now);
            assert!(echo.swallows(wid(68), now + Duration::from_millis(276)));
            assert!(!echo.swallows(wid(68), now + Duration::from_millis(500)));
        }

        /// Rapid presses: the second raise supersedes the first, and its own target must get through even
        /// though the previous raise had it down as an echo.
        #[test]
        fn a_newer_raise_supersedes_the_one_before_it() {
            let now = Instant::now();
            let mut echo = RaiseEcho::default();
            echo.expect([wid(68), wid(92)].into_iter(), Some(wid(58)), now);
            let later = now + Duration::from_millis(50);
            echo.expect([wid(58), wid(92)].into_iter(), Some(wid(92)), later);
            assert!(!echo.swallows(wid(92), later), "the new target gets through");
            assert!(echo.swallows(wid(58), later), "and the new echoes are swallowed");
            assert!(!echo.swallows(wid(68), later), "the old raise is forgotten");
        }

        #[test]
        fn nothing_is_swallowed_before_any_raise() {
            assert!(!RaiseEcho::default().swallows(wid(68), Instant::now()));
        }
    }

    /// The measured case: cmd-tab to Ghostty, and macOS makes the built-in display's window main even
    /// though the user was in the external display's one. Following the pick would switch the built-in
    /// display's workspace to reveal a window the user did not ask for.
    #[test]
    fn a_parked_pick_defers_to_the_window_the_app_was_in() {
        let parked = WindowId::new(954, 11333);
        let visible = WindowId::new(954, 9607);
        assert_eq!(activation_focus_target(parked, false, Some(visible), true), Some(visible));
    }

    #[test]
    fn a_visible_pick_is_always_accepted() {
        // Nothing to gain: no workspace has to move to show it, so macOS's choice stands even when rini
        // remembers a different window.
        let visible = WindowId::new(954, 9607);
        let other = WindowId::new(954, 11333);
        assert_eq!(activation_focus_target(visible, true, Some(other), true), None);
    }

    /// The redirect must never CAUSE a workspace switch, only avoid one. A remembered window that is
    /// itself parked would have to be revealed, which is a switch the user did not ask for either.
    #[test]
    fn a_parked_remembered_window_is_not_worth_a_switch() {
        let parked = WindowId::new(954, 11333);
        let also_parked = WindowId::new(954, 9607);
        assert_eq!(activation_focus_target(parked, false, Some(also_parked), false), None);
    }

    #[test]
    fn nothing_remembered_or_the_same_window_leaves_focus_alone() {
        let parked = WindowId::new(954, 11333);
        assert_eq!(activation_focus_target(parked, false, None, false), None);
        assert_eq!(activation_focus_target(parked, false, Some(parked), true), None);
    }
}
