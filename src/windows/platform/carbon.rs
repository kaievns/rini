#![allow(clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::ptr;

pub type OSStatus = i32;
pub type EventHandlerCallRef = *mut c_void;
pub type EventHandlerRef = *mut c_void;
pub type EventRef = *mut c_void;
pub type EventTargetRef = *mut c_void;

const NO_ERR: OSStatus = 0;

#[allow(non_snake_case)]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EventType {
    pub eventClass: u32,
    pub eventKind: u32,
}

pub const fn event_type(event_class: u32, event_kind: u32) -> EventType {
    EventType {
        eventClass: event_class,
        eventKind: event_kind,
    }
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;

    fn InstallEventHandler(
        target: EventTargetRef,
        handler: Option<
            unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus,
        >,
        num_types: u32,
        type_list: *const EventType,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;

    fn RemoveEventHandler(handler: EventHandlerRef) -> OSStatus;

    fn GetEventKind(event: EventRef) -> u32;

    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: u32,
        actual_size: *mut u32,
        data: *mut c_void,
    ) -> OSStatus;
}

#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub raw: EventRef,
    pub kind: u32,
}

impl Event {
    pub unsafe fn parameter<T: Copy>(&self, name: u32, desired_type: u32) -> Option<T> {
        let mut out: T = unsafe { std::mem::zeroed() };
        let status = unsafe {
            GetEventParameter(
                self.raw,
                name,
                desired_type,
                std::ptr::null_mut(),
                std::mem::size_of::<T>() as u32,
                std::ptr::null_mut(),
                &mut out as *mut T as *mut c_void,
            )
        };
        if status == NO_ERR { Some(out) } else { None }
    }
}

struct CallbackCtx {
    callback: Box<dyn FnMut(Event) -> OSStatus + Send>,
}

unsafe extern "C" fn trampoline(
    _call_ref: EventHandlerCallRef,
    event: EventRef,
    user_data: *mut c_void,
) -> OSStatus {
    let ctx = unsafe { &mut *(user_data as *mut CallbackCtx) };
    let ev = Event {
        raw: event,
        kind: unsafe { GetEventKind(event) },
    };
    (ctx.callback)(ev)
}

pub struct CarbonListener {
    handler_ref: EventHandlerRef,
    ctx: *mut c_void,
}

unsafe impl Send for CarbonListener {}
unsafe impl Sync for CarbonListener {}

impl std::fmt::Debug for CarbonListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CarbonListener")
            .field("handler_ref", &self.handler_ref)
            .finish()
    }
}

impl CarbonListener {
    pub fn application<F>(types: &[EventType], callback: F) -> Result<Self, String>
    where F: FnMut(Event) -> OSStatus + Send + 'static {
        let target = unsafe { GetApplicationEventTarget() };
        if target.is_null() {
            return Err("GetApplicationEventTarget returned null".to_string());
        }

        let ctx =
            Box::into_raw(Box::new(CallbackCtx { callback: Box::new(callback) })) as *mut c_void;

        let mut handler_ref: EventHandlerRef = ptr::null_mut();
        let status = unsafe {
            InstallEventHandler(
                target,
                Some(trampoline),
                types.len() as u32,
                types.as_ptr(),
                ctx,
                &mut handler_ref,
            )
        };

        if status != NO_ERR || handler_ref.is_null() {
            unsafe { drop(Box::from_raw(ctx as *mut CallbackCtx)) };
            return Err(format!("InstallEventHandler failed: status={status}"));
        }

        Ok(Self { handler_ref, ctx })
    }

    pub fn remove(&mut self) -> Result<(), String> {
        if !self.handler_ref.is_null() {
            let status = unsafe { RemoveEventHandler(self.handler_ref) };
            if status != NO_ERR {
                return Err(format!("RemoveEventHandler failed: status={status}"));
            }
            self.handler_ref = ptr::null_mut();
        }
        if !self.ctx.is_null() {
            unsafe { drop(Box::from_raw(self.ctx as *mut CallbackCtx)) };
            self.ctx = ptr::null_mut();
        }
        Ok(())
    }
}

impl Drop for CarbonListener {
    fn drop(&mut self) { let _ = self.remove(); }
}
