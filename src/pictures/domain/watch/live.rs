//! `Watch::Live`: the windows behind the strip, refreshed at a pace once the watch it follows is
//! done, every capture the cache took delivered.

use std::time::{Duration, Instant};

use rini_core::ids::WindowId;

use super::{Accept, Delivered, Looked, Outcome, Round, Tag, WatchId, Why, look, news};
use crate::pictures::domain::target::PictureTarget;

pub(super) struct Live {
    tag: Tag,
    targets: Vec<PictureTarget>,
    every: Duration,
    after: Option<WatchId>,
    initial: Option<Instant>,
    last: Option<Instant>,
}

impl Live {
    pub(super) fn start(
        tag: Tag,
        targets: Vec<PictureTarget>,
        every: Duration,
        after: Option<WatchId>,
        initial: Option<Instant>,
    ) -> Self {
        Self {
            tag,
            targets,
            every,
            after,
            initial,
            last: None,
        }
    }

    /// Carries on from `old`: the background keeps its pace.
    pub(super) fn carry_on(&mut self, old: Live) { self.last = old.last }

    pub(super) fn forget(&mut self, window: WindowId) {
        self.targets.retain(|target| target.window != window);
    }

    /// The watch it waits for.
    pub(super) fn after(&self) -> Option<WatchId> { self.after }

    /// When it is next due, `after_done` saying whether the watch it waits for is done: `every` after
    /// its last refresh once that is, and at `initial` regardless; never with nothing to refresh.
    pub(super) fn due(&self, now: Instant, after_done: bool) -> Option<Instant> {
        if self.targets.is_empty() {
            return None;
        }
        let regular = after_done.then(|| self.last.map_or(now, |last| last + self.every));
        match (regular, self.initial) {
            (Some(regular), Some(initial)) => Some(regular.min(initial)),
            (regular, initial) => regular.or(initial),
        }
    }

    /// Every window of it, in a set of its own with nothing behind.
    pub(super) fn due_looks(&self, id: WatchId, round: &mut Round) {
        for target in &self.targets {
            round.add(id, &[], look(*target, Accept::OnDisplay, false));
        }
    }

    /// A look at `window` refreshed it at `now`: delivered when the cache took its capture, whatever
    /// its band print.
    pub(super) fn on_seen(
        &mut self,
        window: WindowId,
        now: Instant,
        looked: &Looked,
        prints: &mut Delivered,
        out: &mut Outcome,
    ) {
        self.last = Some(now);
        self.initial = None;
        if let Some((frame, taken)) = looked.taken(window, Accept::OnDisplay) {
            news(prints, self.tag.client, window, &taken.band);
            out.deliver(self.tag, Why::Background, window, frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::super::*;
    use super::*;

    /// Windows behind the strip refresh every 16ms, every capture the cache took delivered whatever its
    /// band print: content changes below the band, which a band print would never see.
    #[test]
    fn the_background_refreshes_every_sixteen_milliseconds_and_delivers_each_capture_the_cache_took()
     {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[], &[(4, LEFT)]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        watches.watch(WatchId(1), live(&[4], None, None), start);
        let (sets, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        assert!(sets[0].behind.is_empty());
        assert_eq!(
            delivered(&outcome),
            vec![(Why::Background, vec![wid(4)])],
            "unchanged, delivered"
        );
        assert!(watches.next(start + ms(15)).is_none());
        let (_, outcome) = round_of(
            &mut watches,
            start + ms(16),
            |_| Some(LEFT),
            |_| Capture::Refused,
        )
        .unwrap();
        assert!(outcome.deliveries.is_empty(), "refused: not delivered");
        let (_, outcome) =
            round(&mut watches, start + ms(32), |_| Some(LEFT), |_| Some(1)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Background, vec![wid(4)])]);
    }

    /// The background waits for what the transition waits on: a window still landing, and a change
    /// capture due within 8ms.
    #[test]
    fn the_background_never_runs_ahead_of_a_landing_or_a_change_capture() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        watches.watch(WatchId(2), live(&[4], None, None), start);
        let (sets, _) = round(
            &mut watches,
            start,
            |w| (w == wid(4)).then_some(RIGHT),
            |_| Some(1),
        )
        .unwrap();
        assert_eq!(
            looked(&sets),
            vec![vec![wid(1)]],
            "the landing, not the background"
        );
        assert!(
            watches.next(start).is_none(),
            "the background waits while it lands"
        );
        round(
            &mut watches,
            start + ms(1),
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |_| Some(1),
        )
        .unwrap();
        assert_eq!(
            looked(&watches.next(start + ms(1)).unwrap()),
            vec![vec![wid(4)]],
            "landed: now"
        );

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        watches.watch(WatchId(2), live(&[4], None, None), start);
        round(
            &mut watches,
            start,
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |_| Some(1),
        )
        .unwrap();
        assert!(watches.next(start + ms(7)).is_none(), "the next try is 8ms off");
        assert_eq!(
            looked(&watches.next(start + ms(6)).unwrap()),
            vec![vec![wid(4)]],
            "9ms off"
        );
    }

    /// The background waits for the focus target's change watch to be done; once at `initial`
    /// regardless, for a window frame zero left out.
    #[test]
    fn the_background_waits_for_its_change_watch_but_once_at_initial() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        watches.watch(WatchId(1), change(1, LEFT), start);
        watches.watch(
            WatchId(2),
            live(&[4], Some(WatchId(1)), Some(start + ms(30))),
            start,
        );
        assert!(watches.next(start).is_none(), "the focus target first");
        assert_eq!(watches.wake(start), Some(start + ms(30)));
        let (sets, outcome) =
            round(&mut watches, start + ms(30), |_| Some(RIGHT), |_| Some(1)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(4)]]);
        assert_eq!(delivered(&outcome), vec![(Why::Background, vec![wid(4)])]);
        assert!(watches.next(start + ms(100)).is_none(), "and then waits again");

        watches.hint(Hint::Focused(wid(1)), start + ms(100));
        let (_, outcome) = round(
            &mut watches,
            start + ms(100),
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |_| Some(2),
        )
        .unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(1)])]);
        assert_eq!(
            looked(&watches.next(start + ms(100)).unwrap()),
            vec![vec![wid(4)]],
            "after the change"
        );
    }

    /// The background waits for the target's change, not for the window losing focus, which may go on
    /// alone for its six tries.
    #[test]
    fn the_background_waits_for_the_targets_change_and_not_the_old_windows() {
        let start = Instant::now();
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT)], &[]));
        round(&mut watches, start, at, |_| Some(1)).unwrap();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        watches.watch(WatchId(2), live(&[4], Some(WatchId(1)), None), start);
        let (sets, _) = round(&mut watches, start, at, |_| Some(1)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(1), wid(2)]]);
        let quiet = start + ms(9);
        assert!(watches.next(quiet).is_none(), "the target's change to come");
        let next = start + CHANGE_EVERY;
        let (_, outcome) =
            round(&mut watches, next, at, |w| Some(if w == wid(2) { 2 } else { 1 })).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(2)])]);
        assert_eq!(
            looked(&watches.next(next).unwrap()),
            vec![vec![wid(4)]],
            "the old window still watched"
        );
    }

    /// The target's change in, the windows behind the strip keep their pace beside a window losing
    /// focus never where it ends, polled for every 1ms.
    #[test]
    fn the_background_keeps_its_pace_beside_an_old_window_never_where_it_ends() {
        let start = Instant::now();
        let short = rect(14.0, 32.0, 700.0, 1081.0);
        let at = |w: WindowId| Some(if w == wid(1) { short } else { RIGHT });
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
        watches.watch(WatchId(1), changing(LEFT), start);
        watches.watch(WatchId(2), live(&[4], Some(WatchId(1)), None), start);
        let (_, outcome) = round(&mut watches, start, at, |w| {
            Some(if w == wid(2) { 2 } else { 1 })
        })
        .unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(2)])]);
        let (mut now, mut background, mut polls) = (start, Vec::new(), 0);
        while let Some(wake) = watches.wake(now)
            && wake < start + ms(50)
        {
            now = wake;
            let (sets, _) = round(&mut watches, now, at, |_| Some(1)).unwrap();
            match looked(&sets).concat().as_slice() {
                [window] if *window == wid(4) => background.push(now - start),
                _ => polls += 1,
            }
        }
        assert_eq!(background, vec![
            ms(0),
            LIVE_EVERY,
            LIVE_EVERY * 2,
            LIVE_EVERY * 3
        ]);
        assert!(polls >= 45, "the old window still polled for: {polls}");
    }

    /// A stopped change watch stops holding the background up.
    #[test]
    fn the_background_follows_a_change_watch_that_was_stopped() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(WatchId(1), change(1, LEFT), start);
        watches.watch(WatchId(2), live(&[4], Some(WatchId(1)), None), start);
        assert!(watches.next(start).is_none());
        watches.stop(WatchId(1));
        assert!(watches.next(start).is_some());
    }
}
