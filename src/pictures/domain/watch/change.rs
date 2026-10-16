//! `Watch::Change`: armed by a focus report, the window gaining focus and the window losing it
//! captured where each ends until its band print turns, each end with its own baseline and tries.

use std::time::{Duration, Instant};

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::ScreenRect;

use super::{
    Accept, Capture, Delivered, END_SLACK, LANDING_POLL, Looked, Outcome, Round, Tag, WatchId, Why,
    look, news,
};
use crate::pictures::domain::print::print_changed;
use crate::pictures::domain::target::PictureTarget;

pub(super) struct Change {
    tag: Tag,
    target: Turning,
    losing: Option<Turning>,
    behind: Vec<WindowServerId>,
    every: Duration,
    armed: bool,
    /// When its ends not done are next looked at, together.
    next: Instant,
    /// When its ends the last look missed are looked for again, before `next`.
    poll: Option<Instant>,
}

/// How a `Change` watch was looked at: at a poll for the ends the last look missed, or at its round,
/// which `captured` either end or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Visit {
    Poll,
    Round { captured: bool },
}

/// One end of a `Change` watch.
pub(super) struct Turning {
    target: PictureTarget,
    /// The band print a change is judged from: what the client was delivered of the window as the
    /// watch started; with none then, what it was delivered by the first capture, else that capture.
    baseline: Option<Vec<u8>>,
    end: ScreenRect,
    left: u32,
    state: EndState,
}

/// Where one end of a `Change` watch is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EndState {
    /// Watched, the last look found it where it ends or it has not been looked at.
    Looking,
    /// Watched, the last look found it not where it ends.
    Missed,
    /// Done: its face changed.
    Turned,
    /// Done with no change: its tries ran out, the cache refused its capture, it was handed to
    /// ScreenCaptureKit, or, the window losing focus, it was found clipped. The window losing focus
    /// ended so is lost.
    Ended,
}

/// What befalls an end of a `Change` watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EndEdge {
    /// A look at it begins.
    Looked,
    /// The look found it not where it ends.
    Missed,
    /// Its face changed.
    Turned,
    /// A try spent with no change, `left` tries left.
    Spent { left: u32 },
    /// Its capture refused, or it found cut by the display's edge.
    Ended,
}

impl EndState {
    /// A change of face turns any end, and a turned end stays turned. A spent try ends one with no
    /// tries left and watches it again otherwise; a look clears a miss and a miss marks only an end
    /// still watched.
    pub(super) fn next(self, edge: EndEdge) -> EndState {
        use EndState::*;
        match (self, edge) {
            (_, EndEdge::Turned) => Turned,
            (Turned, _) => Turned,
            (Missed, EndEdge::Looked) => Looking,
            (Looking | Missed, EndEdge::Missed) => Missed,
            (Ended, EndEdge::Looked | EndEdge::Missed) | (Looking, EndEdge::Looked) => self,
            (_, EndEdge::Spent { left: 0 }) | (_, EndEdge::Ended) => Ended,
            (_, EndEdge::Spent { .. }) => Looking,
        }
    }

    pub(super) fn done(self) -> bool { matches!(self, EndState::Turned | EndState::Ended) }
}

impl Turning {
    pub(super) fn new(
        target: PictureTarget,
        end: ScreenRect,
        baseline: Option<Vec<u8>>,
        tries: u32,
    ) -> Self {
        Self {
            target,
            baseline,
            end,
            left: tries,
            state: if tries == 0 {
                EndState::Ended
            } else {
                EndState::Looking
            },
        }
    }

    /// A try spent: with none left it is done.
    fn spend(&mut self) {
        self.left = self.left.saturating_sub(1);
        self.state = self.state.next(EndEdge::Spent { left: self.left });
    }

    /// Carries on from `old`, the same window's, while its end keeps its size: a band print is drawn
    /// from the window's whole width, so one taken at another size never matches, and the cache
    /// refuses a capture only at the size of what it holds. Whether it did.
    fn carry_on(&mut self, old: Turning) -> bool {
        let kept = self.target.window == old.target.window && self.end.size == old.end.size;
        if kept {
            self.baseline = old.baseline;
            self.left = old.left.min(self.left);
            self.state = old.state;
        }
        kept
    }

    /// A capture of it the cache took at `frame`, with band print `band`: done, delivered as its
    /// focus change, once its face differs from its baseline; with nothing delivered to judge by,
    /// delivered as the window where it ends and judged from; else a try spent.
    fn judge(
        &mut self,
        tag: Tag,
        frame: ScreenRect,
        band: &[u8],
        prints: &mut Delivered,
        out: &mut Outcome,
    ) {
        let (client, window) = (tag.client, self.target.window);
        // A landing may have delivered the changed face first: the change is judged from what was
        // delivered before it, not from the latest delivery.
        let judged = self.baseline.take().or_else(|| prints.get(&(client, window)).cloned());
        news(prints, client, window, band);
        if let Some(baseline) = judged.as_deref()
            && print_changed(Some(baseline), band)
        {
            self.state = self.state.next(EndEdge::Turned);
            out.deliver(tag, Why::FocusChanged, window, frame);
        } else {
            if judged.is_none() {
                out.deliver(tag, Why::Landed, window, frame);
            }
            self.baseline = Some(judged.unwrap_or_else(|| band.to_vec()));
            self.spend();
        }
    }
}

impl Change {
    pub(super) fn start(
        tag: Tag,
        target: Turning,
        losing: Option<Turning>,
        behind: Vec<WindowServerId>,
        every: Duration,
        armed: bool,
        now: Instant,
    ) -> Self {
        Self {
            tag,
            target,
            losing,
            behind,
            every,
            armed,
            next: now,
            poll: None,
        }
    }

    /// Its ends, the window losing focus first: its face turns first on screen.
    fn ends(&self) -> impl Iterator<Item = &Turning> { self.losing.iter().chain([&self.target]) }

    fn end_of(&mut self, window: WindowId) -> Option<&mut Turning> {
        self.losing
            .iter_mut()
            .chain([&mut self.target])
            .find(|end| end.target.window == window)
    }

    fn done(&self) -> bool { self.ends().all(|end| end.state.done()) }

    /// When it is next looked at: its round, or a poll before it while an end the last look missed is
    /// still watched. A poll left by an end forgotten or retargeted since would hand out nothing.
    fn when(&self) -> Instant {
        match self.poll {
            Some(poll) if self.ends().any(|end| end.state == EndState::Missed) => {
                poll.min(self.next)
            }
            _ => self.next,
        }
    }

    /// When it is next looked at, while it is armed and not done.
    pub(super) fn wake(&self) -> Option<Instant> {
        (self.armed && !self.done()).then(|| self.when())
    }

    /// When its target is next looked at, while it is not done.
    fn target_wake(&self) -> Option<Instant> {
        let target = &self.target;
        (!target.state.done()).then(|| match self.poll {
            Some(poll) if target.state == EndState::Missed => poll.min(self.next),
            _ => self.next,
        })
    }

    /// Whether it is armed and its target is next looked at by `at`.
    pub(super) fn target_due_by(&self, at: Instant) -> bool {
        self.armed && self.target_wake().is_some_and(|wake| wake <= at)
    }

    /// Its target delivered its change, ran out of tries, or was handed over.
    pub(super) fn target_done(&self) -> bool { self.target.state.done() }

    /// `window` was reported focused at `now`: when it is the target, the watch is armed from now.
    pub(super) fn arm(&mut self, window: WindowId, now: Instant) {
        if self.target.target.window == window && !self.armed {
            self.armed = true;
            self.next = now;
        }
    }

    /// `window` is gone: the window losing focus stops being watched, and the watch is over when it
    /// was the target. Whether it goes on.
    pub(super) fn forget(&mut self, window: WindowId) -> bool {
        self.losing = self.losing.take().filter(|end| end.target.window != window);
        self.target.target.window != window
    }

    /// Carries on from `old` while it watches the same target: a focus report seen stays seen, and
    /// each end keeps its baseline, tries and finish while it keeps its size. The window losing focus
    /// given up on, to report lost again under a new tag.
    pub(super) fn carry_on(&mut self, old: Change) -> Option<(Tag, WindowId)> {
        if self.target.target.window != old.target.target.window {
            return None;
        }
        self.armed = old.armed;
        if self.target.carry_on(old.target) {
            self.next = old.next;
            self.poll = old.poll;
        }
        let retagged = self.tag != old.tag;
        let losing = self.losing.as_mut()?;
        (losing.carry_on(old.losing?) && losing.state == EndState::Ended && retagged)
            .then_some((self.tag, losing.target.window))
    }

    /// Its ends due a look now: at its round every end not done, and at a poll before it the ends the
    /// last look missed.
    pub(super) fn due_looks(&self, id: WatchId, now: Instant, round: &mut Round) {
        if !self.wake().is_some_and(|at| at <= now) {
            return;
        }
        let full = self.next <= now;
        if full {
            round.plan.rounds.push(id);
        }
        for end in self.ends().filter(|end| match end.state {
            EndState::Looking => full,
            EndState::Missed => true,
            EndState::Turned | EndState::Ended => false,
        }) {
            let accept = Accept::Near { end: end.end, slack: END_SLACK };
            round.add(id, &self.behind, look(end.target, accept, false));
        }
    }

    /// What one look at `window`, one of its ends, decides: judged where the cache took its capture,
    /// done where it refused it or found it there but cut by the display's edge, the target's owed a
    /// capture elsewhere; else missed. The window losing focus done with no change is lost. Whether
    /// the look found it there, or `None` when it is not an end of this watch.
    pub(super) fn on_seen(
        &mut self,
        window: WindowId,
        looked: &Looked,
        prints: &mut Delivered,
        out: &mut Outcome,
    ) -> Option<bool> {
        let tag = self.tag;
        let losing = self.losing.as_ref().is_some_and(|end| end.target.window == window);
        let end = self.end_of(window)?;
        end.state = end.state.next(EndEdge::Looked);
        let mut found = true;
        match looked.at(window, Accept::Near { end: end.end, slack: END_SLACK }) {
            Some((frame, Capture::Taken(taken))) => end.judge(tag, frame, &taken.band, prints, out),
            // Every later capture at this size is refused as well.
            Some((_, Capture::Refused)) => end.state = end.state.next(EndEdge::Ended),
            Some((_, Capture::Missed)) | None => match looked.cut(window, end.end, END_SLACK) {
                Some(frame) => {
                    end.state = end.state.next(EndEdge::Ended);
                    if !losing {
                        out.clipped.push((tag, end.target, frame));
                    }
                }
                None => {
                    end.state = end.state.next(EndEdge::Missed);
                    found = false;
                }
            },
        }
        if losing && end.state == EndState::Ended {
            out.lost.push((tag, window));
        }
        Some(found)
    }

    /// Looked at again together `every` after a round that captured either end, and every
    /// `LANDING_POLL` for an end not there yet: a poll leaves the round where it was, however long
    /// its look took.
    pub(super) fn reschedule(&mut self, now: Instant, visit: Visit) {
        if let Visit::Round { captured } = visit {
            self.next = now + if captured { self.every } else { LANDING_POLL };
        }
        let missed = self.ends().any(|end| end.state == EndState::Missed);
        self.poll = missed.then_some(now + LANDING_POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::super::*;
    use super::*;

    #[test]
    fn an_end_moves_by_what_befalls_it() {
        use EndState::{Ended, Looking, Missed, Turned};
        let edges = [
            EndEdge::Looked,
            EndEdge::Missed,
            EndEdge::Turned,
            EndEdge::Spent { left: 0 },
            EndEdge::Spent { left: 2 },
            EndEdge::Ended,
        ];
        for (state, after) in [
            (Looking, [Looking, Missed, Turned, Ended, Looking, Ended]),
            (Missed, [Looking, Missed, Turned, Ended, Looking, Ended]),
            (Turned, [Turned; 6]),
            (Ended, [Ended, Ended, Turned, Ended, Looking, Ended]),
        ] {
            for (edge, after) in edges.into_iter().zip(after) {
                assert_eq!(state.next(edge), after, "{state:?} on {edge:?}");
            }
        }
        assert_eq!([Looking, Missed, Turned, Ended].map(EndState::done), [
            false, false, true, true
        ]);
    }

    /// The focus target is captured only once its focus is reported; from then every 15ms, six times
    /// at most, stopping at the first change of its band print.
    #[test]
    fn a_change_watch_captures_after_the_focus_report_until_the_face_changes() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        watches.watch(WatchId(1), change(1, LEFT), start);
        assert!(watches.next(start + ms(100)).is_none(), "no focus report");

        watches.hint(Hint::Focused(wid(1)), start + ms(100));
        let mut at = start + ms(100);
        for _ in 0..2 {
            let (_, outcome) = round(&mut watches, at, |_| Some(LEFT), |_| Some(1)).unwrap();
            assert!(outcome.deliveries.is_empty(), "unchanged");
            assert!(watches.next(at + ms(14)).is_none(), "every 15ms");
            at += CHANGE_EVERY;
        }
        let (_, outcome) = round(&mut watches, at, |_| Some(LEFT), |_| Some(2)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(1)])]);
        assert!(
            watches.next(at + ms(100)).is_none(),
            "stopped at the first change"
        );
    }

    /// With nothing delivered of the window to judge a change by, its first capture is delivered as
    /// the window where it ends, and the change judged from it; six tries in all.
    #[test]
    fn a_change_watch_with_nothing_to_judge_by_starts_from_its_first_capture_and_gives_up_after_six()
     {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        let mut at = start;
        let mut outcomes = Vec::new();
        while let Some((_, outcome)) = round(&mut watches, at, |_| Some(LEFT), |_| Some(1)) {
            outcomes.push(outcome);
            at += CHANGE_EVERY;
        }
        assert_eq!(outcomes.len(), CHANGE_TRIES as usize);
        assert_eq!(
            delivered(&outcomes[0]),
            vec![(Why::Landed, vec![wid(1)])],
            "never a focus change"
        );
        assert!(outcomes[1..].iter().all(|outcome| outcome.deliveries.is_empty()));
    }

    /// A window that was focused before it landed: its landing, looked at first in the same round,
    /// delivers the focused face, and its change watch still sees it changed from frame zero's.
    #[test]
    fn a_change_watch_sees_the_face_a_landing_delivered_first() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, RIGHT)], &[]));
        round(&mut watches, start, |_| Some(RIGHT), |_| Some(1)).unwrap();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        watches.watch(WatchId(2), change(1, LEFT), start);
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(2)).unwrap();
        assert_eq!(delivered(&outcome), vec![
            (Why::Landed, vec![wid(1)]),
            (Why::FocusChanged, vec![wid(1)])
        ]);

        // A parked window, nothing delivered as the watch started: judged from what its landing
        // delivered by the watch's first capture.
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        watches.watch(WatchId(2), change(1, LEFT), start);
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        assert_eq!(
            delivered(&outcome),
            vec![(Why::Landed, vec![wid(1)])],
            "one landing, delivered once"
        );
        let (_, outcome) =
            round(&mut watches, start + CHANGE_EVERY, |_| Some(LEFT), |_| Some(2)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(1)])]);
    }

    /// The focus report often comes before the watch. Remembered for a second, it arms a watch
    /// created after it; older, it does not.
    #[test]
    fn a_focus_reported_just_before_the_watch_arms_it() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start + ms(900));
        assert!(watches.next(start + ms(900)).is_some(), "900ms after the report");

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start + ms(1100));
        assert!(
            watches.next(start + ms(1100)).is_none(),
            "a report over a second old"
        );

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        assert!(watches.next(start).is_none(), "another window's");
    }

    /// A change capture the cache refuses finishes the watch, delivering nothing: every later capture
    /// of the window at that size is refused as well. The windows behind the strip that wait on it refresh
    /// at once, and nothing judges the window's face again, a retarget of it included.
    #[test]
    fn a_change_capture_the_cache_refuses_finishes_the_watch() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        watches.watch(WatchId(2), live(&[4], Some(WatchId(1)), None), start);
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let (_, outcome) = round(&mut watches, start, at, |_| Some(1)).unwrap();
        assert!(outcome.deliveries.is_empty(), "unchanged: a try spent");

        let next = start + CHANGE_EVERY;
        let (sets, outcome) = round_of(&mut watches, next, at, |_| Capture::Refused).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(1)]], "the background waits for it");
        assert!(outcome.deliveries.is_empty());
        assert_eq!(
            looked(&watches.next(next).unwrap()),
            vec![vec![wid(4)]],
            "done: the background now"
        );
        watches.retarget(WatchId(1), change(1, LEFT), next);
        let later = next + CHANGE_EVERY;
        let (sets, _) = round(&mut watches, later, at, |_| Some(2)).unwrap();
        assert_eq!(
            looked(&sets),
            vec![vec![wid(4)]],
            "its face is never captured again"
        );

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        let (_, outcome) =
            round_of(&mut watches, start, |_| Some(LEFT), |_| Capture::Refused).unwrap();
        assert!(outcome.deliveries.is_empty(), "not delivered as landed either");
        assert!(watches.next(start + CHANGE_EVERY).is_none());
    }

    /// A change watch retargeted to an end of another size starts afresh but for its focus report: a
    /// refusal or the tries spent at one size say nothing of another. At the same size it carries on.
    #[test]
    fn a_change_watch_retargeted_to_another_size_starts_afresh_but_for_its_focus_report() {
        let start = Instant::now();
        let wider = rect(4.0, 32.0, 1200.0, 1081.0);
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        round_of(&mut watches, start, |_| Some(LEFT), |_| Capture::Refused).unwrap();
        let later = start + ms(2000);
        watches.retarget(WatchId(1), change(1, rect(14.0, 32.0, 859.0, 1081.0)), later);
        assert!(watches.next(later).is_none(), "the same size: still done");
        watches.retarget(WatchId(1), change(1, wider), later);
        assert_eq!(
            looked(&watches.next(later).unwrap()),
            vec![vec![wid(1)]],
            "another size: captured again, armed long after the report"
        );

        let tries = |watches: &mut Watches, mut at: Instant, end: ScreenRect| {
            let mut tries = 0;
            while round(watches, at, |_| Some(end), |_| Some(1)).is_some() {
                tries += 1;
                at += CHANGE_EVERY;
            }
            tries
        };
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        let at = start + CHANGE_EVERY * (CHANGE_TRIES - 1);
        let mut now = start;
        while now < at {
            round(&mut watches, now, |_| Some(LEFT), |_| Some(1)).unwrap();
            now += CHANGE_EVERY;
        }
        watches.retarget(WatchId(1), change(1, LEFT), at);
        assert_eq!(
            tries(&mut watches, at, LEFT),
            1,
            "the same size: the one try left"
        );
        watches.retarget(WatchId(1), change(1, wider), at);
        assert_eq!(
            tries(&mut watches, at, wider),
            CHANGE_TRIES,
            "another size: every try again"
        );
    }

    /// Until the window is where it ends, a change watch only watches it land: no try is spent.
    #[test]
    fn a_change_watch_spends_no_try_before_its_window_is_there() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        let mut at = start;
        for _ in 0..20 {
            round(
                &mut watches,
                at,
                |_| PARKED.intersection(&LAPTOP).map(|on| on.union(&rect(900.0, 32.0, 1.0, 1.0))),
                |_| Some(1),
            );
            at += LANDING_POLL;
        }
        let mut tries = 0;
        while round(&mut watches, at, |_| Some(LEFT), |_| Some(1)).is_some() {
            tries += 1;
            at += CHANGE_EVERY;
        }
        assert_eq!(tries, CHANGE_TRIES);
    }

    /// A focus target that ends cut by the display's edge cannot be pictured whole by SkyLight: it is
    /// owed one ScreenCaptureKit capture instead, and the watch is done.
    #[test]
    fn a_change_watch_on_a_clipped_end_asks_for_one_capture_elsewhere() {
        let start = Instant::now();
        let end = rect(1300.0, 32.0, 859.0, 1081.0);
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, end), start);
        let (_, outcome) = round(&mut watches, start, |_| Some(end), |_| Some(1)).unwrap();
        assert!(outcome.deliveries.is_empty());
        assert_eq!(outcome.clipped, vec![(tag(1), target(1), end)]);
        assert!(watches.next(start + ms(100)).is_none(), "once");
    }

    /// The window losing focus is captured in the same rounds as the target, in one set, listed first,
    /// each judged from its own baseline and done at its own change; a report of its own focus arms
    /// nothing.
    #[test]
    fn a_change_watch_captures_the_window_losing_focus_beside_the_target_each_done_at_its_own_change()
     {
        let start = Instant::now();
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let started = |watches: &mut Watches| {
            watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT)], &[]));
            round(watches, start, at, |w| Some(if w == wid(1) { 1 } else { 5 })).unwrap();
            watches.watch(WatchId(1), changing(LEFT), start);
        };
        let mut watches = Watches::default();
        started(&mut watches);
        watches.hint(Hint::Focused(wid(1)), start);
        assert!(
            watches.next(start).is_none(),
            "armed by the target's report only"
        );
        watches.hint(Hint::Focused(wid(2)), start);
        let (sets, outcome) = round(&mut watches, start, at, |w| {
            Some(if w == wid(1) { 1 } else { 5 })
        })
        .unwrap();
        assert_eq!(
            looked(&sets),
            vec![vec![wid(1), wid(2)]],
            "one set, the old window first"
        );
        assert!(outcome.deliveries.is_empty());
        let next = start + CHANGE_EVERY;
        let (_, outcome) =
            round(&mut watches, next, at, |w| Some(if w == wid(1) { 2 } else { 5 })).unwrap();
        assert_eq!(
            delivered(&outcome),
            vec![(Why::FocusChanged, vec![wid(1)])],
            "from its own frame zero's"
        );
        let next = next + CHANGE_EVERY;
        let (sets, outcome) =
            round(&mut watches, next, at, |w| Some(if w == wid(1) { 2 } else { 6 })).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(2)]], "done, it drops out");
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(2)])]);
        assert!(watches.next(next + ms(100)).is_none(), "done when both are");

        let mut watches = Watches::default();
        started(&mut watches);
        watches.hint(Hint::Focused(wid(2)), start);
        let (_, outcome) = round(&mut watches, start, at, |w| {
            Some(if w == wid(1) { 2 } else { 6 })
        })
        .unwrap();
        assert_eq!(
            delivered(&outcome),
            vec![(Why::FocusChanged, vec![wid(1), wid(2)])],
            "in one round, the old window first"
        );
    }

    /// Each end spends its own tries, only on its own captures. While one is not there yet the other is
    /// looked at every 15ms, not polled with it every 1ms. The old window, out of tries with no change,
    /// is reported lost; the target never is.
    #[test]
    fn a_change_watch_spends_each_ends_tries_on_its_own_captures() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        let landed_at = start + CHANGE_EVERY * 3;
        let mut at = start;
        let mut rounds = Vec::new();
        let mut lost = Vec::new();
        while let Some((sets, outcome)) = round(
            &mut watches,
            at,
            |w| {
                Some(if w == wid(2) {
                    RIGHT
                } else if at < landed_at {
                    PARKED
                } else {
                    LEFT
                })
            },
            |_| Some(1),
        ) {
            rounds.push(looked(&sets).concat());
            lost.extend(outcome.lost);
            at += CHANGE_EVERY;
        }
        let both = vec![wid(1), wid(2)];
        assert_eq!(rounds.len(), 3 + CHANGE_TRIES as usize, "{rounds:?}");
        assert!(
            rounds[..CHANGE_TRIES as usize].iter().all(|looked| *looked == both),
            "together, 15ms apart"
        );
        assert!(
            rounds[CHANGE_TRIES as usize..].iter().all(|looked| *looked == vec![wid(1)]),
            "the old window's own six"
        );
        assert_eq!(lost, vec![(tag(1), wid(1))], "once, as its last try is spent");
    }

    /// An end not there yet is looked for every 1ms alone, while the end captured waits for the
    /// round 15ms after its capture, spending one try a round; once there, it joins that round.
    #[test]
    fn a_change_watch_polls_an_end_not_there_yet_alone_between_its_rounds() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        let arrives = start + ms(5);
        let short = rect(700.0, 32.0, 859.0, 1081.0);
        let mut at = start;
        let mut rounds = Vec::new();
        while let Some(wake) = watches.wake(at) {
            at = wake;
            let (sets, _) = round(
                &mut watches,
                at,
                |w| {
                    Some(if w == wid(1) {
                        LEFT
                    } else if at < arrives {
                        short
                    } else {
                        RIGHT
                    })
                },
                |_| Some(1),
            )
            .unwrap();
            rounds.push((at - start, looked(&sets).concat()));
        }
        let (both, target) = (vec![wid(1), wid(2)], vec![wid(2)]);
        assert_eq!(rounds[0], (ms(0), both.clone()));
        assert_eq!(
            rounds[1..=5].iter().map(|(_, w)| w.clone()).collect::<Vec<_>>(),
            vec![target; 5],
            "{rounds:?}"
        );
        assert_eq!(rounds[1].0, LANDING_POLL);
        assert_eq!(
            rounds[6],
            (CHANGE_EVERY, both.clone()),
            "there, it joins the round"
        );
        let old = rounds.iter().filter(|(_, looked)| looked.contains(&wid(1))).count();
        assert_eq!(old, CHANGE_TRIES as usize, "the old window's tries, one a round");
        assert!(rounds[6..].iter().all(|(_, looked)| *looked == both));
    }

    /// A window losing focus that ends cut by the display's edge is done with no capture: the one
    /// ScreenCaptureKit capture a focus change lets through is the target's. Forgotten, it is
    /// watched no more, and the target still is.
    #[test]
    fn a_window_losing_focus_ending_clipped_is_done_with_no_capture_elsewhere() {
        let start = Instant::now();
        let cut = rect(1300.0, 32.0, 859.0, 1081.0);
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(cut), start);
        let at = |w: WindowId| Some(if w == wid(1) { cut } else { RIGHT });
        let (_, outcome) = round(&mut watches, start, at, |_| Some(1)).unwrap();
        assert!(outcome.clipped.is_empty());
        assert_eq!(outcome.lost, vec![(tag(1), wid(1))], "its turn will not be seen");
        let (sets, _) = round(&mut watches, start + CHANGE_EVERY, at, |_| Some(1)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(2)]]);

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let (_, outcome) = round_of(&mut watches, start, at, |w| {
            if w == wid(1) {
                Capture::Refused
            } else {
                taken(1)
            }
        })
        .unwrap();
        assert_eq!(outcome.lost, vec![(tag(1), wid(1))], "refused, nor will it");

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        watches.forget(wid(1));
        assert_eq!(looked(&watches.next(start).unwrap()), vec![vec![wid(2)]]);
    }

    /// A poll lasts only while an end it is for is still missed: with that end forgotten, or
    /// retargeted to another size, the watch wakes at its round and not at a poll gone by.
    #[test]
    fn a_change_watch_with_no_end_missed_left_wakes_at_its_round() {
        let start = Instant::now();
        let short = rect(14.0, 32.0, 700.0, 1081.0);
        let missed = || {
            let mut watches = Watches::default();
            watches.hint(Hint::Focused(wid(2)), start);
            watches.watch(WatchId(1), changing(LEFT), start);
            round(
                &mut watches,
                start,
                |w| Some(if w == wid(1) { short } else { RIGHT }),
                |_| Some(1),
            )
            .unwrap();
            assert_eq!(
                watches.wake(start),
                Some(start + LANDING_POLL),
                "the old window polled for"
            );
            watches
        };
        let later = start + LANDING_POLL * 3;

        let mut watches = missed();
        watches.forget(wid(1));
        assert!(watches.next(later).is_none());
        assert_eq!(watches.wake(later), Some(start + CHANGE_EVERY), "forgotten");

        let mut watches = missed();
        watches.retarget(WatchId(1), changing(rect(4.0, 32.0, 860.0, 1081.0)), later);
        assert!(watches.next(later).is_none());
        assert_eq!(
            watches.wake(later),
            Some(start + CHANGE_EVERY),
            "at another size"
        );
    }

    /// A window losing focus given up on is reported lost again when a retarget carries it on under a
    /// new tag: the client may have dropped the report under the tag before. Under the same tag, or
    /// once its face changed, it is not.
    #[test]
    fn a_window_losing_focus_given_up_on_is_reported_lost_again_under_a_retargets_new_tag() {
        let start = Instant::now();
        let cut = rect(1300.0, 32.0, 859.0, 1081.0);
        let under = |generation: u64, losing: ScreenRect| match changing(losing) {
            Watch::Change {
                window,
                end,
                losing,
                behind,
                every,
                tries,
                ..
            } => Watch::Change {
                tag: tag(generation),
                window,
                end,
                losing,
                behind,
                every,
                tries,
            },
            _ => unreachable!(),
        };
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), under(1, cut), start);
        let at = |w: WindowId| Some(if w == wid(1) { cut } else { RIGHT });
        let (_, outcome) = round(&mut watches, start, at, |_| Some(1)).unwrap();
        assert_eq!(outcome.lost, vec![(tag(1), wid(1))]);
        let later = start + ms(2);
        watches.retarget(WatchId(1), under(1, cut), later);
        assert!(watches.expire(later).is_empty(), "the same tag");
        watches.retarget(WatchId(1), under(2, cut), later);
        assert_eq!(watches.wake(later), Some(later));
        assert_eq!(watches.expire(later), vec![(tag(2), wid(1))]);
        assert!(watches.expire(later).is_empty(), "once");

        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT)], &[]));
        round(
            &mut watches,
            start,
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |_| Some(1),
        )
        .unwrap();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), under(1, LEFT), start);
        round(
            &mut watches,
            start,
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |w| Some(if w == wid(1) { 2 } else { 1 }),
        )
        .unwrap();
        watches.retarget(WatchId(1), under(2, LEFT), later);
        assert!(watches.expire(later).is_empty(), "its face changed");
    }

    /// A retarget keeps each end's baseline, tries and finish while that end keeps its size.
    #[test]
    fn a_change_watch_retargeted_carries_each_end_on_while_it_keeps_its_size() {
        let start = Instant::now();
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT)], &[]));
        round(&mut watches, start, at, |_| Some(1)).unwrap();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        let (_, outcome) = round(&mut watches, start, at, |w| {
            Some(if w == wid(1) { 2 } else { 1 })
        })
        .unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(1)])]);
        let next = start + CHANGE_EVERY;
        watches.retarget(WatchId(1), changing(rect(14.0, 32.0, 859.0, 1081.0)), next);
        assert_eq!(
            looked(&watches.next(next).unwrap()),
            vec![vec![wid(2)]],
            "the same size: still done"
        );
        let wider = rect(4.0, 32.0, 860.0, 1081.0);
        watches.retarget(WatchId(1), changing(wider), next);
        let at = |w: WindowId| Some(if w == wid(1) { wider } else { RIGHT });
        let (sets, outcome) = round(&mut watches, next, at, |_| Some(2)).unwrap();
        assert_eq!(
            looked(&sets),
            vec![vec![wid(1), wid(2)]],
            "another size: watched afresh"
        );
        assert_eq!(
            delivered(&outcome),
            vec![(Why::FocusChanged, vec![wid(2)])],
            "the target's baseline kept"
        );
    }

    /// A poll looking at an end not there yet leaves the round where it was, even when its look ends
    /// past the round's time: the end captured before is looked at in that round.
    #[test]
    fn a_poll_finishing_past_the_round_leaves_the_round_where_it_was() {
        let start = Instant::now();
        let short = rect(700.0, 32.0, 859.0, 1081.0);
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        round(
            &mut watches,
            start,
            |w| Some(if w == wid(1) { LEFT } else { short }),
            |_| Some(1),
        )
        .unwrap();
        let polled = start + CHANGE_EVERY - Duration::from_micros(400);
        assert_eq!(
            looked(&watches.next(polled).unwrap()),
            vec![vec![wid(2)]],
            "the poll alone"
        );
        let seen = start + CHANGE_EVERY + Duration::from_micros(200);
        watches.seen(
            seen,
            &[Seen {
                window: wid(2),
                frame: Some(RIGHT),
                capture: taken(1),
            }],
            Some(LAPTOP),
        );
        assert_eq!(
            looked(&watches.next(seen).unwrap()),
            vec![vec![wid(1), wid(2)]],
            "the round, both ends"
        );
    }
}
