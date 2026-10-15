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
    /// # Safety
    /// `context` must stay valid for `work` until the queue runs it.
    unsafe fn after_f(&self, when: Time, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn after_f_s<T>(&self, when: Time, context: T, work: fn(T));
}

impl DispatchExt for Unmanaged {
    unsafe fn after_f(&self, when: Time, context: *mut c_void, work: extern "C" fn(*mut c_void)) {
        unsafe { dispatch_after_f(when, self, context, work) }
    }

    fn after_f_s<T>(&self, when: Time, context: T, work: fn(T)) {
        extern "C" fn trampoline<T>(ctx: *mut c_void) {
            let ctx = unsafe { Box::from_raw(ctx as *mut (T, fn(T))) };
            let (context, work) = *ctx;
            work(context);
        }
        let ctx = Box::into_raw(Box::new((context, work))) as *mut c_void;
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
