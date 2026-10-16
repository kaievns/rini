//! Helpers for managing run loops.

use std::ffi::c_void;
use std::mem;
use std::rc::Rc;
use std::time::{Duration, Instant};

use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFIndex, CFRetained, CFRunLoop, CFRunLoopActivity, CFRunLoopMode,
    CFRunLoopObserver, CFRunLoopObserverContext, CFRunLoopSource, CFRunLoopSourceContext,
    CFRunLoopTimer, CFRunLoopTimerContext, kCFRunLoopCommonModes,
};

use crate::turns::{self, MainTurn};

/// `kCFRunLoopCommonModes`, the modes every source, timer and observer here is added in.
pub(crate) fn common_modes() -> Option<&'static CFRunLoopMode> {
    // SAFETY: a CoreFoundation constant, set before any Rust code runs and never written.
    unsafe { kCFRunLoopCommonModes }
}

/// A core foundation run loop source.
///
/// This type primarily exists for the purpose of managing manual sources, which
/// can be used for signaling code that blocks on a run loop.
///
/// More information is available in the Apple documentation at
/// <https://developer.apple.com/documentation/corefoundation/cfrunloopsource-rhr>.
#[derive(Clone, PartialEq)]
pub struct WakeupHandle(CFRetained<CFRunLoopSource>, CFRetained<CFRunLoop>);

// SAFETY: the handle holds two retained CF objects, whose retain and release are atomic, and
// reaches the handler only through `wake`: CFRunLoopSourceSignal and CFRunLoopWakeUp may be called
// from any thread, and the handler runs only on the run loop it was added to. A handle dropped last
// on another thread after that loop's thread ended frees the source there, through the run loop it
// retains, and `release` then leaks the non-`Send` closure rather than drop it off its thread.
unsafe impl Send for WakeupHandle {}
// SAFETY: as for `Send`: `wake`, the one thing a shared handle does, is callable from any thread.
unsafe impl Sync for WakeupHandle {}

struct Handler<F> {
    ref_count: isize,
    func: F,
    /// The thread that made it, the one `func` may be dropped on.
    owner: libc::pthread_t,
}

/// The calling thread. It needs no thread-local, so it reads while the thread's run loop is torn
/// down; only a thread begun after this one ended can share it.
fn this_thread() -> libc::pthread_t {
    // SAFETY: names the calling thread; no pointer is passed or read.
    unsafe { libc::pthread_self() }
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
        let handler_ptr = Box::into_raw(Box::new(Handler {
            ref_count: 0,
            func: handler,
            owner: this_thread(),
        }));

        // `info` is a *mut Handler<F>; retain/release keep its count and free the box at zero.
        unsafe extern "C-unwind" fn perform<F: Fn() + 'static>(info: *mut c_void) {
            // SAFETY: `info` is the leaked Box<Handler<F>>, freed only by the last release, after
            // the source's last perform. Shared, so a perform re-entered from a nested run of the
            // loop aliases nothing mutable.
            let handler = unsafe { &*(info as *const Handler<F>) };
            (handler.func)();
        }
        unsafe extern "C-unwind" fn retain<F>(info: *const c_void) -> *const c_void {
            // SAFETY: `info` is the leaked Box<Handler<F>>; CF retains it once, as it creates the
            // source, while nothing else can reach it.
            let handler = unsafe { &mut *(info as *mut Handler<F>) };
            handler.ref_count += 1;
            info
        }
        unsafe extern "C-unwind" fn release<F>(info: *const c_void) {
            // SAFETY: `info` is the leaked Box<Handler<F>>; CF releases it as the source is freed,
            // when no perform can be running.
            let handler = unsafe { &mut *(info as *mut Handler<F>) };
            handler.ref_count -= 1;
            if handler.ref_count == 0 {
                // SAFETY: the count retain raised is back to zero, so this is the box's last use.
                let handler = unsafe { Box::from_raw(info as *mut Handler<F>) };
                let Handler { func, owner, .. } = *handler;
                if owner != this_thread() {
                    mem::forget(func);
                }
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

        // SAFETY: the context is live for the call, which copies it, and its callbacks each take
        // the `Handler<F>` its `info` points at.
        let source = unsafe { CFRunLoopSource::new(None, order, &mut context as *mut _) };

        let run_loop = CFRunLoop::current().unwrap();
        run_loop.add_source(source.as_deref(), common_modes());

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

/// A handler a CF callout reaches through its `info` pointer. The owner holds one count and each
/// callout in progress its own, so a handler that drops its owner is freed only once it returns.
struct SharedHandler<H>(*const H);

impl<H> SharedHandler<H> {
    fn new(handler: H) -> Self { Self(Rc::into_raw(Rc::new(handler))) }

    fn info(&self) -> *mut c_void { self.0.cast_mut().cast() }

    /// The handler, held for one callout.
    ///
    /// # Safety
    ///
    /// `info` is the [`info`](Self::info) of a `SharedHandler<H>` that has not dropped.
    unsafe fn for_call(info: *mut c_void) -> Rc<H> {
        let handler = info.cast_const().cast::<H>();
        // SAFETY: `handler` came from `Rc::into_raw` and, by the caller's contract, the owner still
        // holds the count `new` took.
        unsafe {
            Rc::increment_strong_count(handler);
            Rc::from_raw(handler)
        }
    }
}

impl<H> Drop for SharedHandler<H> {
    fn drop(&mut self) {
        // SAFETY: gives back the count `new` took, once; a callout running now holds its own.
        drop(unsafe { Rc::from_raw(self.0) });
    }
}

/// A repeating timer on the current [`CFRunLoop`], invalidated on drop. The run-loop replacement for
/// `tokio::time` (`docs/run-loop-executor.md`).
pub struct RepeatingTimer {
    timer: CFRetained<CFRunLoopTimer>,
    run_loop: CFRetained<CFRunLoop>,
    /// Held for its drop, which comes after `Drop` has invalidated the timer.
    _handler: SharedHandler<Box<dyn Fn()>>,
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
            // SAFETY: `info` is the timer's handler, which its Drop frees only after invalidating the
            // timer, so no callout starts once it is gone.
            let handler = unsafe { SharedHandler::<Box<dyn Fn()>>::for_call(info) };
            handler();
        }

        let seconds = interval.as_secs_f64();
        let handler = SharedHandler::new(Box::new(handler) as Box<dyn Fn()>);
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: handler.info(),
            retain: None,
            release: None,
            copyDescription: None,
        };

        let fire_date = CFAbsoluteTimeGetCurrent() + seconds;
        // SAFETY: the context is live for the call, which copies it; its `info` is the shared
        // handler, which `callout`, of CFRunLoopTimerCallBack's signature, reads as its own type.
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
        let timer = timer?;
        let run_loop = CFRunLoop::current()?;
        run_loop.add_timer(Some(&timer), common_modes());

        Some(Self {
            timer,
            run_loop,
            _handler: handler,
        })
    }
}

impl Drop for RepeatingTimer {
    fn drop(&mut self) {
        self.timer.invalidate();
        self.run_loop.remove_timer(Some(&self.timer), common_modes());
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
    /// Held for its drop, which comes after `Drop` has invalidated both observers.
    _handler: SharedHandler<Box<dyn Fn(MainTurn)>>,
}

impl TurnObserver {
    pub fn install<F: Fn(MainTurn) + 'static>(handler: F) -> Option<Self> {
        unsafe extern "C-unwind" fn woke(
            _: *mut CFRunLoopObserver,
            _: CFRunLoopActivity,
            _: *mut c_void,
        ) {
            turns::woke(Instant::now());
        }
        unsafe extern "C-unwind" fn sleeps(
            _: *mut CFRunLoopObserver,
            _: CFRunLoopActivity,
            info: *mut c_void,
        ) {
            let Some(turn) = turns::sleeps(Instant::now()) else {
                return;
            };
            // SAFETY: `info` is the observer's handler, which its Drop frees only after
            // invalidating this observer, so no callout starts once it is gone.
            let handler = unsafe { SharedHandler::<Box<dyn Fn(MainTurn)>>::for_call(info) };
            handler(turn);
        }

        let handler = SharedHandler::new(Box::new(handler) as Box<dyn Fn(MainTurn)>);
        let observer = |activity: CFRunLoopActivity,
                        order,
                        callout: unsafe extern "C-unwind" fn(_, _, _),
                        info| {
            let mut context = CFRunLoopObserverContext {
                version: 0,
                info,
                retain: None,
                release: None,
                copyDescription: None,
            };
            // SAFETY: the callout matches CFRunLoopObserverCallBack and the context outlives the call,
            // which copies it.
            unsafe {
                CFRunLoopObserver::new(None, activity.0, true, order, Some(callout), &mut context)
            }
        };
        let observers = (
            observer(
                CFRunLoopActivity::AfterWaiting,
                CFIndex::MIN,
                woke,
                std::ptr::null_mut(),
            ),
            observer(
                CFRunLoopActivity::BeforeWaiting,
                CFIndex::MAX,
                sleeps,
                handler.info(),
            ),
            CFRunLoop::current(),
        );
        let (Some(woke), Some(sleeps), Some(run_loop)) = observers else {
            return None;
        };
        let modes = common_modes();
        run_loop.add_observer(Some(&woke), modes);
        run_loop.add_observer(Some(&sleeps), modes);
        Some(Self {
            woke,
            sleeps,
            run_loop,
            _handler: handler,
        })
    }
}

impl Drop for TurnObserver {
    fn drop(&mut self) {
        let modes = common_modes();
        for observer in [&self.woke, &self.sleeps] {
            observer.invalidate();
            self.run_loop.remove_observer(Some(observer), modes);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;
    use crate::executor::{Executor, sleep};

    /// Records each thread it is dropped on.
    struct DropProbe(Arc<Mutex<Vec<libc::pthread_t>>>);

    impl Drop for DropProbe {
        fn drop(&mut self) { self.0.lock().unwrap().push(this_thread()); }
    }

    #[test]
    fn a_handler_is_never_dropped_on_a_thread_other_than_the_one_that_made_it() {
        let drops = Arc::new(Mutex::new(Vec::new()));
        let handle = thread::spawn({
            let drops = drops.clone();
            move || {
                let probe = DropProbe(drops);
                WakeupHandle::for_current_thread(0, move || {
                    let _ = &probe;
                })
            }
        })
        .join()
        .unwrap();
        assert!(
            drops.lock().unwrap().is_empty(),
            "the ended thread's loop still holds it"
        );
        drop(handle);
        assert_eq!(
            *drops.lock().unwrap(),
            [],
            "not dropped here, off the thread that made it"
        );

        let owner = thread::spawn({
            let drops = drops.clone();
            move || {
                let probe = DropProbe(drops);
                drop(WakeupHandle::for_current_thread(0, move || {
                    let _ = &probe;
                }));
                this_thread()
            }
        })
        .join()
        .unwrap();
        assert_eq!(
            *drops.lock().unwrap(),
            [owner],
            "dropped as its own thread ended"
        );
    }

    #[test]
    fn a_timer_dropped_by_its_own_handler_frees_the_handler_only_once_it_returns() {
        let drops = Arc::new(Mutex::new(Vec::new()));
        let drops_when_it_returned = Rc::new(Cell::new(None));
        let slot: Rc<RefCell<Option<RepeatingTimer>>> = Rc::default();
        let timer = RepeatingTimer::every(Duration::from_millis(5), {
            let slot = slot.clone();
            let probe = DropProbe(drops.clone());
            let drops_when_it_returned = drops_when_it_returned.clone();
            move || {
                drop(slot.borrow_mut().take());
                drops_when_it_returned.set(Some(probe.0.lock().unwrap().len()));
            }
        });
        *slot.borrow_mut() = timer;

        Executor::run(async { sleep(Duration::from_millis(30)).await });

        assert!(slot.borrow().is_none(), "the handler ran and took its timer");
        assert_eq!(
            drops_when_it_returned.get(),
            Some(0),
            "its captures live through the call"
        );
        assert_eq!(drops.lock().unwrap().len(), 1, "and drop once it returns");
    }

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
        assert_eq!(actors.iter().map(|(actor, _)| *actor).collect::<Vec<_>>(), [
            "busy"
        ]);
        assert!(actors[0].1 >= Duration::from_millis(12));
    }
}
