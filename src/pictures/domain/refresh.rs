//! Windows pictured afresh at rest once their look has turned: through SkyLight where the window
//! server has them wholly on the display, and through ScreenCaptureKit anywhere else.
//!
//! Pure. Asks, holds and what each look came to go in with `now`; the window to look at, and one owed
//! a ScreenCaptureKit capture instead, come out. A press cancels every refresh, and the release asks
//! again for each its flight did not picture; `crate::pictures::domain::schedule` makes the looks
//! before any ScreenCaptureKit call. The rules and why each holds: "The schedule" in
//! `src/pictures/docs/README.md`.

use std::time::{Duration, Instant};

use rini_core::ids::WindowId;
use rini_geometry::space::ScreenRect;

use crate::pictures::domain::print::print_changed;
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::domain::watch::{Accept, CHANGE_EVERY, CHANGE_TRIES, Capture, Look, Seen, Set};

/// How long after a look turns a picture of the window shows the turn: above the 68ms p90 repaint
/// after make-key ("Where a moving focus change spends its time" in
/// `src/animation/docs/capture-overlay-research.md`).
pub const REPAINT_SETTLED: Duration = Duration::from_millis(70);

/// Every window owed a picture at rest, and the one the last round looked at.
#[derive(Default)]
pub struct Refreshes {
    windows: Vec<Refresh>,
    looked: Option<WindowId>,
}

struct Refresh {
    target: PictureTarget,
    /// When its look turned, as far as the service heard it.
    asked: Instant,
    /// `None` once a press cancelled it, until the release asks again.
    poll: Option<Poll>,
}

struct Poll {
    /// The band print of its first capture, which its turn is judged from.
    first: Option<Vec<u8>>,
    left: u32,
    next: Instant,
    /// Asked again at a release, and not looked at since: a picture of the window taken
    /// `REPAINT_SETTLED` after `asked` already shows the turn.
    again: bool,
}

impl Poll {
    fn fresh(next: Instant, again: bool) -> Self {
        Self {
            first: None,
            left: CHANGE_TRIES,
            next,
            again,
        }
    }

    fn spend(&mut self, now: Instant) {
        self.left = self.left.saturating_sub(1);
        self.next = now + CHANGE_EVERY;
    }
}

impl Refreshes {
    /// Owes each target a picture, its look turned at `asked` and looked at from `from` on, and holds
    /// every window already owed one until then too: a press that turns a look may start a flight,
    /// whose hold comes after it. A window already owed one is owed it afresh: its look has turned
    /// again.
    pub fn ask(&mut self, targets: Vec<PictureTarget>, asked: Instant, from: Instant) {
        for poll in self.windows.iter_mut().filter_map(|refresh| refresh.poll.as_mut()) {
            poll.next = poll.next.max(from);
        }
        for target in targets {
            self.forget(target.window);
            self.windows.push(Refresh {
                target,
                asked,
                poll: Some(Poll::fresh(from, false)),
            });
        }
    }

    /// A press: every refresh stops, keeping only its window and when its look turned. Nothing of it is
    /// looked at again before `release`.
    pub fn cancel(&mut self) {
        for refresh in &mut self.windows {
            refresh.poll = None;
        }
    }

    /// The release after a press: each refresh it cancelled is owed afresh from `from`, judged from a
    /// first capture of its own, since the flight may have moved or resized the window since.
    pub fn release(&mut self, from: Instant) {
        for refresh in self.windows.iter_mut().filter(|refresh| refresh.poll.is_none()) {
            refresh.poll = Some(Poll::fresh(from, true));
        }
    }

    pub fn forget(&mut self, window: WindowId) { self.windows.retain(|refresh| refresh.target.window != window) }

    /// Every window owed a picture, cancelled or not.
    pub fn owed(&self) -> impl Iterator<Item = WindowId> + '_ { self.windows.iter().map(|refresh| refresh.target.window) }

    /// The window due a look soonest at `now`, taken wholly on the display wherever it is, with
    /// nothing composited behind it but the desktop. One window a round. `pictured` is when the
    /// picture held of a window was taken: a refresh asked again at a release is done before its first
    /// look if that is `REPAINT_SETTLED` after its look turned. A picture taken sooner, frame zero's at
    /// the press among them, can predate the repaint.
    pub fn next(&mut self, now: Instant, pictured: impl Fn(WindowId) -> Option<Instant>) -> Option<Set> {
        self.windows.retain(|refresh| {
            let again = refresh.poll.as_ref().is_some_and(|poll| poll.again);
            let settled = refresh.asked + REPAINT_SETTLED;
            !(again && pictured(refresh.target.window).is_some_and(|taken| taken >= settled))
        });
        let (target, poll) = self
            .windows
            .iter_mut()
            .filter_map(|refresh| Some((refresh.target, refresh.poll.as_mut()?)))
            .filter(|(_, poll)| poll.next <= now)
            .min_by_key(|(_, poll)| poll.next)?;
        poll.again = false;
        self.looked = Some(target.window);
        Some(Set {
            looks: vec![Look {
                target,
                accepts: vec![Accept::OnDisplay],
                thumbprint: false,
            }],
            behind: Vec::new(),
        })
    }

    /// When a look is next due, if a window is owed one that no press has cancelled.
    pub fn wake(&self) -> Option<Instant> {
        self.windows.iter().filter_map(|refresh| refresh.poll.as_ref()).map(|poll| poll.next).min()
    }

    /// What the window server and the capture said of the window `next` handed out, on `display`. It
    /// is done at the first capture whose band print differs from its first capture's, once its tries
    /// run out, or at a capture the cache refused. A capture that missed it wholly on the display
    /// spends a try. One the window server does not have wholly on the display, or one no try
    /// captured, is returned, owed a ScreenCaptureKit capture. One not in `seen`, its call never made
    /// for a press on its way, spends nothing and is owed nowhere else.
    pub fn seen(&mut self, now: Instant, seen: &[Seen], display: Option<ScreenRect>) -> Option<PictureTarget> {
        let window = self.looked.take()?;
        let seen = seen.iter().find(|seen| seen.window == window)?;
        let at = self.windows.iter().position(|refresh| refresh.target.window == window)?;
        let on_display =
            seen.frame.zip(display).is_some_and(|(frame, display)| Accept::OnDisplay.accepts(frame, display));
        let target = self.windows[at].target;
        let poll = self.windows[at].poll.as_mut()?;
        let (done, elsewhere) = match &seen.capture {
            // Every later capture at this size is refused too.
            Capture::Refused => (true, false),
            Capture::Taken(prints) => {
                let turned =
                    poll.first.as_deref().is_some_and(|first| print_changed(Some(first), &prints.band));
                poll.first.get_or_insert_with(|| prints.band.clone());
                poll.spend(now);
                (turned || poll.left == 0, false)
            }
            _ if on_display => {
                poll.spend(now);
                let done = poll.left == 0;
                (done, done && poll.first.is_none())
            }
            _ => (true, true),
        };
        if done {
            self.windows.remove(at);
        }
        elsewhere.then_some(target)
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

    fn unpictured(_: WindowId) -> Option<Instant> { None }

    /// One look at the window `next` hands out at `now`, which saw `seen`.
    fn round(refreshes: &mut Refreshes, now: Instant, seen: Seen) -> Option<PictureTarget> {
        assert_eq!(looked(refreshes.next(now, unpictured)), vec![seen.window]);
        refreshes.seen(now, &[seen], Some(LAPTOP))
    }

    /// Nothing is looked at before its start, and then each window due is, one a round, taken wholly
    /// on the display wherever the window server has it, with only the desktop composited behind it.
    #[test]
    fn a_refresh_is_looked_at_from_its_start_one_window_a_round_with_nothing_behind_it() {
        let now = Instant::now();
        let from = now + Duration::from_millis(50);
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1), target(2)], now, from);
        assert_eq!(refreshes.wake(), Some(from));
        assert!(refreshes.next(now, unpictured).is_none(), "not yet");
        let set = refreshes.next(from, unpictured).expect("due");
        assert!(set.behind.is_empty());
        assert_eq!(set.looks[0].accepts, vec![Accept::OnDisplay]);
        assert_eq!(looked(Some(set)), vec![wid(1)]);
        refreshes.seen(from, &[on(1, taken(1))], Some(LAPTOP));
        assert_eq!(looked(refreshes.next(from, unpictured)), vec![wid(2)]);
    }

    /// An app repaints a while after its focus changes, and a picture held from before can differ in
    /// its band for other reasons: the turn is judged from the refresh's own first capture. One
    /// captured before the repaint is looked at again until a capture differs; one whose band never
    /// changes is done when its tries run out, its last capture held.
    #[test]
    fn a_refresh_ends_at_the_first_capture_that_differs_from_its_first_or_when_its_tries_run_out() {
        let now = Instant::now();
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1)], now, now);
        assert!(round(&mut refreshes, now, on(1, taken(8))).is_none());
        assert_eq!(refreshes.wake(), Some(now + CHANGE_EVERY), "the first capture is never the turn");
        assert!(refreshes.next(now, unpictured).is_none(), "looked at again only after CHANGE_EVERY");
        round(&mut refreshes, now + CHANGE_EVERY, on(1, taken(8)));
        round(&mut refreshes, now + CHANGE_EVERY * 2, on(1, taken(9)));
        assert!(refreshes.owed().next().is_none(), "turned");

        refreshes.ask(vec![target(2)], now, now);
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
        refreshes.ask(vec![target(1), target(2), target(3)], now, now);
        assert_eq!(round(&mut refreshes, now, at(1, Some(parked), Capture::Missed)), Some(target(1)));
        assert_eq!(round(&mut refreshes, now, at(2, None, Capture::Missed)), Some(target(2)));
        assert!(round(&mut refreshes, now, on(3, Capture::Refused)).is_none());
        assert!(refreshes.owed().next().is_none());

        refreshes.ask(vec![target(4)], now, now);
        assert!(refreshes.next(now, unpictured).is_some());
        assert_eq!(refreshes.seen(now, &[on(4, Capture::Missed)], None), Some(target(4)), "no display held");

        refreshes.ask(vec![target(5), target(6)], now, now);
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
        refreshes.ask(vec![target(1), target(2)], now, now);
        round(&mut refreshes, now, on(1, taken(7)));
        let later = now + Duration::from_millis(5);
        let from = later + Duration::from_millis(50);
        refreshes.ask(vec![target(1), target(3)], later, from);
        assert_eq!(refreshes.wake(), Some(from), "window 2 waits too");
        assert!(refreshes.next(later, unpictured).is_none());

        round(&mut refreshes, from, on(2, taken(1)));
        round(&mut refreshes, from, on(1, taken(9)));
        assert!(refreshes.owed().any(|window| window == wid(1)), "judged from its next capture");

        assert_eq!(looked(refreshes.next(from, unpictured)), vec![wid(3)]);
        refreshes.forget(wid(3));
        assert!(refreshes.seen(from, &[on(3, Capture::Missed)], Some(LAPTOP)).is_none());
        assert!(refreshes.owed().all(|window| window != wid(3)));
    }

    /// A press cancels every refresh, looked at or not: nothing of it is looked at while held, and it
    /// is still owed. The release owes it afresh from the release on, judged from a first capture of
    /// its own with every try, since the flight may have moved or resized the window.
    #[test]
    fn a_press_cancels_every_refresh_and_the_release_asks_again_with_a_fresh_first_capture_and_tries() {
        let now = Instant::now();
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1), target(2)], now, now);
        round(&mut refreshes, now, on(1, taken(7)));
        round(&mut refreshes, now, on(2, taken(7)));
        refreshes.cancel();
        assert_eq!(refreshes.wake(), None);
        assert!(refreshes.next(now + CHANGE_EVERY, unpictured).is_none(), "cancelled");
        assert_eq!(refreshes.owed().collect::<Vec<_>>(), vec![wid(1), wid(2)], "still owed");

        let release = now + Duration::from_millis(400);
        refreshes.release(release);
        assert_eq!(refreshes.wake(), Some(release));
        refreshes.forget(wid(2));
        let (mut looks, mut at) = (0, release);
        while refreshes.owed().next().is_some() {
            assert!(round(&mut refreshes, at, on(1, taken(8))).is_none(), "8 is its first capture now");
            looks += 1;
            at += CHANGE_EVERY;
        }
        assert_eq!(looks, CHANGE_TRIES, "every try");
    }

    /// Asked again at a release, a window is done unlooked at when the cache holds a picture of it
    /// taken `REPAINT_SETTLED` after its look turned; one taken sooner, frame zero's at the press
    /// among them, can predate the repaint. Only a refresh asked again is judged so, and only before
    /// its first look: after that its own captures judge it.
    #[test]
    fn a_refresh_asked_again_is_done_only_where_a_picture_taken_repaint_settled_after_its_turn_is_held() {
        let asked = Instant::now();
        let release = asked + Duration::from_millis(300);
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1), target(2)], asked, asked);
        refreshes.cancel();
        refreshes.release(release);
        let soon = Duration::from_millis(1);
        let pictured =
            |window: WindowId| Some(asked + REPAINT_SETTLED - if window == wid(1) { Duration::ZERO } else { soon });
        assert_eq!(looked(refreshes.next(release, pictured)), vec![wid(2)]);
        assert_eq!(refreshes.owed().collect::<Vec<_>>(), vec![wid(2)], "window 1 is pictured");
        refreshes.seen(release, &[on(2, taken(1))], Some(LAPTOP));
        let later = |_: WindowId| Some(release);
        assert_eq!(looked(refreshes.next(release + CHANGE_EVERY, later)), vec![wid(2)], "looked at since");

        refreshes.ask(vec![target(3)], asked, asked);
        assert_eq!(looked(refreshes.next(asked, later)), vec![wid(3)], "never cancelled");
    }

    /// A round the service stopped short of a window, for a press on its way, says nothing of it: no
    /// try is spent and nothing is owed ScreenCaptureKit's capture. The press's hold cancels it next.
    #[test]
    fn a_window_cut_from_a_round_spends_nothing_and_is_owed_nowhere_else() {
        let now = Instant::now();
        let mut refreshes = Refreshes::default();
        refreshes.ask(vec![target(1)], now, now);
        assert_eq!(looked(refreshes.next(now, unpictured)), vec![wid(1)]);
        assert!(refreshes.seen(now, &[], Some(LAPTOP)).is_none());
        assert_eq!(refreshes.wake(), Some(now), "no try spent");
        let mut at = now;
        for _ in 0..CHANGE_TRIES {
            assert!(round(&mut refreshes, at, on(1, taken(7))).is_none());
            at += CHANGE_EVERY;
        }
        assert!(refreshes.owed().next().is_none());
    }
}
