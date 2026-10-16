use std::ffi::c_void;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use dispatchr::queue::Unmanaged;
use dispatchr::semaphore::Managed;
use dispatchr::time::Time;
use futures_task::{ArcWake, waker};

unsafe extern "C" {
    fn dispatch_after_f(
        when: Time,
        queue: *const Unmanaged,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

pub trait DispatchExt {
    /// `work` run with `context` on this queue at `when`.
    ///
    /// # Safety
    /// `context` must stay valid for `work` until the queue runs it, and be usable from the
    /// queue's thread.
    unsafe fn after_f(&self, when: Time, context: *mut c_void, work: extern "C" fn(*mut c_void));
    /// `work` run with `context` on this queue at `when`.
    fn after_f_s<T: 'static>(&self, when: Time, context: T, work: fn(T));
}

impl DispatchExt for Unmanaged {
    unsafe fn after_f(&self, when: Time, context: *mut c_void, work: extern "C" fn(*mut c_void)) {
        // SAFETY: `self` is a live queue, which the call retains until `work` has run; the caller
        // keeps `context` valid for `work` and usable on the queue's thread.
        unsafe { dispatch_after_f(when, self, context, work) }
    }

    fn after_f_s<T: 'static>(&self, when: Time, context: T, work: fn(T)) {
        extern "C" fn trampoline<T>(ctx: *mut c_void) {
            // SAFETY: `ctx` is the box `after_f_s` leaked for this one run of `trampoline`.
            let ctx = unsafe { Box::from_raw(ctx as *mut (T, fn(T))) };
            let (context, work) = *ctx;
            work(context);
        }
        let ctx = Box::into_raw(Box::new((context, work))) as *mut c_void;
        // SAFETY: `ctx` owns a `'static` value, freed by the one `trampoline` that runs it. That it
        // may move to the queue's thread the bound does not say: every caller schedules on the
        // main queue, one of them a pointer to a main-thread object.
        unsafe { self.after_f(when, ctx, trampoline::<T>) };
    }
}

pub fn block_on<T: 'static>(
    mut fut: r#continue::Future<T>,
    timeout: Duration,
) -> Result<T, String> {
    struct GcdWaker {
        sem: Managed,
    }
    impl ArcWake for GcdWaker {
        fn wake_by_ref(this: &Arc<Self>) { this.sem.signal(); }
    }

    let sem = Managed::new(0);
    let waker: Waker = waker(Arc::new(GcdWaker { sem: sem.clone() }));
    let mut cx = Context::from_waker(&waker);

    let deadline = Instant::now() + timeout;

    loop {
        match Pin::new(&mut fut).poll(&mut cx) {
            Poll::Ready(v) => return Ok(v),
            Poll::Pending => {
                let now = Instant::now();
                if now >= deadline {
                    return Err("Timeout".into());
                }

                let remaining = deadline - now;
                let ns = i64::try_from(remaining.as_nanos()).unwrap_or(i64::MAX);
                let t = Time::NOW.new_after(ns);

                if sem.wait(t) != 0 {
                    return Err("Timeout".into());
                }
            }
        }
    }
}
