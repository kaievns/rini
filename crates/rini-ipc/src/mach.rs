#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case, unsafe_op_in_unsafe_fn, clippy::missing_safety_doc)]
//! rini's Mach service: the bootstrap name, the receive port on a CFRunLoop, request dispatch and
//! replies. Built on `rini_mach_sys`.
use core::mem::{size_of, zeroed};
use core::ptr::{copy_nonoverlapping, null, null_mut};
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};

use rini_mach_sys::*;
use tracing::{debug, error, info};

const MACH_BS_NAME_FMT_PREFIX: &str = "git.";
static G_NAME: &str = "kaievns.rini";
fn bs_name() -> CString {
    if let Ok(name) = std::env::var("RINI_BS_NAME") {
        return CString::new(name).unwrap();
    }
    CString::new(format!("{}{}", MACH_BS_NAME_FMT_PREFIX, G_NAME)).unwrap()
}
pub fn is_mach_server_registered() -> bool {
    let bs_name = bs_name();
    unsafe { mach_get_bs_port(&bs_name) != 0 }
}
type CFIndex = isize;
type CFAllocatorRef = *const c_void;
type CFStringRef = *const c_void;
type CFMachPortRef = *const c_void;
type CFRunLoopSourceRef = *const c_void;
type CFRunLoopRef = *const c_void;
#[repr(C)]
struct CFMachPortContext {
    version: CFIndex,
    info: *mut c_void,
    retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<extern "C" fn(*const c_void)>,
    #[allow(non_snake_case)]
    copyDescription: Option<extern "C" fn(*const c_void) -> CFStringRef>,
}
type CFMachPortCallBack =
    Option<extern "C" fn(port: CFMachPortRef, msg: *mut c_void, size: CFIndex, info: *mut c_void)>;
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFMachPortCreateWithPort(
        allocator: CFAllocatorRef,
        portNum: mach_port_t,
        callout: CFMachPortCallBack,
        context: *const CFMachPortContext,
        shouldFreeInfo: u8,
    ) -> CFMachPortRef;

    fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: c_int,
    ) -> CFRunLoopSourceRef;

    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopGetMain() -> CFRunLoopRef;
    fn CFRunLoopRun();

    fn CFRelease(obj: *const c_void);

    static kCFRunLoopDefaultMode: CFStringRef;
}
pub type mach_handler = unsafe extern "C" fn(
    context: *mut c_void,
    message: *mut c_char,
    len: u32,
    original_msg: *mut mach_msg_header_t,
);
#[repr(C)]
pub struct mach_server {
    is_running: bool,
    task: mach_port_name_t,
    port: mach_port_t,
    bs_port: mach_port_t,
    handler: Option<mach_handler>,
    context: *mut c_void,
}
impl Default for mach_server {
    fn default() -> Self {
        Self {
            is_running: false,
            task: 0,
            port: 0,
            bs_port: 0,
            handler: None,
            context: null_mut(),
        }
    }
}
extern "C" fn mach_message_callback(
    _port: CFMachPortRef,
    message: *mut c_void,
    _size: CFIndex,
    context: *mut c_void,
) {
    unsafe {
        if context.is_null() || message.is_null() {
            return;
        }
        let mach_server = &mut *(context as *mut mach_server);
        let header_val = core::ptr::read_unaligned(message as *const mach_msg_header_t);
        let header_ptr = &header_val as *const mach_msg_header_t as *mut mach_msg_header_t;
        if header_val.msgh_remote_port == 0 {
            return;
        }

        let mut payload_ptr: *mut c_char = null_mut();
        let mut payload_len: u32 = 0;

        if (header_val.msgh_bits & MACH_MSGH_BITS_COMPLEX) != 0 {
            let body_ptr = (message as *const u8).add(size_of::<mach_msg_header_t>())
                as *const mach_msg_body_t;
            let body_val = core::ptr::read_unaligned(body_ptr);
            if body_val.msgh_descriptor_count >= 1 {
                let desc_ptr = ((body_ptr as usize + size_of::<mach_msg_body_t>() + 7) & !7)
                    as *const mach_msg_ool_descriptor_t;
                let desc_val = core::ptr::read_unaligned(desc_ptr);
                payload_ptr = desc_val.address as *mut c_char;
                payload_len = desc_val.size;
                if payload_ptr.is_null() || payload_len == 0 {
                    payload_len =
                        header_val.msgh_size.saturating_sub(size_of::<mach_msg_header_t>() as u32);
                    payload_ptr =
                        (message as *mut u8).add(size_of::<mach_msg_header_t>()) as *mut c_char;
                }
            }
        } else {
            payload_len =
                header_val.msgh_size.saturating_sub(size_of::<mach_msg_header_t>() as u32);
            payload_ptr = (message as *mut u8).add(size_of::<mach_msg_header_t>()) as *mut c_char;
        }

        if let Some(handler) = mach_server.handler {
            handler(mach_server.context, payload_ptr, payload_len, header_ptr);
        }

        let _ = mach_msg_destroy(message as *mut mach_msg_header_t);
    }
}
pub unsafe fn mach_server_begin(
    mach_server: &mut mach_server,
    context: *mut c_void,
    handler: mach_handler,
) -> bool {
    mach_server.task = mach_task_self();

    if task_get_special_port(mach_server.task, TASK_BOOTSTRAP_PORT, &mut mach_server.bs_port)
        != KERN_SUCCESS
    {
        error!("mach_server_begin: task_get_special_port failed");
        return false;
    }

    let service_name = bs_name();

    let ar = mach_port_allocate(mach_server.task, MACH_PORT_RIGHT_RECEIVE, &mut mach_server.port);
    if ar != KERN_SUCCESS {
        error!("mach_server_begin: mach_port_allocate failed (kr={})", ar);
        return false;
    }

    let ir = mach_port_insert_right(
        mach_server.task,
        mach_server.port,
        mach_server.port,
        MACH_MSG_TYPE_MAKE_SEND as c_int,
    );
    if ir != KERN_SUCCESS {
        error!(
            "mach_server_begin: mach_port_insert_right (MAKE_SEND) failed (kr={})",
            ir
        );
        return false;
    }

    let rr = bootstrap_register(mach_server.bs_port, service_name.as_ptr(), mach_server.port);
    if rr != KERN_SUCCESS {
        match rr {
            BOOTSTRAP_NAME_IN_USE => error!(
                "mach_server_begin: bootstrap_register: name in use: {}.",
                service_name.to_string_lossy()
            ),
            BOOTSTRAP_NOT_PRIVILEGED => error!(
                "mach_server_begin: bootstrap_register: not privileged for domain (kr={}).",
                rr
            ),
            _ => error!(
                "mach_server_begin: bootstrap_register failed (kr={}) for {}",
                rr,
                service_name.to_string_lossy()
            ),
        }
        return false;
    }

    let limits = mach_port_limits {
        mpl_qlimit: MACH_PORT_QLIMIT_LARGE,
    };
    let _ = mach_port_set_attributes(
        mach_server.task,
        mach_server.port,
        MACH_PORT_LIMITS_INFO,
        &limits as *const _ as *const c_void,
        MACH_PORT_LIMITS_INFO_COUNT,
    );

    mach_server.handler = Some(handler);
    mach_server.context = context;
    mach_server.is_running = true;

    let cf_context = CFMachPortContext {
        version: 0,
        info: mach_server as *mut _ as *mut c_void,
        retain: None,
        release: None,
        copyDescription: None,
    };
    let cf_mach_port = CFMachPortCreateWithPort(
        null(),
        mach_server.port,
        Some(mach_message_callback),
        &cf_context,
        0,
    );
    if cf_mach_port.is_null() {
        error!(
            "mach_server_begin: CFMachPortCreateWithPort returned null (port={})",
            mach_server.port
        );
        return false;
    }
    let source = CFMachPortCreateRunLoopSource(null(), cf_mach_port, 0);
    if source.is_null() {
        error!("mach_server_begin: CFMachPortCreateRunLoopSource returned null");
        CFRelease(cf_mach_port);
        return false;
    }
    CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopDefaultMode);
    CFRelease(source);
    CFRelease(cf_mach_port);

    info!(
        "mach_server_begin: registered '{}' in current bootstrap domain (port={}, bs_port={})",
        bs_name().to_string_lossy(),
        mach_server.port,
        mach_server.bs_port
    );

    true
}
pub unsafe fn send_mach_reply(
    original_msg: *mut mach_msg_header_t,
    response_data: *const c_char,
    response_len: u32,
) -> bool {
    if original_msg.is_null() || response_data.is_null() || response_len > MAX_MESSAGE_SIZE {
        error!(
            "send_mach_reply: invalid args original_msg={:?} response_data={:?} response_len={}",
            original_msg, response_data, response_len
        );
        return false;
    }

    let task = mach_task_self();
    let mut remote_port_type: u32 = 0;
    let mut local_port_type: u32 = 0;

    if (*original_msg).msgh_remote_port != 0 {
        let _ = mach_port_type(task, (*original_msg).msgh_remote_port, &mut remote_port_type);
    }
    if (*original_msg).msgh_local_port != 0 {
        let _ = mach_port_type(task, (*original_msg).msgh_local_port, &mut local_port_type);
    }

    let reply_port = (*original_msg).msgh_remote_port;
    let reply_descriptor = MACH_MSG_TYPE_COPY_SEND;
    if reply_port == 0 {
        error!(
            "send_mach_reply: no available send right (remote_port_type={} local_port_type={} remote_port={} local_port={})",
            remote_port_type,
            local_port_type,
            (*original_msg).msgh_remote_port,
            (*original_msg).msgh_local_port
        );
        return false;
    };

    let mut reply: mach_message_t = zeroed();

    let aligned_len = (response_len + 3) & !3;
    let total_size = (size_of::<mach_msg_header_t>() as u32) + aligned_len;

    reply.header.msgh_bits = MACH_MSGH_BITS(reply_descriptor as u32, 0);
    reply.header.msgh_size = total_size;
    reply.header.msgh_remote_port = reply_port;
    reply.header.msgh_local_port = 0;
    reply.header.msgh_voucher_port = 0;
    reply.header.msgh_id = (*original_msg).msgh_id;

    copy_nonoverlapping(
        response_data as *const u8,
        reply.data.as_mut_ptr() as *mut u8,
        response_len as usize,
    );
    if aligned_len > response_len {
        let pad = (aligned_len - response_len) as usize;
        let dst = reply.data.as_mut_ptr().add(response_len as usize);
        core::ptr::write_bytes(dst, 0, pad);
    }

    let result = mach_msg(
        &mut reply.header,
        MACH_SEND_MSG,
        reply.header.msgh_size,
        0,
        0,
        MACH_MSG_TIMEOUT_NONE,
        0,
    );

    if result != MACH_MSG_SUCCESS {
        let mut _port_type: u32 = 0;
        let _ = mach_port_type(task, reply_port, &mut _port_type);
        error!(
            "send_mach_reply: mach_msg failed result={} reply_port={} port_type={} descriptor={} remote_port_type={} local_port_type={} original_remote={} original_local={}",
            result,
            reply_port,
            _port_type,
            reply_descriptor,
            remote_port_type,
            local_port_type,
            MACH_MSGH_BITS_REMOTE((*original_msg).msgh_bits),
            MACH_MSGH_BITS_LOCAL((*original_msg).msgh_bits)
        );
        return false;
    }

    true
}
#[allow(static_mut_refs)]
pub unsafe fn mach_server_run(context: *mut c_void, handler: mach_handler) -> bool {
    static mut SERVER: mach_server = mach_server {
        is_running: false,
        task: 0,
        port: 0,
        bs_port: 0,
        handler: None,
        context: null_mut(),
    };

    debug!(
        "mach_server_run: initial state task={} port={} bs_port={} handler_set={} context_ptr={:?}",
        SERVER.task,
        SERVER.port,
        SERVER.bs_port,
        SERVER.handler.is_some(),
        SERVER.context
    );

    if !mach_server_begin(&mut SERVER, context, handler) {
        error!("mach_server_run: mach_server_begin failed, aborting run loop");
        return false;
    }

    debug!(
        "mach_server_run: ports ready (task={}, port={}, bs_port={})",
        SERVER.task, SERVER.port, SERVER.bs_port
    );
    CFRunLoopRun();
    true
}
