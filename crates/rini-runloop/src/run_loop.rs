//! Helpers for managing run loops.

use std::ffi::c_void;
use std::mem;
use std::time::Duration;

use std::time::Instant;

use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFIndex, CFRetained, CFRunLoop, CFRunLoopActivity, CFRunLoopObserver,
    CFRunLoopObserverContext, CFRunLoopSource, CFRunLoopSourceContext, CFRunLoopTimer,
    CFRunLoopTimerContext, kCFRunLoopCommonModes,
};

use crate::turns::{self, MainTurn};

/// A core foundation run loop source.
///
/// This type primarily exists for the purpose of managing manual sources, which
/// can be used for signaling code that blocks on a run loop.
///
/// More information is available in the Apple documentation at
/// <https://developer.apple.com/documentation/corefoundation/cfrunloopsource-rhr>.
#[derive(Clone, PartialEq)]
pub struct WakeupHandle(CFRetained<CFRunLoopSource>, CFRetained<CFRunLoop>);

// SAFETY:
// - CFRunLoopSource and CFRunLoop are CoreFoundation/ObjC objects which are allowed to be used
//   from multiple threads.
// - This handle only exposes `wake()` (signal + wake_up). It does not expose the underlying
//   handler or allow mutation of the run loop/source beyond signaling.
// - Therefore it is safe to treat this as Send + Sync for the purposes of a Waker hot path.
unsafe impl Send for WakeupHandle {}
unsafe impl Sync for WakeupHandle {}

struct Handler<F> {
    ref_count: isize,
    func: F,
}

impl WakeupHandle {
    /// Creates and adds a manual source for the current [`CFRunLoop`].
    ///
    /// The supplied function `handler` is called inside the run loop when this
    /// handle has been woken and the run loop is running.
    ///
    /// The handler is run in all common modes. `order` controls the order it is
    /// run in relative to other run loop sources, and should normally be set to
    /// 0.
    pub fn for_current_thread<F: Fn() + 'static>(order: CFIndex, handler: F) -> WakeupHandle {
        let handler_ptr = Box::into_raw(Box::new(Handler { ref_count: 0, func: handler }));

        // `info` is a *mut Handler<F>; retain/release keep its count and free the box at zero.
        unsafe extern "C-unwind" fn perform<F: Fn() + 'static>(info: *mut c_void) {
            // SAFETY: `info` was created from a Box<Handler<F>> and is valid.
            let handler = unsafe { &mut *(info as *mut Handler<F>) };
            (handler.func)();
        }
        unsafe extern "C-unwind" fn retain<F>(info: *const c_void) -> *const c_void {
            // SAFETY: `info` was created from a Box<Handler<F>> and is valid.
            let handler = unsafe { &mut *(info as *mut Handler<F>) };
            handler.ref_count += 1;
            info
        }
        unsafe extern "C-unwind" fn release<F>(info: *const c_void) {
            // SAFETY: `info` was created from a Box<Handler<F>> and is valid.
            let handler = unsafe { &mut *(info as *mut Handler<F>) };
            handler.ref_count -= 1;
            if handler.ref_count == 0 {
                // Recreate the Box to drop it.
                mem::drop(unsafe { Box::from_raw(info as *mut Handler<F>) });
            }
        }

        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: handler_ptr as *mut c_void,
            retain: Some(retain::<F>),
            release: Some(release::<F>),
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(perform::<F>),
        };

        let source = unsafe { CFRunLoopSource::new(None, order, &mut context as *mut _) };

        let run_loop = CFRunLoop::current().unwrap();
        run_loop.add_source(source.as_deref(), unsafe { kCFRunLoopCommonModes });

        WakeupHandle(source.unwrap(), run_loop)
    }

    /// Wakes the run loop that owns the target of this handle and schedules its
    /// handler to be called.
    ///
    /// Multiple signals may be collapsed into a single call of the handler.
    pub fn wake(&self) {
        self.0.signal();
        self.1.wake_up();
    }
}

/// A repeating timer on the current [`CFRunLoop`], invalidated on drop. The run-loop replacement for
/// `tokio::time` (`docs/run-loop-executor.md`).
pub struct RepeatingTimer {
    timer: CFRetained<CFRunLoopTimer>,
    run_loop: CFRetained<CFRunLoop>,
    /// Type-erased so `Drop` frees it as the right type and runs the closure's own destructor.
    handler: *mut Box<dyn Fn()>,
}

impl RepeatingTimer {
    /// Schedules `handler` to run every `interval` on the current run loop, in all common modes.
    ///
    /// The first fire is one full interval away, which is what a frame clock wants: the caller has
    /// just drawn frame zero itself.
    pub fn every<F: Fn() + 'static>(interval: Duration, handler: F) -> Option<Self> {
        unsafe extern "C-unwind" fn callout(_timer: *mut CFRunLoopTimer, info: *mut c_void) {
            if info.is_null() {
                return;
            }
            // SAFETY: `info` is the pointer handed to CFRunLoopTimerContext below. It stays valid
            // until Drop invalidates the timer, which is what prevents this running afterwards.
            let handler = unsafe { &*(info as *const Box<dyn Fn()>) };
            handler();
        }

        let seconds = interval.as_secs_f64();
        let handler: Box<dyn Fn()> = Box::new(handler);
        let handler_ptr = Box::into_raw(Box::new(handler));
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: handler_ptr as *mut c_void,
            retain: None,
            release: None,
            copyDescription: None,
        };

        let fire_date = CFAbsoluteTimeGetCurrent() + seconds;
        // SAFETY: the context pointer is valid, and the callout matches CFRunLoopTimerCallBack.
        let timer = unsafe {
            CFRunLoopTimer::new(
                None,
                fire_date,
                seconds,
                0,
                0,
                Some(callout),
                &mut context as *mut _,
            )
        };
        let Some(timer) = timer else {
            // SAFETY: the timer was never created, so nothing can reference the handler.
            drop(unsafe { Box::from_raw(handler_ptr) });
            return None;
        };
        let Some(run_loop) = CFRunLoop::current() else {
            // SAFETY: as above, the timer was never scheduled.
            drop(unsafe { Box::from_raw(handler_ptr) });
            return None;
        };
        run_loop.add_timer(Some(&timer), unsafe { kCFRunLoopCommonModes });

        Some(Self {
            timer,
            run_loop,
            handler: handler_ptr,
        })
    }
}

impl Drop for RepeatingTimer {
    fn drop(&mut self) {
        // Invalidate before freeing the handler: invalidation is what guarantees the callout will
        // not be entered again.
        self.timer.invalidate();
        self.run_loop.remove_timer(Some(&self.timer), unsafe { kCFRunLoopCommonModes });
        if !self.handler.is_null() {
            // SAFETY: the timer is invalidated and removed, so the callout cannot run again, and
            // nothing else holds this pointer.
            drop(unsafe { Box::from_raw(self.handler) });
            self.handler = std::ptr::null_mut();
        }
    }
}

/// Times every turn of the current thread's run loop, in all common modes, from waking to going back
/// to sleep, and hands each one over `turns::SLOW` to its handler. Removed on drop.
///
/// The turn opens before any other after-waiting observer and closes after every before-waiting one,
/// so it holds the work the loop woke for and Core Animation's commit at the end of it (CA's observer
/// order, 2000000, is inferred, not verified).
pub struct TurnObserver {
    woke: CFRetained<CFRunLoopObserver>,
    sleeps: CFRetained<CFRunLoopObserver>,
    run_loop: CFRetained<CFRunLoop>,
    handler: *mut Box<dyn Fn(MainTurn)>,
}

impl TurnObserver {
    pub fn install<F: Fn(MainTurn) + 'static>(handler: F) -> Option<Self> {
        unsafe extern "C-unwind" fn woke(_: *mut CFRunLoopObserver, _: CFRunLoopActivity, _: *mut c_void) {
            turns::woke(Instant::now());
        }
        unsafe extern "C-unwind" fn sleeps(_: *mut CFRunLoopObserver, _: CFRunLoopActivity, info: *mut c_void) {
            let Some(turn) = turns::sleeps(Instant::now()) else { return };
            // SAFETY: `info` is the handler `install` boxed, freed only after `Drop` invalidated
            // this observer, which is what stops it being called.
            let handler = unsafe { &*(info as *const Box<dyn Fn(MainTurn)>) };
            handler(turn);
        }

        let handler: Box<dyn Fn(MainTurn)> = Box::new(handler);
        let handler_ptr = Box::into_raw(Box::new(handler));
        let observer = |activity: CFRunLoopActivity, order, callout: unsafe extern "C-unwind" fn(_, _, _), info| {
            let mut context =
                CFRunLoopObserverContext { version: 0, info, retain: None, release: None, copyDescription: None };
            // SAFETY: the callout matches CFRunLoopObserverCallBack and the context outlives the call,
            // which copies it.
            unsafe { CFRunLoopObserver::new(None, activity.0, true, order, Some(callout), &mut context) }
        };
        let observers = (
            observer(CFRunLoopActivity::AfterWaiting, CFIndex::MIN, woke, std::ptr::null_mut()),
            observer(CFRunLoopActivity::BeforeWaiting, CFIndex::MAX, sleeps, handler_ptr as *mut c_void),
            CFRunLoop::current(),
        );
        let (Some(woke), Some(sleeps), Some(run_loop)) = observers else {
            // SAFETY: no observer was added, so nothing can reference the handler.
            drop(unsafe { Box::from_raw(handler_ptr) });
            return None;
        };
        let modes = unsafe { kCFRunLoopCommonModes };
        run_loop.add_observer(Some(&woke), modes);
        run_loop.add_observer(Some(&sleeps), modes);
        Some(Self { woke, sleeps, run_loop, handler: handler_ptr })
    }
}

impl Drop for TurnObserver {
    fn drop(&mut self) {
        let modes = unsafe { kCFRunLoopCommonModes };
        for observer in [&self.woke, &self.sleeps] {
            observer.invalidate();
            self.run_loop.remove_observer(Some(observer), modes);
        }
        // SAFETY: both observers are invalidated and removed, so the callout cannot run again, and
        // nothing else holds this pointer.
        drop(unsafe { Box::from_raw(self.handler) });
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::thread;

    use super::*;
    use crate::executor::{Executor, sleep};

    #[test]
    fn a_turn_over_a_frame_is_handed_over_with_the_actor_that_held_it() {
        let turns = thread::spawn(|| {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let observer = TurnObserver::install({
                let seen = seen.clone();
                move |turn| seen.borrow_mut().push(turn)
            });
            Executor::run(turns::timed("busy", async {
                sleep(Duration::from_millis(5)).await;
                thread::sleep(Duration::from_millis(12));
                sleep(Duration::from_millis(5)).await;
            }));
            drop(observer);
            seen.take().into_iter().map(|turn| (turn.busy, turn.actors)).collect::<Vec<_>>()
        });
        let turns = turns.join().unwrap();
        assert_eq!(turns.len(), 1, "{turns:?}");
        let (busy, actors) = &turns[0];
        assert!(*busy >= Duration::from_millis(12), "{busy:?}");
        assert_eq!(actors.iter().map(|(actor, _)| *actor).collect::<Vec<_>>(), ["busy"]);
        assert!(actors[0].1 >= Duration::from_millis(12));
    }
}
