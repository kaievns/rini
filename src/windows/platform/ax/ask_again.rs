//! How long to wait before asking an application too busy to answer Accessibility again: 20ms,
//! doubling to 1s, until the patience it was given runs out.
use std::future::Future;
use std::time::{Duration, Instant};

const FIRST_WAIT: Duration = Duration::from_millis(20);
const LONGEST_WAIT: Duration = Duration::from_secs(1);

pub struct AskAgain {
    deadline: Instant,
    wait: Duration,
}

impl AskAgain {
    pub fn new(now: Instant, patience: Duration) -> Self {
        Self {
            deadline: now + patience,
            wait: FIRST_WAIT,
        }
    }

    /// The wait before the next ask, or `None` once patience has run out at `now`.
    pub fn next_wait(&mut self, now: Instant) -> Option<Duration> {
        let remaining = self.deadline.checked_duration_since(now).filter(|left| !left.is_zero())?;
        let wait = self.wait.min(remaining);
        self.wait = (self.wait * 2).min(LONGEST_WAIT);
        Some(wait)
    }
}

/// Asks until `ask` answers true or `again` runs out of patience, sleeping through `sleep` between
/// asks; whether it was answered.
pub async fn ask_until<S: Future<Output = ()>>(
    mut again: AskAgain,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration) -> S,
    mut ask: impl FnMut() -> bool,
) -> bool {
    loop {
        if ask() {
            return true;
        }
        let Some(wait) = again.next_wait(now()) else {
            return false;
        };
        sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_wait_doubles_from_20ms_to_1s() {
        let start = Instant::now();
        let mut again = AskAgain::new(start, Duration::from_secs(60));
        let waits: Vec<_> = (0..8).map(|_| again.next_wait(start).unwrap()).collect();
        assert_eq!(
            waits,
            [
                ms(20),
                ms(40),
                ms(80),
                ms(160),
                ms(320),
                ms(640),
                ms(1000),
                ms(1000)
            ]
        );
    }

    #[test]
    fn the_last_wait_ends_at_the_deadline_and_none_comes_after() {
        let start = Instant::now();
        let mut again = AskAgain::new(start, ms(30));
        assert_eq!(again.next_wait(start), Some(ms(20)));
        assert_eq!(again.next_wait(start + ms(20)), Some(ms(10)));
        assert_eq!(again.next_wait(start + ms(30)), None);
        assert_eq!(AskAgain::new(start, Duration::ZERO).next_wait(start), None);
    }

    /// Drives `ask_until` on a clock that moves only when it sleeps, against an `ask` that is too busy
    /// to answer `busy` times; whether it was answered, how often it asked and how long it slept.
    fn run(patience: Duration, busy: u32) -> (bool, u32, Duration) {
        let start = Instant::now();
        let clock = Cell::new(start);
        let asks = Cell::new(0);
        let asked = ask_until(
            AskAgain::new(start, patience),
            || clock.get(),
            |wait| {
                clock.set(clock.get() + wait);
                std::future::ready(())
            },
            || {
                asks.set(asks.get() + 1);
                asks.get() > busy
            },
        );
        let Poll::Ready(answered) = pin!(asked).poll(&mut Context::from_waker(Waker::noop()))
        else {
            panic!("a sleep that is ready at once never leaves the asking pending");
        };
        (answered, asks.get(), clock.get() - start)
    }

    #[test]
    fn an_application_too_busy_to_answer_is_asked_again_until_it_answers() {
        assert_eq!(run(Duration::from_secs(60), 3), (true, 4, ms(20 + 40 + 80)));
    }

    #[test]
    fn an_application_that_answers_at_once_is_asked_once() {
        assert_eq!(run(Duration::from_secs(60), 0), (true, 1, Duration::ZERO));
    }

    #[test]
    fn an_application_that_never_answers_is_given_up_on_at_the_deadline() {
        let (answered, asks, slept) = run(Duration::from_secs(60), u32::MAX);
        assert!(!answered);
        assert_eq!(slept, Duration::from_secs(60));
        assert_eq!(
            asks, 66,
            "65 waits: 6 doubling to 640ms, 58 of 1s and the 740ms left, an ask either side"
        );
    }
}
