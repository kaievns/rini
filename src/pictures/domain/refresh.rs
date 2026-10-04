//! Windows pictured afresh at rest once their look has turned: through SkyLight where the window
//! server has them wholly on the display, and through ScreenCaptureKit anywhere else.
//!
//! Pure. Asks and what each look came to go in with `now`; the windows to look at, and those owed a
//! ScreenCaptureKit capture instead, come out. `crate::pictures::domain::schedule` holds the looks off
//! while a transition holds the service and makes them before any ScreenCaptureKit call. The rules and
//! why each holds: "The schedule" in `src/pictures/docs/README.md`.

use std::time::Instant;

use rini_core::ids::WindowId;

use crate::pictures::domain::print::print_changed;
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::domain::watch::{Accept, CHANGE_EVERY, CHANGE_TRIES, Capture, Look, Seen, Set};

/// Every window owed a picture at rest, and the ones the last round looked at.
#[derive(Default)]
pub struct Refreshes {
    windows: Vec<Refresh>,
    looked: Vec<WindowId>,
}

struct Refresh {
    target: PictureTarget,
    /// The band print its turn is judged from: the picture held when it was asked; with none, its
    /// first capture.
    baseline: Option<Vec<u8>>,
    left: u32,
    next: Instant,
}

impl Refreshes {
    /// Owes each target a picture, looked at from `from` on. `held` is the band print of the picture
    /// held of a window. A window already owed one is owed it afresh: its look has turned again.
    pub fn ask(&mut self, targets: Vec<PictureTarget>, from: Instant, held: &dyn Fn(WindowId) -> Option<Vec<u8>>) {
        for target in targets {
            self.forget(target.window);
            self.windows.push(Refresh {
                target,
                baseline: held(target.window),
                left: CHANGE_TRIES,
                next: from,
            });
        }
    }

    pub fn forget(&mut self, window: WindowId) { self.windows.retain(|refresh| refresh.target.window != window) }

    pub fn owed(&self) -> impl Iterator<Item = WindowId> + '_ { self.windows.iter().map(|refresh| refresh.target.window) }

    /// The windows due a look at `now`, each taken wholly on the display wherever it is, in one set
    /// with nothing composited behind them but the desktop.
    pub fn next(&mut self, now: Instant) -> Option<Set> {
        let looks: Vec<Look> = self
            .windows
            .iter()
            .filter(|refresh| refresh.next <= now)
            .map(|refresh| Look {
                target: refresh.target,
                accepts: vec![Accept::OnDisplay],
                thumbprint: false,
            })
            .collect();
        if looks.is_empty() {
            return None;
        }
        self.looked = looks.iter().map(|look| look.target.window).collect();
        Some(Set { looks, behind: Vec::new() })
    }

    /// When a look is next due, if a window is owed one.
    pub fn wake(&self) -> Option<Instant> { self.windows.iter().map(|refresh| refresh.next).min() }

    /// What the window server and the captures said of the windows `next` handed out. A window is
    /// done at the first capture whose band print differs from its baseline, once its tries run out,
    /// or at a capture the cache refused; one not captured wholly on the display is done here and
    /// returned, owed a ScreenCaptureKit capture.
    pub fn seen(&mut self, now: Instant, seen: &[Seen]) -> Vec<PictureTarget> {
        let mut elsewhere = Vec::new();
        for window in std::mem::take(&mut self.looked) {
            let Some(at) = self.windows.iter().position(|refresh| refresh.target.window == window) else {
                continue;
            };
            let refresh = &mut self.windows[at];
            let done = match seen.iter().find(|seen| seen.window == window).map(|seen| &seen.capture) {
                Some(Capture::Taken(prints)) => {
                    let turned = refresh
                        .baseline
                        .as_deref()
                        .is_some_and(|baseline| print_changed(Some(baseline), &prints.band));
                    refresh.baseline.get_or_insert_with(|| prints.band.clone());
                    refresh.left = refresh.left.saturating_sub(1);
                    refresh.next = now + CHANGE_EVERY;
                    turned || refresh.left == 0
                }
                // Every later capture at this size is refused too.
                Some(Capture::Refused) => true,
                Some(Capture::Missed) | None => {
                    elsewhere.push(refresh.target);
                    true
                }
            };
            if done {
                self.windows.remove(at);
            }
        }
        elsewhere
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rini_core::ids::WindowServerId;
    use rini_geometry::space::{ScreenPoint, ScreenRect, ScreenSize};

    use super::*;
    use crate::pictures::domain::watch::Prints;

    fn wid(idx: u32) -> WindowId { WindowId::new(1, idx) }

    fn target(idx: u32) -> PictureTarget {
        PictureTarget {
            window: wid(idx),
            server_id: WindowServerId::new(idx),
            size: ScreenSize::new(859.0, 1081.0),
        }
    }

    fn held(band: u8) -> impl Fn(WindowId) -> Option<Vec<u8>> { move |_| Some(vec![band]) }

    fn nothing_held(_: WindowId) -> Option<Vec<u8>> { None }

    fn looked(set: Option<Set>) -> Vec<WindowId> {
        set.map(|set| set.looks.into_iter().map(|look| look.target.window).collect()).unwrap_or_default()
    }

    fn seen(idx: u32, capture: Capture) -> Seen {
        Seen {
            window: wid(idx),
            frame: Some(ScreenRect::new(ScreenPoint::new(4.0, 32.0), ScreenSize::new(859.0, 1081.0))),
            capture,
        }
    }

    fn taken(band: u8) -> Capture {
        Capture::Taken(Prints {
            band: vec![band],
            thumb: None,
        })
    }

    /// Nothing is looked at before its start, and then every window due is, taken wholly on the
    /// display wherever the window server has it, with only the desktop composited behind it.
    #[test]
    fn a_refresh_is_looked_at_from_its_start_wholly_on_the_display_with_nothing_behind_it() {
        let now = Instant::now();
        let from = now + Duration::from_millis(50);
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1), target(2)], from, &nothing_held);
        assert_eq!(refreshes.wake(), Some(from));
        assert!(refreshes.next(now).is_none(), "not yet");
        let set = refreshes.next(from).expect("both due");
        assert!(set.behind.is_empty());
        assert!(set.looks.iter().all(|look| look.accepts == vec![Accept::OnDisplay] && !look.thumbprint));
        assert_eq!(looked(Some(set)), vec![wid(1), wid(2)]);
    }

    /// An app repaints a while after its focus changes: a capture showing the band held when it was
    /// asked is looked at again `CHANGE_EVERY` later, and the first that differs is the turned look.
    /// One whose band never changes is done when its tries run out, its last capture held.
    #[test]
    fn a_refresh_ends_at_the_first_capture_whose_band_turned_or_when_its_tries_run_out() {
        let now = Instant::now();
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1), target(2)], now, &held(7));
        let _ = refreshes.next(now);
        assert!(refreshes.seen(now, &[seen(1, taken(7)), seen(2, taken(7))]).is_empty());
        assert_eq!(refreshes.wake(), Some(now + CHANGE_EVERY));
        assert!(refreshes.next(now).is_none(), "looked at again only after CHANGE_EVERY");

        let mut at = now + CHANGE_EVERY;
        let _ = refreshes.next(at);
        refreshes.seen(at, &[seen(1, taken(8)), seen(2, taken(7))]);
        assert_eq!(refreshes.owed().collect::<Vec<_>>(), vec![wid(2)], "window 1 turned");

        let mut tries = 2;
        loop {
            at += CHANGE_EVERY;
            let Some(set) = refreshes.next(at) else { break };
            assert_eq!(looked(Some(set)), vec![wid(2)]);
            refreshes.seen(at, &[seen(2, taken(7))]);
            tries += 1;
        }
        assert_eq!(tries, CHANGE_TRIES);
        assert!(refreshes.owed().next().is_none());
    }

    /// With no picture held to judge it by, or only a surface, a first capture cannot be called a
    /// turn: the turn is judged from it.
    #[test]
    fn with_nothing_held_the_first_capture_is_what_a_turn_is_judged_from() {
        let now = Instant::now();
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1)], now, &nothing_held);
        let _ = refreshes.next(now);
        refreshes.seen(now, &[seen(1, taken(5))]);
        assert_eq!(refreshes.owed().collect::<Vec<_>>(), vec![wid(1)]);
        let _ = refreshes.next(now + CHANGE_EVERY);
        refreshes.seen(now + CHANGE_EVERY, &[seen(1, taken(6))]);
        assert!(refreshes.owed().next().is_none());
    }

    /// SkyLight captures only what is on a display: a window parked, clipped or gone from the server
    /// is owed ScreenCaptureKit's capture instead, and a capture the cache refused ends a refresh with
    /// nothing more owed, since every later one at that size is refused too.
    #[test]
    fn a_window_not_captured_wholly_on_the_display_is_owed_elsewhere_and_a_refused_one_nothing() {
        let now = Instant::now();
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1), target(2), target(3)], now, &held(7));
        let _ = refreshes.next(now);
        let elsewhere = refreshes.seen(now, &[seen(1, Capture::Missed), seen(2, Capture::Refused), seen(3, taken(7))]);
        assert_eq!(elsewhere, vec![target(1)]);
        assert_eq!(refreshes.owed().collect::<Vec<_>>(), vec![wid(3)]);
    }

    /// A window whose look turns again while it is owed is owed afresh, judged from what is held then
    /// and with every try; one forgotten is owed nothing, and what a round saw of it is dropped.
    #[test]
    fn a_window_asked_again_is_owed_afresh_and_a_forgotten_one_nothing() {
        let now = Instant::now();
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1)], now, &held(7));
        let _ = refreshes.next(now);
        refreshes.seen(now, &[seen(1, taken(7))]);
        let later = now + Duration::from_millis(5);
        refreshes.ask(vec![target(1)], later, &held(9));
        assert_eq!(refreshes.wake(), Some(later));
        let _ = refreshes.next(later);
        refreshes.seen(later, &[seen(1, taken(7))]);
        assert!(refreshes.owed().next().is_none(), "judged from the band held when asked again");

        refreshes.ask(vec![target(2)], now, &held(7));
        let _ = refreshes.next(now);
        refreshes.forget(wid(2));
        assert!(refreshes.seen(now, &[seen(2, Capture::Missed)]).is_empty());
        assert!(refreshes.owed().next().is_none());
    }
}
