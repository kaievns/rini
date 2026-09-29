//! Which window is focused, and telling the user's focus changes from the ones rini's own raises
//! and macOS's activation picks produce.
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap as HashMap;

use crate::windows::domain::focus_order::FocusOrder;
use crate::windows::domain::request::Quiet;
use rini_core::ids::{WindowId, pid_t};

/// The focus reports rini's own raises are about to produce.
///
/// macOS reports a focus change for every window a raise touches, and a raise walks the whole workspace.
/// The window meant to end up focused is never swallowed. Cascade measured in
/// `src/animation/docs/capture-overlay-research.md`, "The offset is honest, and it still moved eight times per press".
///
/// A raise a newer one replaced still echoes: raises run one at a time, so in a burst of presses an old
/// one runs late and macOS reports its windows after the newer raise was recorded. Those reports are
/// swallowed for `SUPERSEDED` after the replacement, unless the window is the newest raise's target.
#[derive(Debug, Default)]
pub struct RaiseEcho {
    windows: Vec<WindowId>,
    since: Option<Instant>,
    target: Option<WindowId>,
    /// Every window an earlier raise touched or focused, with when a newer raise replaced it.
    superseded: Vec<(WindowId, Instant)>,
}

impl RaiseEcho {
    /// Long enough to outlast the cascade, which measured 276ms, and short enough not to swallow a click
    /// that follows the keystroke.
    const WINDOW: Duration = Duration::from_millis(400);

    /// How long a replaced raise's reports stay echoes. Late reports arrived up to 0.9s after their
    /// press in a replayed burst; see "Rapid presses" in `specs/focus.md`.
    const SUPERSEDED: Duration = Duration::from_secs(1);

    /// Records the windows a raise is about to touch, superseding the previous raise.
    pub fn expect(
        &mut self,
        raised: impl Iterator<Item = WindowId>,
        target: Option<WindowId>,
        now: Instant,
    ) {
        let replaced: Vec<WindowId> =
            std::mem::take(&mut self.windows).into_iter().chain(self.target).collect();
        for window in replaced {
            match self.superseded.iter_mut().find(|(w, _)| *w == window) {
                Some(entry) => entry.1 = now,
                None => self.superseded.push((window, now)),
            }
        }
        self.superseded.retain(|&(window, at)| {
            Some(window) != target && now.duration_since(at) < Self::SUPERSEDED
        });
        self.windows = raised.filter(|window| Some(*window) != target).collect();
        self.since = Some(now);
        self.target = target;
    }

    /// Whether this focus report is rini's own raise coming back, rather than the user going somewhere.
    pub fn swallows(&self, window: WindowId, now: Instant) -> bool {
        let this_raise = self.since.is_some_and(|since| now.duration_since(since) < Self::WINDOW)
            && self.windows.contains(&window);
        let a_replaced_raise = self
            .superseded
            .iter()
            .any(|&(w, at)| w == window && now.duration_since(at) < Self::SUPERSEDED);
        this_raise || a_replaced_raise
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
    ApplicationLaunched {
        pid: pid_t,
        is_frontmost: bool,
        main_window: Option<WindowId>,
    },
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
    /// WindowServer named a window rini knows and does not manage as focused.
    ///
    /// An app can float a panel of its own above the window the user is in, and macOS names the panel:
    /// Zoom's meeting controls take the report while Accessibility names the call window main and
    /// focused. A panel is no window to switch to, so the report stands for the app's main window.
    PanelFocused(WindowId),
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
    /// Every window in the order it was last focused, most recent first.
    ///
    /// Written here rather than beside the switcher that reads it, because this type is the only place
    /// that sees every authoritative focus edge, and one writer is what stops it becoming a third focus
    /// record that disagrees with the two above.
    focus_order: FocusOrder,
    /// The app that has just been activated, with whatever it had focused BEFORE the activation.
    ///
    /// Snapshotted because the activation immediately overwrites the live record: macOS reports its own
    /// choice of main window a few milliseconds later, and the pre-activation window is what tells rini
    /// whether that choice matches where the user actually was.
    pending_activation: Option<(pid_t, Option<WindowId>)>,
    /// The app whose panel WindowServer last named, so a main-window report arriving after it still
    /// moves the focus.
    panel_focused: Option<pid_t>,
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
                self.focus_order.forget_app(pid);
                return None;
            }
            FocusEvent::WindowDestroyed(wid) => {
                if self.window_server_focus == Some(wid) {
                    self.window_server_focus = None;
                }
                self.focus_order.forget(wid);
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
                if let Some(wid) = wid
                    && self.panel_focused == Some(pid)
                {
                    self.window_server_focused(wid);
                }
                (pid, quiet)
            }
            FocusEvent::WindowServerFocusChanged(wid) => {
                self.panel_focused = None;
                self.window_server_focused(wid);
                return None;
            }
            FocusEvent::PanelFocused(panel) => {
                self.panel_focused = Some(panel.pid);
                let main = self.apps.get(&panel.pid).and_then(|app| app.main_window);
                self.window_server_focused(main.unwrap_or(panel));
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
                // The other focus edge. The window-server arm above returns early, so this is the only
                // other point at which this type decides a window has the focus — an AX activation
                // before the window server has spoken, which is the cold-start case. Touching only the
                // window-server arm would leave the order empty until the first native focus report.
                self.focus_order.touch(wid);
                return Some(wid);
            }
        }
        None
    }

    fn window_server_focused(&mut self, wid: WindowId) {
        self.window_server_focus_authoritative = true;
        self.window_server_focus = Some(wid);
        self.last_focused_by_app.insert(wid.pid, wid);
        self.focus_order.touch(wid);
    }

    /// Every window in the order it was last focused, most recent first.
    pub fn focus_order(&self) -> &FocusOrder {
        &self.focus_order
    }

    /// Carry a window's place in the focus order across an identity change.
    pub fn rekey_focus_order(&mut self, from: WindowId, to: WindowId) {
        self.focus_order.rekey(from, to);
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

    mod focus_order_wiring {
        use super::super::{FocusEvent, MainWindowTracker};
        use crate::windows::domain::request::Quiet;
        use rini_core::ids::WindowId;

        fn wid(pid: rini_core::ids::pid_t, idx: u32) -> WindowId {
            WindowId::new(pid, idx)
        }

        /// The order has to be populated by real focus edges, not just by a test poking the pure type.
        /// `WindowServerFocusChanged` is the authoritative one.
        #[test]
        fn window_server_focus_records_the_order() {
            let mut tracker = MainWindowTracker::default();

            let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(wid(1, 1)));
            let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(wid(2, 1)));

            assert_eq!(
                tracker.focus_order().iter().collect::<Vec<_>>(),
                vec![wid(2, 1), wid(1, 1)]
            );
        }

        /// The cold-start edge. `handle_event` returns early for the window-server arm, so an AX
        /// activation before the window server has spoken is the only OTHER point this type decides a
        /// window has focus — and tracking only the first would leave the order empty until the first
        /// native focus report arrives.
        #[test]
        fn an_activation_before_the_window_server_speaks_records_the_order_too() {
            let mut tracker = MainWindowTracker::default();
            let _ = tracker.handle_event(FocusEvent::ApplicationLaunched {
                pid: 5,
                is_frontmost: true,
                main_window: Some(wid(5, 1)),
            });
            let _ = tracker.handle_event(FocusEvent::ApplicationGloballyActivated(5));

            let focused = tracker.handle_event(FocusEvent::ApplicationMainWindowChanged(
                5,
                Some(wid(5, 1)),
                Quiet::No,
            ));

            assert_eq!(focused, Some(wid(5, 1)));
            assert_eq!(
                tracker.focus_order().iter().collect::<Vec<_>>(),
                vec![wid(5, 1)],
                "the order is not empty before the first window-server report"
            );
        }

        #[test]
        fn a_destroyed_window_leaves_the_order() {
            let mut tracker = MainWindowTracker::default();
            let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(wid(1, 1)));
            let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(wid(1, 2)));

            let _ = tracker.handle_event(FocusEvent::WindowDestroyed(wid(1, 1)));

            assert_eq!(tracker.focus_order().iter().collect::<Vec<_>>(), vec![wid(1, 2)]);
        }

        #[test]
        fn an_application_thread_ending_takes_its_windows_out_of_the_order() {
            let mut tracker = MainWindowTracker::default();
            let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(wid(1, 1)));
            let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(wid(2, 1)));

            let _ = tracker.handle_event(FocusEvent::ApplicationThreadTerminated(1));

            assert_eq!(tracker.focus_order().iter().collect::<Vec<_>>(), vec![wid(2, 1)]);
        }
    }

    mod raise_echo {
        use std::time::{Duration, Instant};

        use super::super::RaiseEcho;
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
        /// though the previous raise had it down as an echo. The first raise is not forgotten: raises run
        /// one at a time, so it can still run late and report the windows it touched.
        #[test]
        fn a_newer_raise_supersedes_the_one_before_it() {
            let now = Instant::now();
            let mut echo = RaiseEcho::default();
            echo.expect([wid(68), wid(92)].into_iter(), Some(wid(58)), now);
            let later = now + Duration::from_millis(50);
            echo.expect([wid(58), wid(92)].into_iter(), Some(wid(92)), later);
            assert!(!echo.swallows(wid(92), later), "the new target gets through");
            assert!(echo.swallows(wid(58), later), "and the new echoes are swallowed");
            assert!(echo.swallows(wid(68), later), "the replaced raise still echoes");
        }

        #[test]
        fn nothing_is_swallowed_before_any_raise() {
            assert!(!RaiseEcho::default().swallows(wid(68), Instant::now()));
        }

        fn at(base: Instant, millis: u64) -> Instant {
            base + Duration::from_millis(millis)
        }

        /// The reported case: three presses, three raises. The first one's target is reported after
        /// the third raise was recorded, and must not pull the focus back to it.
        #[test]
        fn a_replaced_raises_target_reported_late_is_an_echo() {
            let base = Instant::now();
            let (a, b, c) = (WindowId::new(1, 1), WindowId::new(1, 2), WindowId::new(1, 3));
            let mut echo = RaiseEcho::default();
            echo.expect([a, b, c].into_iter(), Some(a), base);
            echo.expect([a, b, c].into_iter(), Some(b), at(base, 150));
            echo.expect([a, b, c].into_iter(), Some(c), at(base, 300));

            assert!(echo.swallows(a, at(base, 900)), "a's late report");
            assert!(echo.swallows(b, at(base, 900)), "b's late report");
            assert!(
                !echo.swallows(c, at(base, 900)),
                "the newest target always lands"
            );
        }

        /// Once the burst is well over, a report for any of those windows is the user again.
        #[test]
        fn a_replaced_raise_stops_echoing_after_a_second() {
            let base = Instant::now();
            let (a, b) = (WindowId::new(1, 1), WindowId::new(1, 2));
            let mut echo = RaiseEcho::default();
            echo.expect([a, b].into_iter(), Some(a), base);
            echo.expect([a, b].into_iter(), Some(b), at(base, 100));

            assert!(!echo.swallows(a, at(base, 1200)));
        }

        /// Going back to a window is a new raise with it as the target, which always lands.
        #[test]
        fn returning_to_a_replaced_target_is_not_swallowed() {
            let base = Instant::now();
            let (a, b) = (WindowId::new(1, 1), WindowId::new(1, 2));
            let mut echo = RaiseEcho::default();
            echo.expect([a, b].into_iter(), Some(a), base);
            echo.expect([a, b].into_iter(), Some(b), at(base, 100));
            echo.expect([a, b].into_iter(), Some(a), at(base, 200));

            assert!(!echo.swallows(a, at(base, 300)));
            assert!(echo.swallows(b, at(base, 300)), "b is now the replaced one");
        }
    }

    /// The measured case: cmd-tab to Ghostty, and macOS makes the built-in display's window main even
    /// though the user was in the external display's one. Following the pick would switch the built-in
    /// display's workspace to reveal a window the user did not ask for.
    #[test]
    fn a_parked_pick_defers_to_the_window_the_app_was_in() {
        let parked = WindowId::new(954, 11333);
        let visible = WindowId::new(954, 9607);
        assert_eq!(
            activation_focus_target(parked, false, Some(visible), true),
            Some(visible)
        );
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
        assert_eq!(
            activation_focus_target(parked, false, Some(also_parked), false),
            None
        );
    }

    #[test]
    fn nothing_remembered_or_the_same_window_leaves_focus_alone() {
        let parked = WindowId::new(954, 11333);
        assert_eq!(activation_focus_target(parked, false, None, false), None);
        assert_eq!(activation_focus_target(parked, false, Some(parked), true), None);
    }

    fn launched(tracker: &mut MainWindowTracker, pid: pid_t, main_window: Option<WindowId>) {
        let _ = tracker.handle_event(FocusEvent::ApplicationLaunched {
            pid,
            is_frontmost: false,
            main_window,
        });
    }

    /// Leaving the app ends the panel's claim: a main-window change there afterwards is the app
    /// rearranging itself in the background, not the user coming back.
    #[test]
    fn a_main_window_change_after_leaving_the_app_moves_nothing() {
        let mut tracker = MainWindowTracker::default();
        let (main, call, panel) = (WindowId::new(1, 1), WindowId::new(1, 2), WindowId::new(1, 3));
        let elsewhere = WindowId::new(2, 1);
        launched(&mut tracker, 1, Some(main));
        launched(&mut tracker, 2, Some(elsewhere));

        let _ = tracker.handle_event(FocusEvent::PanelFocused(panel));
        let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(elsewhere));
        let _ = tracker.handle_event(FocusEvent::ApplicationMainWindowChanged(
            1,
            Some(call),
            Quiet::No,
        ));

        assert_eq!(tracker.window_server_focus, Some(elsewhere));
        assert_eq!(tracker.focus_order.position(call), None);
    }

    /// With no main window known, the panel is all there is to record.
    #[test]
    fn a_panel_of_an_app_with_no_main_window_is_recorded_as_itself() {
        let mut tracker = MainWindowTracker::default();
        let panel = WindowId::new(1, 3);
        launched(&mut tracker, 1, None);

        let _ = tracker.handle_event(FocusEvent::PanelFocused(panel));

        assert_eq!(tracker.window_server_focus, Some(panel));
    }

    #[test]
    fn an_activation_target_is_offered_once_and_only_to_its_own_app() {
        let mut tracker = MainWindowTracker::default();
        let window = WindowId::new(954, 9607);
        tracker.apps.insert(
            954,
            AppState {
                is_frontmost: false,
                frontmost_is_quiet: Quiet::No,
                main_window: None,
            },
        );
        let _ = tracker.handle_event(FocusEvent::WindowServerFocusChanged(window));
        let _ = tracker.handle_event(FocusEvent::ApplicationGloballyActivated(954));
        assert_eq!(
            tracker.take_activation_target(1073),
            None,
            "another app's focus change"
        );
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
        tracker.apps.insert(
            954,
            AppState {
                is_frontmost: false,
                frontmost_is_quiet: Quiet::No,
                main_window: None,
            },
        );
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
        tracker.apps.insert(
            954,
            AppState {
                is_frontmost: false,
                frontmost_is_quiet: Quiet::No,
                main_window: None,
            },
        );
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
