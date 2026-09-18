//! Awaitable `CFRunLoopTimer`s for the run-loop executor. A timer is installed on the run loop of
//! the thread that creates it and fires only while that loop runs; `tokio::time` cannot be used
//! here (see `executor::sleep`).

use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use objc2_core_foundation::{
    CFAbsoluteTime, CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer,
    CFRunLoopTimerContext, CFTimeInterval, kCFAllocatorDefault, kCFRunLoopCommonModes,
};
use parking_lot::Mutex;
use tokio_stream::Stream;

/// One-shot or repeating timer; awaiting it yields when it fires, or immediately after `cancel`.
pub struct Timer {
    inner: Arc<Mutex<TimerState>>,
}

struct TimerState {
    cf_timer: Option<CFRetained<CFRunLoopTimer>>,
    waker: Option<Waker>,
    status: TimerStatus,
}

enum TimerStatus {
    Pending,
    Cancelled,
    Fired,
}

impl Timer {
    /// Fires once after `duration`.
    pub fn sleep(duration: Duration) -> Self {
        let fire_time = CFAbsoluteTimeGetCurrent() + duration.as_secs_f64();
        Self::new(fire_time, 0.0)
    }
    /// Fires after `initial_delay`, then every `interval`.
    pub fn repeating(initial_delay: Duration, interval: Duration) -> Self {
        let fire_time = CFAbsoluteTimeGetCurrent() + initial_delay.as_secs_f64();
        Self::new(fire_time, interval.as_secs_f64())
    }
    /// A repeating timer whose next fire is set by `set_next_fire`; cheaper than a new timer per deadline.
    pub fn manual() -> Self {
        // Apple recommends a far-future repeating timer for this use:
        // https://developer.apple.com/documentation/corefoundation/cfrunlooptimersetnextfiredate(_:_:)
        Self::repeating(Duration::MAX, Duration::MAX)
    }
    /// `interval` of 0.0 makes a one-shot timer.
    pub fn new(fire_date: CFAbsoluteTime, interval: CFTimeInterval) -> Self {
        let inner = Arc::new(Mutex::new(TimerState {
            cf_timer: None,
            waker: None,
            status: TimerStatus::Pending,
        }));

        // We use a Weak reference to avoid circular references
        let timer_ref = Arc::downgrade(&inner);

        let callback_info = Arc::new(timer_ref);

        unsafe extern "C-unwind" fn retain(info: *const c_void) -> *const c_void {
            // SAFETY: The pointer was passed to CFRunLoopTimerContext.info below.
            unsafe { Arc::increment_strong_count(info.cast::<Weak<Mutex<TimerState>>>()) };
            info
        }

        unsafe extern "C-unwind" fn release(info: *const c_void) {
            // SAFETY: The pointer was passed to CFRunLoopTimerContext.info below.
            unsafe { Arc::decrement_strong_count(info.cast::<Weak<Mutex<TimerState>>>()) };
        }

        unsafe extern "C-unwind" fn timer_fire_callback(
            _timer: *mut CFRunLoopTimer,
            info: *mut c_void,
        ) {
            if info.is_null() {
                return;
            }

            // SAFETY: The pointer was passed to CFRunLoopTimerContext.info below.
            let timer_ref = unsafe { &*info.cast::<Weak<Mutex<TimerState>>>() };

            let Some(timer) = timer_ref.upgrade() else { return };
            let mut state = timer.lock();
            state.status = TimerStatus::Fired;
            if let Some(waker) = state.waker.take() {
                waker.wake();
            }
        }

        let mut context = CFRunLoopTimerContext {
            version: 0,
            // This pointer is retained by CF on creation.
            info: Arc::as_ptr(&callback_info) as *mut c_void,
            retain: Some(retain),
            release: Some(release),
            copyDescription: None,
        };

        // SAFETY: The retain/release callbacks and info are thread-safe.
        let cf_timer = unsafe {
            CFRunLoopTimer::new(
                kCFAllocatorDefault,
                fire_date,
                interval,
                0, // flags - documentation says to pass 0 for future compatibility
                0, // order
                Some(timer_fire_callback),
                &mut context,
            )
        }
        .expect("Failed to create CFRunLoopTimer");

        let current_loop = CFRunLoop::current().expect("Failed to get current run loop");
        current_loop.add_timer(Some(&cf_timer), unsafe { kCFRunLoopCommonModes });

        {
            let mut state = inner.lock();
            state.cf_timer = Some(cf_timer);
        }

        Timer { inner }
    }

    /// Sets the next time the timer will fire.
    pub fn set_next_fire(&self, delay: Duration) {
        let mut state = self.inner.lock();
        let Some(cf_timer) = state.cf_timer.as_mut() else {
            return;
        };
        let fire_time = CFAbsoluteTimeGetCurrent() + delay.as_secs_f64();
        cf_timer.set_next_fire_date(fire_time);
    }
    /// Awaiting a cancelled timer completes immediately; `next()` returns `None`.
    pub fn cancel(&self) {
        let mut state = self.inner.lock();
        if let Some(cf_timer) = state.cf_timer.take() {
            cf_timer.invalidate();
        }
        state.status = TimerStatus::Cancelled;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }
    pub fn next(&mut self) -> impl Future<Output = Option<()>> {
        tokio_stream::StreamExt::next(self)
    }
}

impl Stream for Timer {
    type Item = ();

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut state = self.inner.lock();
        match state.status {
            TimerStatus::Cancelled => Poll::Ready(None),
            TimerStatus::Fired => {
                state.status = TimerStatus::Pending;
                Poll::Ready(Some(()))
            }
            TimerStatus::Pending => {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl Future for Timer {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.poll_next(cx).map(|_| ())
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::executor::Executor;

    #[test]
    fn timer_fires_after_delay() {
        let start = Instant::now();
        let delay = Duration::from_millis(50);

        Executor::run(async move {
            Timer::sleep(delay).await;
        });

        let elapsed = start.elapsed();
        // Allow some tolerance; documentation allows sub-millisecond tweaks.
        assert!(elapsed >= delay - Duration::from_millis(1));
    }

    #[test]
    fn timer_can_be_cancelled() {
        let timer = Timer::sleep(Duration::from_secs(100));
        timer.cancel();

        let start = Instant::now();
        Executor::run(async move {
            timer.await;
        });

        assert!(start.elapsed() < Duration::from_secs(45));
    }

    #[test]
    fn multiple_timers_work() {
        let start = Instant::now();

        Executor::run(async move {
            let timer1 = Timer::sleep(Duration::from_millis(30));
            let timer2 = Timer::sleep(Duration::from_millis(60));

            timer1.await;
            let mid = start.elapsed();

            timer2.await;
            let end = start.elapsed();

            assert!(mid >= Duration::from_millis(25));
            assert!(end >= Duration::from_millis(55));
        });
    }

    #[test]
    fn timer_thread_safety() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::thread;


        let timer_fired = Arc::new(AtomicBool::new(false));
        let timer_fired_clone = Arc::clone(&timer_fired);

        let handle = thread::spawn(move || {
            Executor::run(async move {
                let timer = Timer::sleep(Duration::from_millis(50));
                timer.await;
                timer_fired_clone.store(true, Ordering::Relaxed);
            });
        });

        handle.join().unwrap();

        assert!(timer_fired.load(Ordering::Relaxed));
    }

    #[test]
    fn repeating_timer_works() {
        Executor::run(async {
            let start = Instant::now();
            let mut timer = Timer::repeating(
                Duration::from_millis(20), // initial delay
                Duration::from_millis(10), // repeat interval
            );

            timer.next().await; // First fire (after initial delay)
            let elapsed1 = start.elapsed();

            timer.next().await; // Second fire (after repeat interval)
            let elapsed2 = start.elapsed();

            timer.next().await; // Third fire
            let elapsed3 = start.elapsed();

            assert!(elapsed1 >= Duration::from_millis(20 - 1));
            assert!(elapsed2 >= Duration::from_millis(30 - 1));
            assert!(elapsed3 >= Duration::from_millis(40 - 1));
        });
    }
}
