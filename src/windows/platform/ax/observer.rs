use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::{self, NonNull};
use std::rc::Rc;

use objc2_application_services::{AXError, AXObserver, AXUIElement as RawAXUIElement};
use objc2_core_foundation::{
    CFRetained, CFRunLoop, CFRunLoopMode, CFRunLoopSource, CFString, kCFRunLoopCommonModes,
};
use rini_core::ids::pid_t;

use crate::windows::platform::ax::element::{AXUIElement, Error as AxError};

/// An observer for accessibility events.
///
/// Its drop invalidates its run-loop source before it frees the contexts its callbacks read, so
/// it can drop at any time on its own thread, from inside one of its own callbacks included.
pub struct Observer {
    callback: Callback,
    observer: CFRetained<AXObserver>,
    subscription_ctx: RefCell<HashMap<NotificationKey, Rc<SubscriptionContext>>>,
}

static_assertions::assert_not_impl_any!(Observer: Send);

/// Helper type for building an [`Observer`].
pub struct ObserverBuilder<F>(CFRetained<AXObserver>, PhantomData<F>);

type NotificationKey = (NonNull<RawAXUIElement>, &'static str);

struct SubscriptionContext {
    callback: Callback,
    data: Cell<usize>,
    notification: &'static str,
}

enum Callback {
    Data(Rc<dyn Fn(AXUIElement, usize)>),
    Notification(Rc<dyn Fn(AXUIElement, &'static str)>),
}

impl Observer {
    /// Creates a new observer for an app, given its `pid`.
    ///
    /// Note that you must call [`ObserverBuilder::install`] on the result of
    /// this function and supply a callback for the observer to have any effect.
    #[allow(
        clippy::new_ret_no_self,
        reason = "an observer exists once installed with its callback"
    )]
    pub fn new<F: Fn(AXUIElement, usize) + 'static>(
        pid: pid_t,
    ) -> Result<ObserverBuilder<F>, AxError> {
        Self::create(pid, internal_callback)
    }

    /// `AXObserverCreate` with whichever trampoline the callback shape needs. The only difference
    /// between the two constructors above and below.
    fn create<F>(
        pid: pid_t,
        trampoline: unsafe extern "C-unwind" fn(
            NonNull<AXObserver>,
            NonNull<RawAXUIElement>,
            NonNull<CFString>,
            *mut c_void,
        ),
    ) -> Result<ObserverBuilder<F>, AxError> {
        let mut observer_ptr: *mut AXObserver = ptr::null_mut();
        // SAFETY: `observer_ptr` is a local the call writes, and `trampoline` has the callback's
        // signature, checked by the type above.
        let status = unsafe {
            AXObserver::create(
                pid,
                Some(trampoline),
                NonNull::new(&mut observer_ptr as *mut *mut AXObserver).expect("nonnull pointer"),
            )
        };
        make_result(status)?;
        // SAFETY: a Create call that succeeded, so the observer is non-null and +1 to us.
        let observer = unsafe {
            CFRetained::from_raw(NonNull::new(observer_ptr).expect("observer must be non-null"))
        };
        Ok(ObserverBuilder(observer, PhantomData))
    }

    /// Creates an observer whose callback receives the notification name.
    ///
    /// This is useful for APIs such as Dock's expose notifications where the
    /// notification itself is the event payload rather than an opaque callback
    /// value supplied at registration time.
    pub fn new_with_notification<F: Fn(AXUIElement, &'static str) + 'static>(
        pid: pid_t,
    ) -> Result<ObserverBuilder<F>, AxError> {
        Self::create(pid, internal_callback_with_notification)
    }
}

impl<F: Fn(AXUIElement, usize) + 'static> ObserverBuilder<F> {
    /// Installs the observer with the supplied callback into the current
    /// thread's run loop.
    pub fn install(self, callback: F) -> Observer {
        self.install_inner(Callback::Data(Rc::new(callback)))
    }
}

impl<F: Fn(AXUIElement, &'static str) + 'static> ObserverBuilder<F> {
    /// Installs a callback that receives the registered notification name.
    pub fn install_with_notification(self, callback: F) -> Observer {
        self.install_inner(Callback::Notification(Rc::new(callback)))
    }
}

impl<F> ObserverBuilder<F> {
    fn install_inner(self, callback: Callback) -> Observer {
        let run_loop_source = source_of(&self.0);
        if let Some(run_loop) = CFRunLoop::current() {
            // SAFETY: a CoreFoundation constant, set when the framework loads and never written.
            let mode: &CFRunLoopMode =
                unsafe { kCFRunLoopCommonModes.expect("kCFRunLoopCommonModes") };
            run_loop.add_source(Some(run_loop_source.as_ref()), Some(mode));
        }
        Observer {
            callback,
            observer: self.0,
            subscription_ctx: RefCell::new(HashMap::new()),
        }
    }
}

/// The one run-loop source an observer delivers its notifications through.
fn source_of(observer: &AXObserver) -> CFRetained<CFRunLoopSource> {
    // SAFETY: `observer` is a live AXObserver for the call, and the binding retains the source
    // AXObserverGetRunLoopSource answers under the Get rule.
    unsafe { observer.run_loop_source() }
}

impl Drop for Observer {
    fn drop(&mut self) { source_of(&self.observer).invalidate(); }
}

impl Clone for Callback {
    fn clone(&self) -> Self {
        match self {
            Self::Data(callback) => Self::Data(Rc::clone(callback)),
            Self::Notification(callback) => Self::Notification(Rc::clone(callback)),
        }
    }
}

impl Observer {
    pub fn add_notification(
        &self,
        elem: &AXUIElement,
        notification: &'static str,
    ) -> Result<(), AxError> {
        self.add_notification_with_data(elem, notification, 0)
    }

    pub fn add_notification_with_data(
        &self,
        elem: &AXUIElement,
        notification: &'static str,
        data: usize,
    ) -> Result<(), AxError> {
        let callback_ctx = self.subscription_context(elem, notification, data);
        self.add_notification_inner(elem, notification, callback_ctx)
    }

    fn add_notification_inner(
        &self,
        elem: &AXUIElement,
        notification: &'static str,
        callback_ctx: Rc<SubscriptionContext>,
    ) -> Result<(), AxError> {
        let notification_cf = CFString::from_static_str(notification);
        let observer: &AXObserver = &self.observer;
        let callback_data = Rc::as_ptr(&callback_ctx).cast_mut().cast();
        // SAFETY: `elem` and the name are live for the call, and the refcon is a context
        // `subscription_ctx` keeps until the observer's drop has invalidated its source.
        make_result(unsafe {
            observer.add_notification(
                elem.as_concrete_TypeRef(),
                notification_cf.as_ref(),
                callback_data,
            )
        })
    }

    fn subscription_context(
        &self,
        elem: &AXUIElement,
        notification: &'static str,
        data: usize,
    ) -> Rc<SubscriptionContext> {
        let key = (elem.raw_ptr(), notification);
        let mut subscription_ctx = self.subscription_ctx.borrow_mut();
        let ctx = subscription_ctx.entry(key).or_insert_with(|| {
            Rc::new(SubscriptionContext {
                callback: self.callback.clone(),
                data: Cell::new(data),
                notification,
            })
        });
        ctx.data.set(data);
        Rc::clone(ctx)
    }

    pub fn remove_notification(
        &self,
        elem: &AXUIElement,
        notification: &'static str,
    ) -> Result<(), AxError> {
        let notification_cf = CFString::from_static_str(notification);
        let observer: &AXObserver = &self.observer;
        // SAFETY: `elem` and the name are live for the call.
        let result = make_result(unsafe {
            observer.remove_notification(elem.as_concrete_TypeRef(), notification_cf.as_ref())
        });
        // AXObserverRemoveNotification can succeed while a notification
        // carrying this refcon is already queued on the run loop. Keep the
        // context alive until the observer is dropped instead of freeing
        // memory that CoreFoundation may still pass back to us.
        result
    }
}

unsafe extern "C-unwind" fn internal_callback(
    _observer: NonNull<AXObserver>,
    elem: NonNull<RawAXUIElement>,
    _notif: NonNull<CFString>,
    data: *mut c_void,
) {
    // SAFETY: AX calls this trampoline only through the source of the observer it was created
    // for, with a refcon `add_notification_inner` registered on it.
    let Some(ctx) = (unsafe { context_for_call(data) }) else {
        return;
    };
    // SAFETY: AX hands the callback a live element under the Get rule.
    let elem = unsafe { AXUIElement::from_get_rule(elem.as_ptr()) };
    if let Callback::Data(callback) = &ctx.callback {
        callback(elem, ctx.data.get());
    }
}

unsafe extern "C-unwind" fn internal_callback_with_notification(
    _observer: NonNull<AXObserver>,
    elem: NonNull<RawAXUIElement>,
    _notif: NonNull<CFString>,
    data: *mut c_void,
) {
    // SAFETY: AX calls this trampoline only through the source of the observer it was created
    // for, with a refcon `add_notification_inner` registered on it.
    let Some(ctx) = (unsafe { context_for_call(data) }) else {
        return;
    };
    // SAFETY: AX hands the callback a live element under the Get rule.
    let elem = unsafe { AXUIElement::from_get_rule(elem.as_ptr()) };
    if let Callback::Notification(callback) = &ctx.callback {
        callback(elem, ctx.notification);
    }
}

/// Takes a strong count on the context behind `data` for the length of one callout, so a callback
/// that drops its own [`Observer`] does not free the context it runs on.
///
/// # Safety
///
/// `data` is null or a refcon `add_notification_inner` registered, called from the run-loop
/// source of the observer that holds it.
unsafe fn context_for_call(data: *mut c_void) -> Option<Rc<SubscriptionContext>> {
    let data = NonNull::new(data)?.cast::<SubscriptionContext>().as_ptr().cast_const();
    // SAFETY: `data` came from `Rc::as_ptr` on a context the observer's map holds until the
    // observer drops, and that drop invalidates this source first, so no callout starts after it
    // and the count is at least one here.
    unsafe {
        Rc::increment_strong_count(data);
        Some(Rc::from_raw(data))
    }
}

fn make_result(err: AXError) -> Result<(), AxError> {
    if err == AXError::Success {
        Ok(())
    } else {
        Err(AxError::Ax(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A notification added again on the same element keeps the context its refcon points at and
    /// takes the new data, so a notification already queued with that refcon still finds it.
    #[test]
    fn adding_a_notification_again_keeps_its_context_and_takes_the_new_data() {
        let pid = std::process::id() as pid_t;
        let observer = Observer::new(pid).expect("an observer of this process").install(|_, _| {});
        let app = AXUIElement::application(pid);

        let first = observer.subscription_context(&app, "AXFocusedWindowChanged", 1);
        let again = observer.subscription_context(&app, "AXFocusedWindowChanged", 2);
        let other = observer.subscription_context(&app, "AXWindowCreated", 3);

        assert!(Rc::ptr_eq(&first, &again), "the same context");
        assert_eq!(first.data.get(), 2, "with the new data");
        assert!(!Rc::ptr_eq(&first, &other), "another notification has its own");
        assert_eq!(other.data.get(), 3);
    }

    /// A dropped observer leaves no run-loop source that could call out with the contexts it
    /// freed.
    #[test]
    fn dropping_an_observer_invalidates_its_run_loop_source() {
        let pid = std::process::id() as pid_t;
        let observer = Observer::new(pid).expect("an observer of this process").install(|_, _| {});
        let source = source_of(&observer.observer);
        assert!(source.is_valid(), "installed");

        drop(observer);

        assert!(!source.is_valid(), "invalidated by the drop");
    }

    /// A callout holds its own count on the context, so a callback that drops its observer
    /// still reads a live context until it returns.
    #[test]
    fn a_callout_keeps_its_context_alive_past_its_observer() {
        let pid = std::process::id() as pid_t;
        let observer = Observer::new(pid).expect("an observer of this process").install(|_, _| {});
        let app = AXUIElement::application(pid);
        let ctx = observer.subscription_context(&app, "AXFocusedWindowChanged", 7);
        let refcon = Rc::as_ptr(&ctx).cast_mut().cast::<c_void>();
        drop(ctx);

        // SAFETY: `refcon` is a context the live observer's map holds.
        let held = unsafe { context_for_call(refcon) }.expect("a non-null refcon");
        drop(observer);

        assert_eq!(Rc::strong_count(&held), 1, "the callout's own count");
        assert_eq!(held.data.get(), 7);
    }
}
