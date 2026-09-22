//! Which window is focused, and telling the user's focus changes from the ones rini's own raises
//! and macOS's activation picks produce.
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap as HashMap;

use crate::windows::domain::request::Quiet;
use rini_core::ids::{WindowId, pid_t};

/// The focus reports rini's own raises are about to produce.
///
/// macOS reports a focus change for every window a raise touches, and a raise walks the whole workspace.
/// The window meant to end up focused is never swallowed. Cascade measured in
/// `src/animation/docs/capture-overlay-research.md`, "The offset is honest, and it still moved eight times per press".
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

/// The slice of the application's event stream focus tracking reads. The application builds one
/// from each of its events; anything else is not a focus edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FocusEvent {
    ApplicationLaunched { pid: pid_t, is_frontmost: bool, main_window: Option<WindowId> },
    ApplicationThreadTerminated(pid_t),
    WindowDestroyed(WindowId),
    ApplicationActivated(pid_t, Quiet),
    ApplicationDeactivated(pid_t),
    /// The Carbon front-app edge, the one macOS reports for cmd-tab.
    ApplicationGloballyActivated(pid_t),
    ApplicationGloballyDeactivated(pid_t),
    ApplicationMainWindowChanged(pid_t, Option<WindowId>, Quiet),
    /// WindowServer's key window: authoritative once seen, AX reports are metadata after it.
    WindowServerFocusChanged(WindowId),
}

#[derive(Default)]
pub struct MainWindowTracker {
    apps: HashMap<pid_t, AppState>,
    global_frontmost: Option<pid_t>,
    window_server_focus: Option<WindowId>,
    window_server_focus_authoritative: bool,
    /// Which window of each app rini last saw focused. macOS picks a window of its own on activation,
    /// and that pick is not always this one.
    last_focused_by_app: HashMap<pid_t, WindowId>,
    /// The app that has just been activated, with whatever it had focused BEFORE the activation.
    ///
    /// Snapshotted because the activation immediately overwrites the live record: macOS reports its own
    /// choice of main window a few milliseconds later, and the pre-activation window is what tells rini
    /// whether that choice matches where the user actually was.
    pending_activation: Option<(pid_t, Option<WindowId>)>,
}

struct AppState {
    is_frontmost: bool,
    frontmost_is_quiet: Quiet,
    main_window: Option<WindowId>,
}

impl MainWindowTracker {
    /// Make `window` the focused window, as WindowServer focus would.
    ///
    /// Tests that exercise commands operating on "the focused window" otherwise have to
    /// replay a launch/activate sequence just to populate this, and `add_test_app` does not
    /// set it — so such a command silently no-ops and the test passes for the wrong reason.
    #[cfg(test)]
    pub fn set_focus_for_test(&mut self, window: WindowId) {
        // main_window() requires the owning app to be globally frontmost before it will
        // consult window_server_focus, so both have to be set.
        self.global_frontmost = Some(window.pid);
        self.window_server_focus_authoritative = true;
        self.window_server_focus = Some(window);
    }
    #[must_use]
    pub fn handle_event(&mut self, event: FocusEvent) -> Option<WindowId> {
        let (event_pid, quiet_edge) = match event {
            FocusEvent::ApplicationLaunched { pid, is_frontmost, main_window } => {
                self.apps.insert(
                    pid,
                    AppState {
                        is_frontmost,
                        frontmost_is_quiet: Quiet::No,
                        main_window,
                    },
                );
                (pid, Quiet::No)
            }
            FocusEvent::ApplicationThreadTerminated(pid) => {
                self.apps.remove(&pid);
                if self.window_server_focus.is_some_and(|wid| wid.pid == pid) {
                    self.window_server_focus = None;
                }
                return None;
            }
            FocusEvent::WindowDestroyed(wid) => {
                if self.window_server_focus == Some(wid) {
                    self.window_server_focus = None;
                }
                return None;
            }
            FocusEvent::ApplicationActivated(pid, quiet) => {
                // A quiet activation is rini's own raise. Redirecting the focus change that follows would
                // undo whatever rini just asked for.
                if quiet == Quiet::Yes && self.pending_activation.is_some_and(|(p, _)| p == pid) {
                    self.pending_activation = None;
                }
                let app = self.apps.get_mut(&pid)?;
                app.is_frontmost = true;
                app.frontmost_is_quiet = quiet;
                (pid, quiet)
            }
            FocusEvent::ApplicationDeactivated(pid) => {
                let app = self.apps.get_mut(&pid)?;
                app.is_frontmost = false;
                return None;
            }
            FocusEvent::ApplicationGloballyActivated(pid) => {
                // Only a real activation edge snapshots. A duplicate arrives while the app is already
                // frontmost, and re-snapshotting there would capture the window this activation just
                // focused rather than the one before it.
                if self.global_frontmost != Some(pid) {
                    self.pending_activation =
                        Some((pid, self.last_focused_by_app.get(&pid).copied()));
                }
                self.global_frontmost = Some(pid);
                let Some(app) = self.apps.get_mut(&pid) else {
                    return None;
                };
                app.is_frontmost = true;
                (pid, app.frontmost_is_quiet)
            }
            FocusEvent::ApplicationGloballyDeactivated(pid) => {
                if self.global_frontmost == Some(pid) {
                    self.global_frontmost = None;
                }
                if let Some(app) = self.apps.get_mut(&pid) {
                    app.is_frontmost = false;
                }
                return None;
            }
            FocusEvent::ApplicationMainWindowChanged(pid, wid, quiet) => {
                let app = self.apps.get_mut(&pid)?;
                app.main_window = wid;
                (pid, quiet)
            }
            FocusEvent::WindowServerFocusChanged(wid) => {
                self.window_server_focus_authoritative = true;
                self.window_server_focus = Some(wid);
                self.last_focused_by_app.insert(wid.pid, wid);
                return None;
            }
        };
        // Once WindowServer focus has produced a result, AX activation/main-window
        // events remain useful as metadata and cold-start fallback only. Letting
        // them emit focus here can replay the previous native focus while the new
        // 808/815 resolution is still in flight.
        if self.window_server_focus_authoritative {
            return None;
        }
        if Some(event_pid) == self.global_frontmost && quiet_edge == Quiet::No {
            if let Some(wid) = self.main_window() {
                return Some(wid);
            }
        }
        None
    }

    pub fn main_window(&self) -> Option<WindowId> {
        let Some(pid) = self.global_frontmost else {
            return None;
        };
        if let Some(window) = self.window_server_focus.filter(|window| window.pid == pid) {
            return Some(window);
        }
        match self.apps.get(&pid) {
            Some(&AppState {
                is_frontmost: true,
                main_window: Some(window),
                ..
            }) => Some(window),
            _ => None,
        }
    }

    pub fn is_globally_frontmost(&self, pid: pid_t) -> bool {
        self.global_frontmost == Some(pid)
    }

    /// The window `pid` had focused before it was just activated, once per activation.
    ///
    /// `None` when this focus change is not the one macOS produced for an activation, which is how cmd-`
    /// window cycling stays untouched: rini raises those itself and no activation edge is involved.
    pub fn take_activation_target(&mut self, pid: pid_t) -> Option<WindowId> {
        let remembered = self.peek_activation_target(pid);
        if self.pending_activation.is_some_and(|(p, _)| p == pid) {
            self.pending_activation = None;
        }
        remembered
    }

    /// `take_activation_target` without consuming it, for a caller that may not act on it.
    pub fn peek_activation_target(&self, pid: pid_t) -> Option<WindowId> {
        let (pending_pid, remembered) = self.pending_activation?;
        if pending_pid != pid {
            return None;
        }
        remembered
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    mod raise_echo {
        use std::time::{Duration, Instant};

        use super::RaiseEcho;
        use rini_core::ids::WindowId;

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

    #[test]
    fn an_activation_target_is_offered_once_and_only_to_its_own_app() {
        let mut tracker = MainWindowTracker::default();
        let window = WindowId::new(954, 9607);
        tracker.apps.insert(954, AppState {
            is_frontmost: false,
            frontmost_is_quiet: Quiet::No,
            main_window: None,
        });
        let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(window));
        let _ = tracker.handle_event(FocusEvent::ApplicationGloballyActivated(954));
        assert_eq!(tracker.take_activation_target(1073), None, "another app's focus change");
        assert_eq!(tracker.take_activation_target(954), Some(window));
        assert_eq!(tracker.take_activation_target(954), None, "consumed");
    }

    /// cmd-` cycles windows inside the app rini has already activated, so there is no activation edge and
    /// the switch that reveals a parked window still happens.
    #[test]
    fn a_focus_change_without_an_activation_offers_nothing() {
        let mut tracker = MainWindowTracker::default();
        let window = WindowId::new(954, 9607);
        let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(window));
        assert_eq!(tracker.take_activation_target(954), None);
    }

    /// A raise rini asked for arrives as a quiet activation. Redirecting the focus change behind it would
    /// undo the raise.
    #[test]
    fn a_quiet_activation_drops_the_pending_target() {
        let mut tracker = MainWindowTracker::default();
        let window = WindowId::new(954, 9607);
        tracker.apps.insert(954, AppState {
            is_frontmost: false,
            frontmost_is_quiet: Quiet::No,
            main_window: None,
        });
        let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(window));
        let _ = tracker.handle_event(FocusEvent::ApplicationGloballyActivated(954));
        let _ = tracker.handle_event(FocusEvent::ApplicationActivated(954, Quiet::Yes));
        assert_eq!(tracker.take_activation_target(954), None);
    }

    /// A duplicate global activation must not re-snapshot: by then the window the activation focused is
    /// the live record, and the pre-activation window would be lost.
    #[test]
    fn a_duplicate_activation_keeps_the_original_target() {
        let mut tracker = MainWindowTracker::default();
        let was_in = WindowId::new(954, 9607);
        let picked = WindowId::new(954, 11333);
        tracker.apps.insert(954, AppState {
            is_frontmost: false,
            frontmost_is_quiet: Quiet::No,
            main_window: None,
        });
        let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(was_in));
        let _ = tracker.handle_event(FocusEvent::ApplicationGloballyActivated(954));
        let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(picked));
        let _ = tracker.handle_event(FocusEvent::ApplicationGloballyActivated(954));
        assert_eq!(tracker.take_activation_target(954), Some(was_in));
    }

    #[test]
    fn window_server_focus_supersedes_ax_focus_events() {
        let ax_window = WindowId::new(7, 1);
        let server_window = WindowId::new(7, 2);
        let stale_window = WindowId::new(7, 3);
        let mut tracker = MainWindowTracker::default();
        tracker.global_frontmost = Some(7);
        tracker.apps.insert(
            7,
            AppState {
                is_frontmost: true,
                frontmost_is_quiet: Quiet::No,
                main_window: Some(ax_window),
            },
        );

        assert_eq!(tracker.main_window(), Some(ax_window));
        assert_eq!(
            tracker.handle_event(FocusEvent::WindowServerFocusChanged(server_window)),
            None
        );
        assert_eq!(tracker.main_window(), Some(server_window));

        assert_eq!(
            tracker.handle_event(FocusEvent::ApplicationMainWindowChanged(
                7,
                Some(stale_window),
                Quiet::No,
            )),
            None,
            "AX must not drive focus after native authority is initialized"
        );
        assert_eq!(tracker.main_window(), Some(server_window));

        let _ = tracker.handle_event(FocusEvent::ApplicationMainWindowChanged(
            7,
            Some(ax_window),
            Quiet::No,
        ));

        let _ = tracker.handle_event(FocusEvent::WindowDestroyed(server_window));
        assert_eq!(tracker.main_window(), Some(ax_window));
    }
}
