//! `Watch::Landing`: each window captured once the window server has it where it ends, delivered
//! at once or once its rendering settles, and lost at the deadline; a repainting one there but cut
//! by the display's edge is settled through ScreenCaptureKit (`Outcome::elsewhere`).

use std::time::{Duration, Instant};

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::ScreenRect;

use super::{
    Accept, Capture, CapturedElsewhere, Delivered, LANDING_POLL, Looked, Outcome, Prints, Round,
    Settle, Tag, WatchId, Why, look, news,
};
use crate::pictures::domain::settle::repaint_settled;
use crate::pictures::domain::target::PictureTarget;

/// How often, and how many times at most, a window that has to repaint is captured once it is where
/// it ends: about a second. See "A grow waits for its repaint" in
/// `src/transitions/docs/coordination.md`.
const REPAINT_EVERY: Duration = Duration::from_millis(8);
const REPAINT_TRIES: usize = 125;
/// How many times at most a window that has to repaint, where it ends but cut by the display's edge,
/// is captured through ScreenCaptureKit, one capture after another: about a second at 38-56ms a
/// capture, its listings besides.
const ELSEWHERE_TRIES: usize = 20;

pub(super) struct Landing {
    tag: Tag,
    behind: Vec<WindowServerId>,
    slack: f64,
    settle: Settle,
    until: Instant,
    windows: Vec<Lands>,
    poll: Instant,
}

struct Lands {
    target: PictureTarget,
    end: ScreenRect,
    state: Arrival,
}

#[derive(PartialEq)]
enum Arrival {
    /// Not where it ends yet.
    Pending,
    /// There, and captured until its rendering settles.
    Settling {
        left: usize,
        last: Option<Vec<u8>>,
        next: Instant,
    },
    /// There at `frame` but cut by the display's edge, and captured through ScreenCaptureKit, one
    /// capture owed or out at a time, until its rendering settles.
    Elsewhere {
        left: usize,
        last: Option<Vec<u8>>,
        frame: ScreenRect,
    },
    Arrived,
    Lost,
}

impl Landing {
    pub(super) fn start(
        tag: Tag,
        windows: Vec<(PictureTarget, ScreenRect)>,
        behind: Vec<WindowServerId>,
        slack: f64,
        settle: Settle,
        until: Instant,
        now: Instant,
    ) -> Self {
        Self {
            tag,
            behind,
            slack,
            settle,
            until,
            windows: windows
                .into_iter()
                .map(|(target, end)| Lands {
                    target,
                    end,
                    state: Arrival::Pending,
                })
                .collect(),
            poll: now,
        }
    }

    /// Carries on from `old`: a window that landed where it still ends stays landed, and one settling
    /// there through ScreenCaptureKit goes on settling; one watched to land somewhere new has its
    /// client's print dropped.
    pub(super) fn carry_on(&mut self, mut old: Landing, prints: &mut Delivered) {
        for lands in &mut self.windows {
            let Some(kept) =
                old.windows.iter_mut().find(|o| o.target.window == lands.target.window)
            else {
                continue;
            };
            if kept.end != lands.end {
                prints.remove(&(self.tag.client, lands.target.window));
            } else if matches!(kept.state, Arrival::Arrived | Arrival::Elsewhere { .. }) {
                lands.state = std::mem::replace(&mut kept.state, Arrival::Pending);
            }
        }
    }

    pub(super) fn forget(&mut self, window: WindowId) {
        self.windows.retain(|l| l.target.window != window);
    }

    /// Its windows not there by its deadline, now past, each reported once.
    pub(super) fn expire(&mut self, now: Instant) -> Vec<(Tag, WindowId)> {
        let mut lost = Vec::new();
        if now < self.until {
            return lost;
        }
        for lands in &mut self.windows {
            if lands.state == Arrival::Pending {
                lands.state = Arrival::Lost;
                lost.push((self.tag, lands.target.window));
            }
        }
        lost
    }

    /// Whether a window is still landing or settling where SkyLight sees it whole.
    pub(super) fn waiting(&self) -> bool {
        self.windows
            .iter()
            .any(|l| matches!(l.state, Arrival::Pending | Arrival::Settling { .. }))
    }

    /// Its windows due a look now: one not there yet at its poll, one settling at its next capture.
    pub(super) fn due_looks(&self, id: WatchId, now: Instant, round: &mut Round) {
        for lands in &self.windows {
            let wanted = match &lands.state {
                Arrival::Pending => self.poll <= now,
                Arrival::Settling { next, .. } => *next <= now,
                Arrival::Elsewhere { .. } | Arrival::Arrived | Arrival::Lost => false,
            };
            if wanted {
                let accept = Accept::Near {
                    end: lands.end,
                    slack: self.slack,
                };
                let thumbprint = self.settle == Settle::Repainted;
                round.add(id, &self.behind, look(lands.target, accept, thumbprint));
            }
        }
    }

    /// When a window of it is next looked at or given up on, if any is.
    pub(super) fn wake(&self) -> Option<Instant> {
        self.windows
            .iter()
            .filter_map(|lands| match &lands.state {
                Arrival::Pending => Some(self.poll.min(self.until)),
                Arrival::Settling { next, .. } => Some(*next),
                Arrival::Elsewhere { .. } | Arrival::Arrived | Arrival::Lost => None,
            })
            .min()
    }

    /// What one look at `window` decides: a repainting one there but cut by the display's edge is
    /// handed to ScreenCaptureKit; any other is judged where it ends (`Lands::land`). While a window
    /// is still not there, the next poll is `LANDING_POLL` on.
    pub(super) fn on_seen(
        &mut self,
        window: WindowId,
        now: Instant,
        looked: &Looked,
        prints: &mut Delivered,
        out: &mut Outcome,
    ) {
        let (tag, settle, slack) = (self.tag, self.settle, self.slack);
        if let Some(lands) = self.windows.iter_mut().find(|l| l.target.window == window) {
            match looked.cut(window, lands.end, slack) {
                Some(frame) if settle == Settle::Repainted => {
                    let left = ELSEWHERE_TRIES;
                    lands.state = Arrival::Elsewhere { left, last: None, frame };
                    out.elsewhere.push(lands.target);
                }
                _ => {
                    let there = looked.at(window, Accept::Near { end: lands.end, slack });
                    lands.land(settle, tag, there, now, prints, out);
                }
            }
        }
        if self.windows.iter().any(|l| l.state == Arrival::Pending) {
            self.poll = now + LANDING_POLL;
        }
    }

    /// What the ScreenCaptureKit capture `window` was owed came to, while it settles elsewhere
    /// (`Watches::captured_elsewhere`).
    pub(super) fn captured_elsewhere(
        &mut self,
        window: WindowId,
        capture: &CapturedElsewhere,
        prints: &mut Delivered,
        out: &mut Outcome,
    ) {
        let tag = self.tag;
        let Some(lands) = self.windows.iter_mut().find(|l| l.target.window == window) else {
            return;
        };
        let Arrival::Elsewhere { left, last, frame } = &mut lands.state else {
            return;
        };
        let frame = *frame;
        match capture {
            CapturedElsewhere::Taken(thumb) if repaint_settled(last.as_deref(), thumb) => {
                lands.state = Arrival::Arrived;
                prints.remove(&(tag.client, window));
                out.deliver(tag, Why::Landed, window, frame);
            }
            CapturedElsewhere::Refused => {
                lands.state = Arrival::Lost;
                out.lost.push((tag, window));
            }
            _ if *left <= 1 => {
                lands.state = Arrival::Lost;
                out.lost.push((tag, window));
            }
            capture => {
                *left -= 1;
                *last = match capture {
                    CapturedElsewhere::Taken(thumb) => Some(thumb.clone()),
                    _ => None,
                };
                out.elsewhere.push(lands.target);
            }
        }
    }
}

impl Lands {
    /// What one look at it decides: delivered once there (or once settled there, for a repainting
    /// one); lost when the cache refuses its capture.
    fn land(
        &mut self,
        settle: Settle,
        tag: Tag,
        there: Option<(ScreenRect, &Capture)>,
        now: Instant,
        prints: &mut Delivered,
        out: &mut Outcome,
    ) {
        let window = self.target.window;
        let refused = matches!(there, Some((_, Capture::Refused)));
        let there = there.and_then(|(frame, capture)| match capture {
            Capture::Taken(seen) => Some((frame, seen)),
            Capture::Missed | Capture::Refused => None,
        });
        let tries = match std::mem::replace(&mut self.state, Arrival::Arrived) {
            done @ (Arrival::Elsewhere { .. } | Arrival::Arrived | Arrival::Lost) => {
                self.state = done;
                return;
            }
            // Every later capture at this size is refused as well: what the cache holds is the picture.
            _ if refused => {
                self.state = Arrival::Lost;
                out.lost.push((tag, window));
                return;
            }
            Arrival::Pending if there.is_none() => {
                self.state = Arrival::Pending;
                return;
            }
            Arrival::Pending => (REPAINT_TRIES, None),
            Arrival::Settling { left, last, .. } => (left, last),
        };
        if settle == Settle::AsSoonAsThere {
            if let Some((frame, seen)) = there
                && news(prints, tag.client, window, &seen.band)
            {
                out.deliver(tag, Why::Landed, window, frame);
            }
            return;
        }
        self.repaint(tag, there, tries, now, prints, out);
    }

    /// A repainting window's look where it ends, `left` tries left and `last` the thumbprint of the
    /// capture before: delivered once two in a row match, lost when a repainting one runs out of
    /// tries, else captured again `REPAINT_EVERY` on.
    fn repaint(
        &mut self,
        tag: Tag,
        there: Option<(ScreenRect, &Prints)>,
        (left, last): (usize, Option<Vec<u8>>),
        now: Instant,
        prints: &mut Delivered,
        out: &mut Outcome,
    ) {
        let window = self.target.window;
        let thumb = there.and_then(|(_, seen)| seen.thumb.clone());
        let settled = thumb.as_deref().is_some_and(|thumb| repaint_settled(last.as_deref(), thumb));
        match there {
            Some((frame, seen)) if settled => {
                news(prints, tag.client, window, &seen.band);
                out.deliver(tag, Why::Landed, window, frame);
            }
            _ if left <= 1 => {
                self.state = Arrival::Lost;
                out.lost.push((tag, window));
            }
            _ => {
                self.state = Arrival::Settling {
                    left: left - 1,
                    last: thumb,
                    next: now + REPAINT_EVERY,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::super::*;
    use super::*;

    /// A landing window is looked at every millisecond until it is within 20pt of its end and wholly on
    /// the display, captured once there, and delivered when its band print is news.
    #[test]
    fn a_landing_window_is_captured_once_it_is_where_it_ends() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );

        let (_, outcome) = round(&mut watches, start, |_| Some(PARKED), |_| Some(7)).unwrap();
        assert!(outcome.deliveries.is_empty(), "still parked");
        assert!(watches.next(start).is_none(), "not again before the next poll");
        assert_eq!(watches.wake(start), Some(start + LANDING_POLL));

        let near = rect(
            LEFT.origin.x + 19.0,
            LEFT.origin.y,
            LEFT.size.width + 1.0,
            LEFT.size.height,
        );
        let (_, outcome) = round(&mut watches, start + ms(1), |_| Some(near), |_| Some(7)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
        assert_eq!(
            outcome.deliveries[0].windows[0].1, near,
            "with the frame it was taken at"
        );
        assert!(watches.next(start + ms(50)).is_none(), "landed: no more looks");
        assert!(watches.expire(start + ms(200)).is_empty(), "and never lost");
    }

    /// A landing that shows nothing the client was not delivered already is not delivered again.
    #[test]
    fn a_landing_with_an_unchanged_band_print_is_not_delivered() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(7)).unwrap();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(7)).unwrap();
        assert!(outcome.deliveries.is_empty());
        assert!(watches.next(start + ms(5)).is_none(), "landed all the same");
    }

    /// A window not where it ends by the deadline is reported lost, once.
    #[test]
    fn a_window_not_landed_by_the_deadline_is_lost() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT), (2, RIGHT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        round(
            &mut watches,
            start,
            |w| (w == wid(2)).then_some(RIGHT),
            |_| Some(7),
        )
        .unwrap();
        assert!(watches.expire(start + ms(149)).is_empty());
        assert_eq!(watches.expire(start + ms(150)), vec![(tag(1), wid(1))]);
        assert!(watches.expire(start + ms(151)).is_empty(), "once");
        assert!(watches.next(start + ms(151)).is_none(), "and looked at no more");
    }

    /// A window the flight holds for is captured every 8ms once there, until two prints in a row match,
    /// and that one is delivered whatever its band print.
    #[test]
    fn a_repainting_landing_is_delivered_once_two_prints_in_a_row_match() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(3)).unwrap();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::Repainted, start + ms(150)),
            start,
        );

        let painting = std::cell::Cell::new(1u8);
        let mut at = start;
        let mut rounds = 0;
        let outcome = loop {
            let (sets, outcome) =
                round(&mut watches, at, |_| Some(LEFT), |_| Some(painting.get())).unwrap();
            assert!(sets[0].looks[0].thumbprint, "its thumbprint is wanted");
            rounds += 1;
            if !outcome.deliveries.is_empty() {
                break outcome;
            }
            // Repaints for two captures, then holds still.
            if rounds < 3 {
                painting.set(painting.get() + 1);
            }
            assert!(watches.next(at + ms(7)).is_none(), "every 8ms");
            at += REPAINT_EVERY;
        };
        assert_eq!(rounds, 4, "1, 2, 3, then 3 again");
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
    }

    /// One that never settles is given up on after `REPAINT_TRIES` captures.
    #[test]
    fn a_repainting_landing_that_never_settles_is_lost() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::Repainted, start + ms(150)),
            start,
        );
        let counter = std::cell::Cell::new(0u8);
        let mut at = start;
        let mut lost = Vec::new();
        for _ in 0..REPAINT_TRIES {
            let (_, outcome) = round(
                &mut watches,
                at,
                |_| Some(LEFT),
                |_| {
                    counter.set(counter.get().wrapping_add(1));
                    Some(counter.get())
                },
            )
            .unwrap();
            assert!(outcome.deliveries.is_empty());
            lost.extend(outcome.lost);
            at += REPAINT_EVERY;
        }
        assert_eq!(lost, vec![(tag(1), wid(1))]);
        assert!(watches.next(at).is_none());
        assert!(watches.expire(at + ms(500)).is_empty(), "lost once");
    }

    /// A landing whose capture the cache refuses for the picture it holds is lost at once, waiting or
    /// settling, and never delivered: every later capture at that size would be refused as well.
    #[test]
    fn a_landing_the_cache_refuses_is_lost_at_once() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        watches.watch(
            WatchId(2),
            landing(&[(2, RIGHT)], Settle::Repainted, start + ms(150)),
            start,
        );
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let (_, outcome) = round_of(&mut watches, start, at, |w| {
            if w == wid(1) {
                Capture::Refused
            } else {
                taken(1)
            }
        })
        .unwrap();
        assert!(outcome.deliveries.is_empty(), "refused, and not settled yet");
        assert_eq!(
            outcome.lost,
            vec![(tag(1), wid(1))],
            "at once, not at the deadline"
        );

        let next = start + REPAINT_EVERY;
        let (_, outcome) = round_of(&mut watches, next, at, |_| Capture::Refused).unwrap();
        assert!(outcome.deliveries.is_empty());
        assert_eq!(outcome.lost, vec![(tag(1), wid(2))], "settling, refused");
        assert!(watches.next(start + ms(50)).is_none(), "looked at no more");
        assert!(watches.expire(start + ms(150)).is_empty(), "each lost once");
    }

    /// Where a window ends cut by the left edge, as fold1's g18 unfold ends it.
    const CUT: ScreenRect = rect(-285.0, 32.0, 1148.0, 1081.0);

    /// A repainting window there, watched from `start`, handed to ScreenCaptureKit by the round that
    /// finds it there.
    fn handed_elsewhere(start: Instant) -> Watches {
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, CUT)], Settle::Repainted, start + ms(300)),
            start,
        );
        let (sets, outcome) = round(&mut watches, start, |_| Some(CUT), |_| Some(1)).unwrap();
        assert!(sets[0].looks[0].thumbprint);
        assert!(outcome.deliveries.is_empty(), "SkyLight cannot picture it whole");
        assert_eq!(outcome.elsewhere, vec![target(1)]);
        watches
    }

    /// A repainting window where it ends but cut by the display's edge is captured through
    /// ScreenCaptureKit, one capture after another, rather than looked at through SkyLight, and
    /// delivered as landed, at the frame it was found at, once two captures in a row match. A window
    /// only landing there is never handed over: it is lost at its deadline.
    #[test]
    fn a_repainting_landing_cut_by_the_displays_edge_settles_on_captures_elsewhere() {
        let start = Instant::now();
        let mut watches = handed_elsewhere(start);
        assert!(
            watches.next(start + ms(50)).is_none() && watches.wake(start).is_none(),
            "nothing to poll"
        );
        assert!(
            watches.expire(start + ms(300)).is_empty(),
            "there, so past its deadline"
        );
        let outcome = watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1]));
        assert!(outcome.deliveries.is_empty());
        assert_eq!(outcome.elsewhere, vec![target(1)], "another, to judge it by");
        let outcome = watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1]));
        assert_eq!(outcome.deliveries, vec![Delivery {
            tag: tag(1),
            why: Why::Landed,
            windows: vec![(wid(1), CUT)]
        }]);
        assert!(outcome.elsewhere.is_empty());
        assert_eq!(
            watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1])),
            Outcome::default(),
            "done"
        );

        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, CUT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        let (_, outcome) = round(&mut watches, start, |_| Some(CUT), |_| Some(1)).unwrap();
        assert!(outcome.elsewhere.is_empty());
        assert_eq!(watches.expire(start + ms(150)), vec![(tag(1), wid(1))]);
    }

    /// One captured elsewhere is lost at once when the cache refuses a capture, and once
    /// `ELSEWHERE_TRIES` captures never settle, an empty capture spending a try too.
    #[test]
    fn a_repainting_landing_captured_elsewhere_is_lost_when_refused_or_out_of_tries() {
        let start = Instant::now();
        let mut watches = handed_elsewhere(start);
        let outcome = watches.captured_elsewhere(wid(1), CapturedElsewhere::Refused);
        assert_eq!(
            (outcome.lost, outcome.elsewhere),
            (vec![(tag(1), wid(1))], Vec::new())
        );

        let mut watches = handed_elsewhere(start);
        let mut lost = Vec::new();
        for try_ in 0..ELSEWHERE_TRIES {
            let capture = if try_ == 0 {
                CapturedElsewhere::Missed
            } else {
                CapturedElsewhere::Taken(vec![(try_ as u8).wrapping_mul(50)])
            };
            let outcome = watches.captured_elsewhere(wid(1), capture);
            assert!(outcome.deliveries.is_empty());
            assert_eq!(
                outcome.elsewhere.is_empty(),
                try_ == ELSEWHERE_TRIES - 1,
                "try {try_}"
            );
            lost.extend(outcome.lost);
        }
        assert_eq!(lost, vec![(tag(1), wid(1))]);
    }

    /// A retarget to the same end carries a window settling elsewhere on, its capture out delivered
    /// under the new tag; one to a new end starts it afresh, and what comes back meanwhile is
    /// nobody's.
    #[test]
    fn a_retarget_carries_a_window_settling_elsewhere_on_while_it_ends_there() {
        let start = Instant::now();
        let mut watches = handed_elsewhere(start);
        watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1]));
        let retagged = |end| {
            landing_of(
                tag(2),
                vec![WindowServerId::new(9)],
                &[(1, end)],
                Settle::Repainted,
                start + ms(300),
            )
        };
        watches.retarget(WatchId(1), retagged(CUT), start + ms(40));
        assert!(watches.next(start + ms(40)).is_none(), "not polled again");
        let outcome = watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1]));
        assert_eq!(outcome.deliveries, vec![Delivery {
            tag: tag(2),
            why: Why::Landed,
            windows: vec![(wid(1), CUT)]
        }]);

        let mut watches = handed_elsewhere(start);
        let wider = rect(-285.0, 32.0, 1400.0, 1081.0);
        watches.retarget(WatchId(1), retagged(wider), start + ms(40));
        assert_eq!(
            watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1])),
            Outcome::default()
        );
        assert!(
            watches.next(start + ms(40)).is_some(),
            "looked for where it ends now"
        );
    }

    /// A window that landed and is retargeted to a new end settles there only on two captures in a
    /// row that match: the server resizes the frame before the app repaints, so the first capture
    /// there still shows the picture it landed with, and the next can be half-painted.
    #[test]
    fn a_window_that_landed_and_is_retargeted_to_a_new_end_settles_there_on_two_captures_in_a_row()
    {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::Repainted, start + ms(150)),
            start,
        );
        round(&mut watches, start, |_| Some(LEFT), |_| Some(2)).unwrap();
        let (_, outcome) =
            round(&mut watches, start + REPAINT_EVERY, |_| Some(LEFT), |_| Some(2)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
        let moved = rect(4.0, 32.0, 1200.0, 1081.0);
        let retargeted = start + REPAINT_EVERY + ms(1);
        watches.retarget(
            WatchId(1),
            landing(&[(1, moved)], Settle::Repainted, start + ms(150)),
            retargeted,
        );
        let mut at = retargeted;
        for (band, why) in [
            (2, "the picture it landed with"),
            (3, "half-painted"),
            (4, "painted, once"),
        ] {
            let (_, outcome) = round(&mut watches, at, |_| Some(moved), |_| Some(band)).unwrap();
            assert!(outcome.deliveries.is_empty(), "{why}: not settled");
            at += REPAINT_EVERY;
        }
        let (_, outcome) = round(&mut watches, at, |_| Some(moved), |_| Some(4)).unwrap();
        assert_eq!(
            delivered(&outcome),
            vec![(Why::Landed, vec![wid(1)])],
            "painted twice in a row"
        );
    }

    /// A window's landing delivered at one end, and its watch retargeted to another: the client draws
    /// nothing taken for the end before, so the window is delivered where it lands now, looking the
    /// same or not. One still ending where it landed is not delivered again.
    #[test]
    fn a_window_watched_to_land_somewhere_new_is_delivered_there_whatever_it_looks_like() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT), (2, RIGHT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        round(&mut watches, start, at, |_| Some(1)).unwrap();
        let moved = rect(1000.0, 32.0, 700.0, 1081.0);
        watches.retarget(
            WatchId(1),
            landing_of(
                tag(2),
                vec![WindowServerId::new(9)],
                &[(1, moved), (2, RIGHT)],
                Settle::AsSoonAsThere,
                start + ms(300),
            ),
            start + ms(50),
        );
        let (sets, outcome) =
            round(&mut watches, start + ms(50), |_| Some(moved), |_| Some(1)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(1)]]);
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
    }

    /// Deliveries carry the tag of the watch as it is now, so a client tells its generations apart.
    #[test]
    fn a_retargeted_watch_delivers_under_its_new_tag() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        watches.retarget(
            WatchId(1),
            landing_of(
                tag(2),
                vec![WindowServerId::new(9)],
                &[(1, LEFT)],
                Settle::AsSoonAsThere,
                start + ms(150),
            ),
            start,
        );
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        assert_eq!(outcome.deliveries[0].tag, tag(2));
    }

    /// See "A grow waits for its repaint" in `src/transitions/docs/coordination.md`.
    #[test]
    fn a_window_that_has_to_repaint_is_captured_every_8ms_for_a_second() {
        assert_eq!(REPAINT_EVERY, Duration::from_millis(8));
        assert_eq!(REPAINT_EVERY * REPAINT_TRIES as u32, Duration::from_secs(1));
    }
}
