//! Helpers for managing run loops.

use std::ffi::c_void;
use std::mem;
use std::time::Duration;

use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFIndex, CFRetained, CFRunLoop, CFRunLoopSource,
    CFRunLoopSourceContext, CFRunLoopTimer, CFRunLoopTimerContext, kCFRunLoopCommonModes,
};

/// A core foundation run loop source.
///
/// This type primarily exists for the purpose of managing manual sources, which
/// can be used for signaling code that blocks on a run loop.
///
/// More information is available in the Apple documentation at
/// https://developer.apple.com/documentation/corefoundation/cfrunloopsource-rhr.
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

        // Use the C-unwind ABI and the exact pointer types expected by
        // CFRunLoopSourceContext.
        //
        // The callbacks are unsafe and may be called from C code. Each callback
        // receives the `info` pointer we stored (a *mut Handler<F>). We cast it
        // back and operate on it. The retain/release callbacks mutate the
        // `ref_count` and free the box when it reaches zero.
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

/// A repeating timer attached to the current [`CFRunLoop`].
///
/// Exists because rini's executor is CFRunLoop-based rather than a Tokio runtime, so
/// `tokio::time::sleep` panics with "there is no reactor running". Anything that needs a periodic
/// wakeup on an actor thread has to come from the run loop itself.
///
/// The timer is invalidated and removed on drop, so an animation that ends stops costing wakeups.
pub struct RepeatingTimer {
    timer: CFRetained<CFRunLoopTimer>,
    run_loop: CFRetained<CFRunLoop>,
    /// Owns the boxed callback for as long as the timer can fire.
    ///
    /// Type-erased to `dyn Fn()` rather than generic, so `Drop` can free it correctly without the
    /// struct carrying a type parameter. Freeing this as the wrong type would skip the closure's
    /// own destructor, which matters as soon as it captures something like a channel sender.
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

        Some(Self { timer, run_loop, handler: handler_ptr })
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
