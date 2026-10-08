//! The turns of a thread's run loop, timed: from waking to going back to sleep, how long each actor's
//! polls took, the time outside them, and the window changes made just before. The observer that
//! opens and closes a turn is `run_loop::TurnObserver`; the actors are timed by `timed`, and each
//! window change is noted by `changed` where it is made. All of it is per thread.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::time::{Duration, Instant};

/// A turn longer than this, one frame at 120Hz, is reported.
pub const SLOW: Duration = Duration::from_millis(8);

/// How long before a slow turn began a window change is still listed with it: the flush that holds a
/// turn commits changes earlier turns made.
pub const CHANGES_BEFORE: Duration = Duration::from_millis(50);

/// How many window changes a thread keeps.
const CHANGES_KEPT: usize = 64;

/// A turn of the run loop over `SLOW`: when it woke, how long it ran, each actor polled in it with
/// the time its polls took, in the order first polled, the busy time no poll covers, and the window
/// changes made from `CHANGES_BEFORE` before it began, each with when.
#[derive(Clone, Debug, PartialEq)]
pub struct MainTurn {
    pub began: Instant,
    pub busy: Duration,
    pub actors: Vec<(&'static str, Duration)>,
    pub outside: Duration,
    pub changes: Vec<(&'static str, Instant)>,
}

/// What a thread's turn has gathered so far.
#[derive(Default)]
struct Turn {
    began: Option<Instant>,
    actors: Vec<(&'static str, Duration)>,
    changes: VecDeque<(&'static str, Instant)>,
}

thread_local! {
    static TURN: RefCell<Turn> = RefCell::new(Turn {
        began: None,
        actors: Vec::with_capacity(16),
        changes: VecDeque::with_capacity(CHANGES_KEPT),
    });
}

/// The turn that ran from `began` to `ended`, with the polls it took and the changes kept, if it was
/// slow.
pub fn fold(
    began: Instant,
    ended: Instant,
    actors: &[(&'static str, Duration)],
    changes: &VecDeque<(&'static str, Instant)>,
) -> Option<MainTurn> {
    let busy = ended.saturating_duration_since(began);
    if busy <= SLOW {
        return None;
    }
    let polled: Duration = actors.iter().map(|(_, took)| *took).sum();
    let since = began.checked_sub(CHANGES_BEFORE).unwrap_or(began);
    Some(MainTurn {
        began,
        busy,
        actors: actors.to_vec(),
        outside: busy.saturating_sub(polled),
        changes: changes.iter().copied().filter(|(_, at)| *at >= since).collect(),
    })
}

/// `took` added to `actor`'s time in this turn.
fn add(actors: &mut Vec<(&'static str, Duration)>, actor: &'static str, took: Duration) {
    match actors.iter_mut().find(|(name, _)| *name == actor) {
        Some((_, sum)) => *sum += took,
        None => actors.push((actor, took)),
    }
}

/// The thread's run loop woke at `now`: a turn begins.
pub fn woke(now: Instant) {
    TURN.with_borrow_mut(|turn| {
        turn.began = Some(now);
        turn.actors.clear();
    });
}

/// The thread's run loop goes to sleep at `now`: the turn ends, and is given back if it was slow.
pub fn sleeps(now: Instant) -> Option<MainTurn> {
    TURN.with_borrow_mut(|turn| {
        let slow = turn.began.take().and_then(|began| fold(began, now, &turn.actors, &turn.changes));
        turn.actors.clear();
        slow
    })
}

/// `actor` was polled for `took` in this thread's turn.
pub fn polled(actor: &'static str, took: Duration) {
    TURN.with_borrow_mut(|turn| add(&mut turn.actors, actor, took));
}

/// A window was changed on this thread, now: `what` names the change, `rest.frame` or `overlay.show`.
pub fn changed(what: &'static str) {
    let now = Instant::now();
    TURN.with_borrow_mut(|turn| {
        if turn.changes.len() == CHANGES_KEPT {
            turn.changes.pop_front();
        }
        turn.changes.push_back((what, now));
    });
}

/// `actor`, each of its polls timed into this thread's turn as `actor`.
pub async fn timed<F: Future>(actor: &'static str, future: F) -> F::Output {
    let mut future = pin!(future);
    poll_fn(|cx| {
        let started = Instant::now();
        let poll = future.as_mut().poll(cx);
        polled(actor, started.elapsed());
        poll
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration { Duration::from_millis(n) }

    #[test]
    fn an_iteration_over_a_frame_is_reported_with_each_actor_and_what_was_outside_them() {
        let began = Instant::now() + ms(100);
        let mut actors = Vec::new();
        for (actor, took) in [("renderer", ms(2)), ("rest", ms(3)), ("renderer", ms(1)), ("bar", Duration::ZERO)] {
            add(&mut actors, actor, took);
        }
        let changes = VecDeque::from([("rest.frame", began - ms(60)), ("overlay.show", began - ms(20)), ("rest.show", began + ms(4))]);
        let turn = fold(began, began + ms(12), &actors, &changes).expect("slow");
        assert_eq!(turn, MainTurn {
            began,
            busy: ms(12),
            actors: vec![("renderer", ms(3)), ("rest", ms(3)), ("bar", Duration::ZERO)],
            outside: ms(6),
            changes: vec![("overlay.show", began - ms(20)), ("rest.show", began + ms(4))],
        });
    }

    #[test]
    fn a_quick_iteration_reports_nothing() {
        let began = Instant::now();
        let changes = VecDeque::from([("overlay.show", began)]);
        assert_eq!(fold(began, began + SLOW, &[("renderer", ms(7))], &changes), None, "one frame is not over it");
        assert!(fold(began, began + SLOW + Duration::from_micros(1), &[], &changes).is_some());
    }

    #[test]
    fn a_thread_keeps_its_last_64_window_changes_and_a_turn_is_timed_from_its_wake() {
        for _ in 0..CHANGES_KEPT {
            changed("rest.frame");
        }
        changed("overlay.show");
        TURN.with_borrow(|turn| {
            assert_eq!(turn.changes.len(), CHANGES_KEPT);
            assert_eq!(turn.changes.back().map(|(what, _)| *what), Some("overlay.show"));
        });
        polled("before", ms(1));
        let now = Instant::now();
        woke(now);
        polled("renderer", ms(9));
        let turn = sleeps(now + ms(10)).expect("slow");
        assert_eq!(turn.actors, [("renderer", ms(9))], "a poll before the wake is not this turn's");
        assert_eq!(turn.outside, ms(1));
        assert_eq!(turn.changes.len(), CHANGES_KEPT);
        assert_eq!(sleeps(now + ms(30)), None, "no turn open");
    }
}
