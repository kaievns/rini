//! Windows pictured afresh at rest once their look has turned: through SkyLight where the window
//! server has them wholly on the display, and through ScreenCaptureKit anywhere else.
//!
//! Pure. Asks and what each look came to go in with `now`; the window to look at, and one owed a
//! ScreenCaptureKit capture instead, come out. `crate::pictures::domain::schedule` holds the looks off
//! while a transition holds the service and makes them before any ScreenCaptureKit call. The rules and
//! why each holds: "The schedule" in `src/pictures/docs/README.md`.

use std::time::Instant;

use rini_core::ids::WindowId;
use rini_geometry::space::ScreenRect;

use crate::pictures::domain::print::print_changed;
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::domain::watch::{Accept, CHANGE_EVERY, CHANGE_TRIES, Capture, Look, Seen, Set};

/// Every window owed a picture at rest, and the one the last round looked at.
#[derive(Default)]
pub struct Refreshes {
    windows: Vec<Refresh>,
    looked: Option<WindowId>,
}

struct Refresh {
    target: PictureTarget,
    /// The band print of its first capture, which its turn is judged from.
    first: Option<Vec<u8>>,
    left: u32,
    next: Instant,
}

impl Refreshes {
    /// Owes each target a picture, looked at from `from` on, and holds every window already owed one
    /// until then too: a press that turns a look may start a flight, whose hold comes after it. A
    /// window already owed one is owed it afresh: its look has turned again.
    pub fn ask(&mut self, targets: Vec<PictureTarget>, from: Instant) {
        for refresh in &mut self.windows {
            refresh.next = refresh.next.max(from);
        }
        for target in targets {
            self.forget(target.window);
            self.windows.push(Refresh {
                target,
                first: None,
                left: CHANGE_TRIES,
                next: from,
            });
        }
    }

    pub fn forget(&mut self, window: WindowId) { self.windows.retain(|refresh| refresh.target.window != window) }

    pub fn owed(&self) -> impl Iterator<Item = WindowId> + '_ { self.windows.iter().map(|refresh| refresh.target.window) }

    /// The window due a look soonest at `now`, taken wholly on the display wherever it is, with
    /// nothing composited behind it but the desktop. One window a round, so a round is one SkyLight
    /// call, two for a window seen for the first time.
    pub fn next(&mut self, now: Instant) -> Option<Set> {
        let refresh = self.windows.iter().filter(|refresh| refresh.next <= now).min_by_key(|refresh| refresh.next)?;
        self.looked = Some(refresh.target.window);
        Some(Set {
            looks: vec![Look {
                target: refresh.target,
                accepts: vec![Accept::OnDisplay],
                thumbprint: false,
            }],
            behind: Vec::new(),
        })
    }

    /// When a look is next due, if a window is owed one.
    pub fn wake(&self) -> Option<Instant> { self.windows.iter().map(|refresh| refresh.next).min() }

    /// What the window server and the capture said of the window `next` handed out, on `display`. It
    /// is done at the first capture whose band print differs from its first capture's, once its tries
    /// run out, or at a capture the cache refused. A capture that missed it wholly on the display
    /// spends a try. One the window server does not have wholly on the display, or one no try
    /// captured, is returned, owed a ScreenCaptureKit capture.
    pub fn seen(&mut self, now: Instant, seen: &[Seen], display: Option<ScreenRect>) -> Option<PictureTarget> {
        let window = self.looked.take()?;
        let at = self.windows.iter().position(|refresh| refresh.target.window == window)?;
        let seen = seen.iter().find(|seen| seen.window == window);
        let on_display = seen
            .and_then(|seen| seen.frame)
            .zip(display)
            .is_some_and(|(frame, display)| Accept::OnDisplay.accepts(frame, display));
        let refresh = &mut self.windows[at];
        let (done, elsewhere) = match seen.map(|seen| &seen.capture) {
            // Every later capture at this size is refused too.
            Some(Capture::Refused) => (true, false),
            Some(Capture::Taken(prints)) => {
                let turned =
                    refresh.first.as_deref().is_some_and(|first| print_changed(Some(first), &prints.band));
                refresh.first.get_or_insert_with(|| prints.band.clone());
                refresh.spend(now);
                (turned || refresh.left == 0, false)
            }
            _ if on_display => {
                refresh.spend(now);
                let done = refresh.left == 0;
                (done, done && refresh.first.is_none())
            }
            _ => (true, true),
        };
        let target = refresh.target;
        if done {
            self.windows.remove(at);
        }
        elsewhere.then_some(target)
    }
}

impl Refresh {
    fn spend(&mut self, now: Instant) {
        self.left = self.left.saturating_sub(1);
        self.next = now + CHANGE_EVERY;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rini_core::ids::WindowServerId;
    use rini_geometry::space::{ScreenPoint, ScreenSize};

    use super::*;
    use crate::pictures::domain::watch::Prints;

    const LAPTOP: ScreenRect = rect(0.0, 0.0, 1728.0, 1117.0);

    const fn rect(x: f64, y: f64, w: f64, h: f64) -> ScreenRect {
        ScreenRect::new(ScreenPoint::new(x, y), ScreenSize::new(w, h))
    }

    fn wid(idx: u32) -> WindowId { WindowId::new(1, idx) }

    fn target(idx: u32) -> PictureTarget {
        PictureTarget {
            window: wid(idx),
            server_id: WindowServerId::new(idx),
            size: ScreenSize::new(859.0, 1081.0),
        }
    }

    fn looked(set: Option<Set>) -> Vec<WindowId> {
        set.map(|set| set.looks.into_iter().map(|look| look.target.window).collect()).unwrap_or_default()
    }

    fn on(idx: u32, capture: Capture) -> Seen { at(idx, Some(rect(4.0, 32.0, 859.0, 1081.0)), capture) }

    fn at(idx: u32, frame: Option<ScreenRect>, capture: Capture) -> Seen {
        Seen {
            window: wid(idx),
            frame,
            capture,
        }
    }

    fn taken(band: u8) -> Capture {
        Capture::Taken(Prints {
            band: vec![band],
            thumb: None,
        })
    }

    /// One look at the window `next` hands out at `now`, which saw `seen`.
    fn round(refreshes: &mut Refreshes, now: Instant, seen: Seen) -> Option<PictureTarget> {
        assert_eq!(looked(refreshes.next(now)), vec![seen.window]);
        refreshes.seen(now, &[seen], Some(LAPTOP))
    }

    /// Nothing is looked at before its start, and then each window due is, one a round, taken wholly
    /// on the display wherever the window server has it, with only the desktop composited behind it.
    #[test]
    fn a_refresh_is_looked_at_from_its_start_one_window_a_round_with_nothing_behind_it() {
        let now = Instant::now();
        let from = now + Duration::from_millis(50);
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1), target(2)], from);
        assert_eq!(refreshes.wake(), Some(from));
        assert!(refreshes.next(now).is_none(), "not yet");
        let set = refreshes.next(from).expect("due");
        assert!(set.behind.is_empty());
        assert_eq!(set.looks[0].accepts, vec![Accept::OnDisplay]);
        assert_eq!(looked(Some(set)), vec![wid(1)]);
        refreshes.seen(from, &[on(1, taken(1))], Some(LAPTOP));
        assert_eq!(looked(refreshes.next(from)), vec![wid(2)]);
    }

    /// An app repaints a while after its focus changes, and a picture held from before can differ in
    /// its band for other reasons: the turn is judged from the refresh's own first capture. One
    /// captured before the repaint is looked at again until a capture differs; one whose band never
    /// changes is done when its tries run out, its last capture held.
    #[test]
    fn a_refresh_ends_at_the_first_capture_that_differs_from_its_first_or_when_its_tries_run_out() {
        let now = Instant::now();
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1)], now);
        assert!(round(&mut refreshes, now, on(1, taken(8))).is_none());
        assert_eq!(refreshes.wake(), Some(now + CHANGE_EVERY), "the first capture is never the turn");
        assert!(refreshes.next(now).is_none(), "looked at again only after CHANGE_EVERY");
        round(&mut refreshes, now + CHANGE_EVERY, on(1, taken(8)));
        round(&mut refreshes, now + CHANGE_EVERY * 2, on(1, taken(9)));
        assert!(refreshes.owed().next().is_none(), "turned");

        refreshes.ask(vec![target(2)], now);
        let mut at = now;
        for _ in 0..CHANGE_TRIES {
            assert!(round(&mut refreshes, at, on(2, taken(7))).is_none());
            at += CHANGE_EVERY;
        }
        assert!(refreshes.owed().next().is_none(), "out of tries");
    }

    /// SkyLight captures only what is on a display: a window parked, clipped or with no frame on the
    /// server is owed ScreenCaptureKit's capture instead. One wholly on the display whose capture
    /// missed is looked at again, a try spent, and owed ScreenCaptureKit's only if no try captured it.
    /// A capture the cache refused ends a refresh with nothing more owed: every later one at that
    /// size is refused too.
    #[test]
    fn only_a_window_off_the_display_or_never_captured_is_owed_elsewhere_and_a_refused_one_nothing() {
        let now = Instant::now();
        let parked = rect(1727.0, 32.0, 859.0, 1081.0);
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1), target(2), target(3)], now);
        assert_eq!(round(&mut refreshes, now, at(1, Some(parked), Capture::Missed)), Some(target(1)));
        assert_eq!(round(&mut refreshes, now, at(2, None, Capture::Missed)), Some(target(2)));
        assert!(round(&mut refreshes, now, on(3, Capture::Refused)).is_none());
        assert!(refreshes.owed().next().is_none());

        refreshes.ask(vec![target(4)], now);
        assert!(refreshes.next(now).is_some());
        assert_eq!(refreshes.seen(now, &[on(4, Capture::Missed)], None), Some(target(4)), "no display held");

        refreshes.ask(vec![target(5), target(6)], now);
        let mut at = now;
        for _ in 0..CHANGE_TRIES - 1 {
            assert!(round(&mut refreshes, at, on(5, Capture::Missed)).is_none());
            let sixth = if at == now { taken(1) } else { Capture::Missed };
            assert!(round(&mut refreshes, at, on(6, sixth)).is_none());
            at += CHANGE_EVERY;
        }
        assert_eq!(round(&mut refreshes, at, on(5, Capture::Missed)), Some(target(5)), "never captured");
        assert!(round(&mut refreshes, at, on(6, Capture::Missed)).is_none(), "its first capture is held");
        assert!(refreshes.owed().next().is_none());
    }

    /// A press that turns a look may also start a flight, whose hold reaches the service after the
    /// refresh: every window already being looked at waits for the new refresh's start too. A window
    /// whose look turns again is owed afresh, judged from its next capture; a forgotten one is owed
    /// nothing, and what its round saw is dropped.
    #[test]
    fn an_ask_holds_every_refresh_to_its_start_and_a_window_asked_again_is_owed_afresh() {
        let now = Instant::now();
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1), target(2)], now);
        round(&mut refreshes, now, on(1, taken(7)));
        let later = now + Duration::from_millis(5);
        let from = later + Duration::from_millis(50);
        refreshes.ask(vec![target(1), target(3)], from);
        assert_eq!(refreshes.wake(), Some(from), "window 2 waits too");
        assert!(refreshes.next(later).is_none());

        round(&mut refreshes, from, on(2, taken(1)));
        round(&mut refreshes, from, on(1, taken(9)));
        assert!(refreshes.owed().any(|window| window == wid(1)), "judged from its next capture");

        assert_eq!(looked(refreshes.next(from)), vec![wid(3)]);
        refreshes.forget(wid(3));
        assert!(refreshes.seen(from, &[on(3, Capture::Missed)], Some(LAPTOP)).is_none());
        assert!(refreshes.owed().all(|window| window != wid(3)));
    }
}
