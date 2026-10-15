//! Windows pictured afresh at rest once their look has turned: through SkyLight where the window
//! server has them wholly on the display, and through ScreenCaptureKit anywhere else.
//!
//! Pure. Asks, holds and what each look came to go in with `now`; the windows to look at, the capture
//! each settled on, and those owed a ScreenCaptureKit capture instead, come out. Every window due is
//! looked at in one round, and each is pictured once, with the capture its look settled on. A press
//! cancels every refresh, and the release asks again for each its flight did not picture;
//! `crate::pictures::domain::schedule` makes the looks before any ScreenCaptureKit call. The rules and
//! why each holds: "The schedule" in `src/pictures/docs/README.md`.

use std::time::{Duration, Instant};

use rini_core::ids::WindowId;
use rini_geometry::space::ScreenRect;

use crate::pictures::domain::print::print_changed;
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::domain::watch::{
    Accept, CHANGE_EVERY, CHANGE_TRIES, Capture, Look, Seen, Set,
};

/// How long after a look turns a picture of the window shows the turn: above the 68ms p90 repaint
/// after make-key ("Where a moving focus change spends its time" in
/// `src/animation/docs/capture-overlay-research.md`).
pub const REPAINT_SETTLED: Duration = Duration::from_millis(70);

/// Every window owed a picture at rest, and the round last handed out. `C` is a capture, held from the
/// round that took it until its window settles.
pub struct Refreshes<C> {
    windows: Vec<Refresh<C>>,
    looked: Option<Round>,
}

impl<C> Default for Refreshes<C> {
    fn default() -> Self {
        Self {
            windows: Vec::new(),
            looked: None,
        }
    }
}

/// The windows a round looked at, and when it was handed out: each capture of it was taken after.
struct Round {
    windows: Vec<WindowId>,
    at: Instant,
}

struct Refresh<C> {
    target: PictureTarget,
    /// When its look turned, as far as the service heard it.
    asked: Instant,
    /// `None` once a press cancelled it, until the release asks again.
    poll: Option<Poll<C>>,
}

struct Poll<C> {
    /// Its latest capture the cache would take, and that capture's band print, which the next one is
    /// judged against.
    latest: Option<(Vec<u8>, C)>,
    left: u32,
    next: Instant,
    /// Asked again at a release, and not looked at since: a picture of the window taken
    /// `REPAINT_SETTLED` after `asked` already shows the turn.
    again: bool,
}

impl<C> Poll<C> {
    fn fresh(next: Instant, again: bool) -> Self {
        Self {
            latest: None,
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

/// What a round came to.
#[derive(Debug, PartialEq)]
pub struct Refreshed<C> {
    /// Each window done with a capture, the one its look settled on: put to the cache, once.
    pub settled: Vec<(WindowId, C)>,
    /// Each window owed a ScreenCaptureKit capture instead.
    pub elsewhere: Vec<PictureTarget>,
}

/// How a window's refresh ends.
enum End {
    /// With its latest capture, or, with none, owed ScreenCaptureKit's.
    Latest,
    Elsewhere,
    Nothing,
}

impl<C> Refreshes<C> {
    /// Owes each target a picture, its look turned at `asked` and looked at from `from` on, and holds
    /// every window already owed one until then too: a press that turns a look may start a flight,
    /// whose hold comes after it, and the windows of one change are looked at together. A window
    /// already owed one is owed it afresh: its look has turned again.
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

    /// A press: every refresh stops, keeping only its window and when its look turned, its capture
    /// held dropped. Nothing of it is looked at again before `release`.
    pub fn cancel(&mut self) {
        for refresh in &mut self.windows {
            refresh.poll = None;
        }
    }

    /// The release after a press: each refresh it cancelled is owed afresh from `from`, judged from
    /// captures of its own, since the flight may have moved or resized the window since.
    pub fn release(&mut self, from: Instant) {
        for refresh in self.windows.iter_mut().filter(|refresh| refresh.poll.is_none()) {
            refresh.poll = Some(Poll::fresh(from, true));
        }
    }

    /// A window that is gone: owed nothing, and its capture held dropped.
    pub fn forget(&mut self, window: WindowId) {
        self.windows.retain(|refresh| refresh.target.window != window)
    }

    /// Every window owed a picture, cancelled or not.
    #[cfg(test)]
    pub fn owed(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.windows.iter().map(|refresh| refresh.target.window)
    }

    /// Every window due a look at `now`, in one round, each taken wholly on the display wherever it
    /// is, with nothing composited behind it but the desktop. `pictured` is when the picture held of a
    /// window was taken: a refresh asked again at a release is done before its first look if that is
    /// `REPAINT_SETTLED` after its look turned. A picture taken sooner, frame zero's at the press among
    /// them, can predate the repaint.
    pub fn next(
        &mut self,
        now: Instant,
        pictured: impl Fn(WindowId) -> Option<Instant>,
    ) -> Option<Set> {
        self.windows.retain(|refresh| {
            let again = refresh.poll.as_ref().is_some_and(|poll| poll.again);
            let settled = refresh.asked + REPAINT_SETTLED;
            !(again && pictured(refresh.target.window).is_some_and(|taken| taken >= settled))
        });
        let mut windows = Vec::new();
        let mut looks = Vec::new();
        for refresh in &mut self.windows {
            let Some(poll) = refresh.poll.as_mut().filter(|poll| poll.next <= now) else {
                continue;
            };
            poll.again = false;
            windows.push(refresh.target.window);
            looks.push(Look {
                target: refresh.target,
                accepts: vec![Accept::OnDisplay],
                thumbprint: false,
            });
        }
        if looks.is_empty() {
            return None;
        }
        self.looked = Some(Round { windows, at: now });
        Some(Set { looks, behind: Vec::new() })
    }

    /// When a look is next due, if a window is owed one that no press has cancelled.
    pub fn wake(&self) -> Option<Instant> {
        self.windows
            .iter()
            .filter_map(|refresh| refresh.poll.as_ref())
            .map(|poll| poll.next)
            .min()
    }

    /// What the window server and the captures said of the windows `next` handed out, on `display`,
    /// with the capture of each `seen` has `Taken`, which the cache would take. A window's look has
    /// settled at a capture with the band print of the one before it, taken `REPAINT_SETTLED` after
    /// its look turned; it is done then, or once its tries run out, with that capture or its latest,
    /// and settled windows drop out of the rounds after. One the cache would refuse is done with
    /// nothing: every later capture at that size is refused too. A capture that missed it wholly on
    /// the display spends a try. One the window server does not have wholly on the display, or one no
    /// try captured, is owed a ScreenCaptureKit capture. One not in `seen`, its call never made for a
    /// press on its way, spends nothing and is owed nowhere else.
    pub fn seen(
        &mut self,
        now: Instant,
        seen: &[Seen],
        mut captures: Vec<(WindowId, C)>,
        display: Option<ScreenRect>,
    ) -> Refreshed<C> {
        let mut refreshed = Refreshed {
            settled: Vec::new(),
            elsewhere: Vec::new(),
        };
        let Some(round) = self.looked.take() else {
            return refreshed;
        };
        for window in round.windows {
            let Some(seen) = seen.iter().find(|seen| seen.window == window) else {
                continue;
            };
            let Some(at) = self.windows.iter().position(|refresh| refresh.target.window == window)
            else {
                continue;
            };
            let capture = captures
                .iter()
                .position(|(of, _)| *of == window)
                .map(|at| captures.swap_remove(at).1);
            let on_display = seen
                .frame
                .zip(display)
                .is_some_and(|(frame, display)| Accept::OnDisplay.accepts(frame, display));
            let refresh = &mut self.windows[at];
            let repainted = round.at >= refresh.asked + REPAINT_SETTLED;
            let Some(poll) = refresh.poll.as_mut() else { continue };
            let end = match (&seen.capture, capture) {
                (Capture::Refused, _) => Some(End::Nothing),
                (Capture::Taken(prints), Some(capture)) => {
                    let alike = poll
                        .latest
                        .as_ref()
                        .is_some_and(|(band, _)| !print_changed(Some(band), &prints.band));
                    poll.latest = Some((prints.band.clone(), capture));
                    poll.spend(now);
                    (alike && repainted || poll.left == 0).then_some(End::Latest)
                }
                _ if on_display => {
                    poll.spend(now);
                    (poll.left == 0).then_some(End::Latest)
                }
                _ => Some(End::Elsewhere),
            };
            let Some(end) = end else { continue };
            let Refresh { target, poll, .. } = self.windows.remove(at);
            match (end, poll.and_then(|poll| poll.latest)) {
                (End::Latest, Some((_, capture))) => refreshed.settled.push((window, capture)),
                (End::Latest | End::Elsewhere, _) => refreshed.elsewhere.push(target),
                (End::Nothing, _) => {}
            }
        }
        refreshed
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
        set.map(|set| set.looks.into_iter().map(|look| look.target.window).collect())
            .unwrap_or_default()
    }

    fn on(idx: u32, capture: Capture) -> Seen {
        at(idx, Some(rect(4.0, 32.0, 859.0, 1081.0)), capture)
    }

    fn at(idx: u32, frame: Option<ScreenRect>, capture: Capture) -> Seen {
        Seen {
            window: wid(idx),
            frame,
            capture,
        }
    }

    fn taken(band: u8) -> Capture { Capture::Taken(Prints { band: vec![band], thumb: None }) }

    fn unpictured(_: WindowId) -> Option<Instant> { None }

    /// The captures of a round, each window `Taken` handed `capture`, as the service hands them.
    fn captures(seen: &[Seen], capture: u32) -> Vec<(WindowId, u32)> {
        seen.iter()
            .filter(|seen| matches!(seen.capture, Capture::Taken(_)))
            .map(|seen| (seen.window, capture))
            .collect()
    }

    /// One round at `now`, which looks at the windows `seen` names, in that order, and saw them so,
    /// each capture taken marked `capture`.
    fn round(
        refreshes: &mut Refreshes<u32>,
        now: Instant,
        seen: Vec<Seen>,
        capture: u32,
    ) -> Refreshed<u32> {
        let windows: Vec<WindowId> = seen.iter().map(|seen| seen.window).collect();
        assert_eq!(looked(refreshes.next(now, unpictured)), windows);
        let captures = captures(&seen, capture);
        refreshes.seen(now, &seen, captures, Some(LAPTOP))
    }

    fn nothing() -> Refreshed<u32> {
        Refreshed {
            settled: Vec::new(),
            elsewhere: Vec::new(),
        }
    }

    fn settled(windows: &[(u32, u32)]) -> Refreshed<u32> {
        Refreshed {
            settled: windows.iter().map(|(idx, capture)| (wid(*idx), *capture)).collect(),
            elsewhere: Vec::new(),
        }
    }

    /// Nothing is looked at before its start, and then every window due is, in one round, each taken
    /// wholly on the display wherever the window server has it, with only the desktop composited
    /// behind it: both ends of a focus change are asked together, so they are captured together.
    #[test]
    fn a_refresh_looks_at_every_window_due_in_one_round_from_its_start_with_nothing_behind_it() {
        let now = Instant::now();
        let from = now + Duration::from_millis(50);
        let mut refreshes = Refreshes::<u32>::default();
        refreshes.ask(vec![target(1), target(2)], now, from);
        assert_eq!(refreshes.wake(), Some(from));
        assert!(refreshes.next(now, unpictured).is_none(), "not yet");
        let set = refreshes.next(from, unpictured).expect("due");
        assert!(set.behind.is_empty());
        assert!(
            set.looks
                .iter()
                .all(|look| look.accepts == vec![Accept::OnDisplay] && !look.thumbprint)
        );
        assert_eq!(looked(Some(set)), vec![wid(1), wid(2)]);
        let seen = [on(1, taken(1)), on(2, taken(2))];
        assert_eq!(
            refreshes.seen(from, &seen, captures(&seen, 1), Some(LAPTOP)),
            nothing()
        );
        assert_eq!(
            refreshes.wake(),
            Some(from + CHANGE_EVERY),
            "both again together"
        );
        assert!(refreshes.next(from, unpictured).is_none());
        assert_eq!(looked(refreshes.next(from + CHANGE_EVERY, unpictured)), vec![
            wid(1),
            wid(2)
        ]);
    }

    /// An app repaints a while after its focus changes, and a picture held from before can differ in
    /// its band for other reasons: a window is judged by its own captures, settled at one with the
    /// band print of the capture before it taken `REPAINT_SETTLED` after its look turned. That
    /// capture is the one it is pictured with, and it drops out of the rounds after while the window
    /// beside it, repainting later, goes on. One whose band never settles is pictured with its last
    /// capture once its tries run out.
    #[test]
    fn a_window_settles_at_a_capture_like_the_one_before_taken_repaint_settled_after_its_turn() {
        let asked = Instant::now();
        let first = asked + Duration::from_millis(50);
        let at = |look: u32| first + CHANGE_EVERY * look;
        let mut refreshes = Refreshes::<u32>::default();
        refreshes.ask(vec![target(1), target(2), target(3)], asked, first);
        let seen = |bands: [u8; 3]| {
            vec![
                on(1, taken(bands[0])),
                on(2, taken(bands[1])),
                on(3, taken(bands[2])),
            ]
        };
        assert_eq!(
            round(&mut refreshes, at(0), seen([8, 7, 1]), 0),
            nothing(),
            "a first capture is never settled"
        );
        assert!(at(1) < asked + REPAINT_SETTLED);
        assert_eq!(
            round(&mut refreshes, at(1), seen([8, 9, 2]), 1),
            nothing(),
            "alike, but too soon"
        );
        assert_eq!(
            round(&mut refreshes, at(2), seen([8, 9, 3]), 2),
            settled(&[(1, 2), (2, 2)])
        );
        refreshes.ask(vec![target(4)], asked, at(3));
        for look in 3..CHANGE_TRIES - 1 {
            let band = look as u8 + 1;
            let seen = vec![on(3, taken(band)), on(4, taken(5))];
            let expected = if look == 4 {
                settled(&[(4, look)])
            } else {
                nothing()
            };
            assert_eq!(round(&mut refreshes, at(look), seen, look), expected, "{look}");
        }
        let last = CHANGE_TRIES - 1;
        let refreshed = round(&mut refreshes, at(last), vec![on(3, taken(9))], last);
        assert_eq!(refreshed, settled(&[(3, last)]), "out of tries");
        assert!(refreshes.owed().next().is_none());
    }

    /// SkyLight captures only what is on a display: a window parked, clipped or with no frame on the
    /// server is owed ScreenCaptureKit's capture instead. One wholly on the display whose capture
    /// missed is looked at again, a try spent, and owed ScreenCaptureKit's only if no try captured it;
    /// one captured before is pictured with that capture. A capture the cache would refuse ends a
    /// refresh with nothing put and nothing more owed: every later one at that size is refused too.
    #[test]
    fn only_a_window_off_the_display_or_never_captured_is_owed_elsewhere_and_a_refused_one_nothing()
    {
        let now = Instant::now();
        let parked = rect(1727.0, 32.0, 859.0, 1081.0);
        let mut refreshes = Refreshes::<u32>::default();
        refreshes.ask(vec![target(1), target(2), target(3)], now, now);
        let seen = vec![
            at(1, Some(parked), Capture::Missed),
            at(2, None, Capture::Missed),
            on(3, Capture::Refused),
        ];
        let refreshed = round(&mut refreshes, now, seen, 0);
        assert_eq!(refreshed.elsewhere, vec![target(1), target(2)]);
        assert!(refreshed.settled.is_empty());
        assert!(refreshes.owed().next().is_none());

        refreshes.ask(vec![target(4)], now, now);
        assert!(refreshes.next(now, unpictured).is_some());
        let refreshed = refreshes.seen(now, &[on(4, Capture::Missed)], Vec::new(), None);
        assert_eq!(refreshed.elsewhere, vec![target(4)], "no display held");

        refreshes.ask(vec![target(5), target(6)], now, now);
        let mut at = now;
        for look in 0..CHANGE_TRIES - 1 {
            let sixth = if look == 0 { taken(1) } else { Capture::Missed };
            assert_eq!(
                round(
                    &mut refreshes,
                    at,
                    vec![on(5, Capture::Missed), on(6, sixth)],
                    look
                ),
                nothing()
            );
            at += CHANGE_EVERY;
        }
        let refreshed = round(
            &mut refreshes,
            at,
            vec![on(5, Capture::Missed), on(6, Capture::Missed)],
            9,
        );
        assert_eq!(refreshed.elsewhere, vec![target(5)], "never captured");
        assert_eq!(refreshed.settled, vec![(wid(6), 0)], "its one capture");
        assert!(refreshes.owed().next().is_none());
    }

    /// A press that turns a look may also start a flight, whose hold reaches the service after the
    /// refresh: every window already being looked at waits for the new refresh's start too, and is
    /// looked at in its round. A window whose look turns again is owed afresh, judged from its next
    /// captures, its capture held dropped; a forgotten one is owed nothing, and what its round saw is
    /// dropped.
    #[test]
    fn an_ask_holds_every_refresh_to_its_start_and_a_window_asked_again_is_owed_afresh() {
        let now = Instant::now();
        let mut refreshes = Refreshes::<u32>::default();
        refreshes.ask(vec![target(1), target(2)], now, now);
        round(&mut refreshes, now, vec![on(1, taken(7)), on(2, taken(7))], 0);
        let later = now + Duration::from_millis(5);
        let from = later + REPAINT_SETTLED;
        refreshes.ask(vec![target(1), target(3)], later, from);
        assert_eq!(refreshes.wake(), Some(from), "window 2 waits too");
        assert!(refreshes.next(later, unpictured).is_none());

        let seen = vec![on(2, taken(7)), on(1, taken(7)), on(3, taken(7))];
        let refreshed = round(&mut refreshes, from, seen, 1);
        assert_eq!(
            refreshed,
            settled(&[(2, 1)]),
            "window 1 judged from its next capture"
        );

        assert_eq!(looked(refreshes.next(from + CHANGE_EVERY, unpictured)), vec![
            wid(1),
            wid(3)
        ]);
        refreshes.forget(wid(3));
        let seen = [on(1, taken(8)), on(3, taken(7))];
        assert_eq!(
            refreshes.seen(from + CHANGE_EVERY, &seen, captures(&seen, 2), Some(LAPTOP)),
            nothing()
        );
        assert_eq!(refreshes.owed().collect::<Vec<_>>(), vec![wid(1)]);
    }

    /// A press cancels every refresh, looked at or not, and drops the capture it held: nothing of it
    /// is looked at while held, and it is still owed. The release owes it afresh from the release on,
    /// judged from captures of its own with every try, since the flight may have moved or resized the
    /// window.
    #[test]
    fn a_press_cancels_every_refresh_and_the_release_asks_again_with_a_fresh_first_capture_and_tries()
     {
        let now = Instant::now();
        let mut refreshes = Refreshes::<u32>::default();
        refreshes.ask(vec![target(1), target(2)], now, now);
        round(&mut refreshes, now, vec![on(1, taken(7)), on(2, taken(7))], 0);
        refreshes.cancel();
        assert_eq!(refreshes.wake(), None);
        assert!(
            refreshes.next(now + CHANGE_EVERY, unpictured).is_none(),
            "cancelled"
        );
        assert_eq!(
            refreshes.owed().collect::<Vec<_>>(),
            vec![wid(1), wid(2)],
            "still owed"
        );

        let release = now + Duration::from_millis(400);
        refreshes.release(release);
        assert_eq!(refreshes.wake(), Some(release));
        refreshes.forget(wid(2));
        let mut at = release;
        for look in 0..CHANGE_TRIES - 1 {
            let band = 7 + look as u8;
            assert_eq!(
                round(&mut refreshes, at, vec![on(1, taken(band))], look),
                nothing(),
                "7 is its first capture now"
            );
            at += CHANGE_EVERY;
        }
        let last = CHANGE_TRIES - 1;
        assert_eq!(
            round(&mut refreshes, at, vec![on(1, taken(99))], last),
            settled(&[(1, last)]),
            "every try"
        );
    }

    /// Asked again at a release, a window is done unlooked at when the cache holds a picture of it
    /// taken `REPAINT_SETTLED` after its look turned; one taken sooner, frame zero's at the press
    /// among them, can predate the repaint. Only a refresh asked again is judged so, and only before
    /// its first look: after that its own captures judge it.
    #[test]
    fn a_refresh_asked_again_is_done_only_where_a_picture_taken_repaint_settled_after_its_turn_is_held()
     {
        let asked = Instant::now();
        let release = asked + Duration::from_millis(300);
        let mut refreshes = Refreshes::<u32>::default();
        refreshes.ask(vec![target(1), target(2)], asked, asked);
        refreshes.cancel();
        refreshes.release(release);
        let soon = Duration::from_millis(1);
        let pictured = |window: WindowId| {
            Some(
                asked + REPAINT_SETTLED
                    - if window == wid(1) {
                        Duration::ZERO
                    } else {
                        soon
                    },
            )
        };
        assert_eq!(looked(refreshes.next(release, pictured)), vec![wid(2)]);
        assert_eq!(
            refreshes.owed().collect::<Vec<_>>(),
            vec![wid(2)],
            "window 1 is pictured"
        );
        let seen = [on(2, taken(1))];
        refreshes.seen(release, &seen, captures(&seen, 0), Some(LAPTOP));
        let later = |_: WindowId| Some(release);
        assert_eq!(
            looked(refreshes.next(release + CHANGE_EVERY, later)),
            vec![wid(2)],
            "looked at since"
        );

        refreshes.ask(vec![target(3)], asked, asked);
        assert_eq!(
            looked(refreshes.next(asked, later)),
            vec![wid(3)],
            "never cancelled"
        );
    }

    /// A round the service stopped short of a window, for a press on its way, says nothing of it: no
    /// try is spent and nothing is owed ScreenCaptureKit's capture. The press's hold cancels it next.
    #[test]
    fn a_window_cut_from_a_round_spends_nothing_and_is_owed_nowhere_else() {
        let now = Instant::now();
        let mut refreshes = Refreshes::<u32>::default();
        refreshes.ask(vec![target(1), target(2)], now, now);
        assert_eq!(looked(refreshes.next(now, unpictured)), vec![wid(1), wid(2)]);
        let seen = [on(1, taken(1))];
        assert_eq!(
            refreshes.seen(now, &seen, captures(&seen, 0), Some(LAPTOP)),
            nothing(),
            "window 2 cut"
        );
        assert_eq!(refreshes.wake(), Some(now), "no try spent of window 2");
        assert_eq!(round(&mut refreshes, now, vec![on(2, taken(1))], 0), nothing());
        let mut at = now;
        for look in 1..CHANGE_TRIES - 1 {
            at += CHANGE_EVERY;
            let band = 10 + look as u8;
            assert_eq!(
                round(
                    &mut refreshes,
                    at,
                    vec![on(1, taken(band)), on(2, taken(band))],
                    look
                ),
                nothing()
            );
        }
        let last = CHANGE_TRIES - 1;
        at += CHANGE_EVERY;
        let refreshed = round(
            &mut refreshes,
            at,
            vec![on(1, taken(99)), on(2, taken(98))],
            last,
        );
        assert_eq!(
            refreshed,
            settled(&[(1, last), (2, last)]),
            "out of tries together: the cut spent none"
        );
    }
}
