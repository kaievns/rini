#![allow(
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case,
    unsafe_op_in_unsafe_fn,
    clippy::missing_safety_doc
)]
//! A window's sub-level, asked of SkyLight's private connection server port over Mach. The port
//! comes from a Mach message to `SLSServerPort`; there is no public API for either.
use core::mem::{size_of, zeroed};
use std::os::raw::c_int;

use rini_mach_sys::*;
use tracing::debug;

const CONNECTION_SERVER_PORT_MSG_ID: i32 = 0x7468;
const CONNECTION_SERVER_PORT_RECV_SIZE: u32 = 0x48;
#[repr(C, packed(2))]
struct window_sub_level_info_t {
    pub header: mach_msg_header_t,
    pub NDR_record: ndr_record_t,
}
#[repr(C, packed(2))]
struct window_sub_level_payload_t {
    pub wid: i32,
}
#[repr(C, packed(2))]
struct window_sub_level_response_t {
    pub sub_level: i32,
    pub padding: i64,
}
#[repr(C, packed(2))]
struct get_window_sub_level_message_t {
    pub info: window_sub_level_info_t,
    pub payload: window_sub_level_payload_t,
    pub response: window_sub_level_response_t,
}
const _: [(); 32] = [(); size_of::<window_sub_level_info_t>()];
const _: [(); 4] = [(); size_of::<window_sub_level_payload_t>()];
const _: [(); 12] = [(); size_of::<window_sub_level_response_t>()];
const _: [(); 48] = [(); size_of::<get_window_sub_level_message_t>()];
#[repr(C, packed(2))]
struct create_connection_server_port_message_t {
    header: mach_msg_header_t,
    magic1: i32,
    server_port: mach_port_t,
    padding1: i32,
    magic2: i32,
    padding2: i64,
    magic3: i64,
    padding3: i64,
    bundle_size: i32,
    bundle_name: [u8; 0x8],
    connection_count: i32,
    launched_before_login: i32,
    is_ui_process: i32,
    context_id: i32,
}
const _: [(); 92] = [(); size_of::<create_connection_server_port_message_t>()];
static mut g_server_port: mach_port_t = MACH_PORT_NULL;
static mut window_sub_level_error_logged: bool = false;
static mut window_sub_level_invalid_msg_logged: bool = false;
pub fn init_window_sub_level_server_port() {
    unsafe {
        g_server_port = create_connection_server_port();
    }
}
unsafe fn create_connection_server_port() -> mach_port_t {
    let mut msg = aligned_message_t(zeroed::<create_connection_server_port_message_t>());
    msg.0.header.msgh_bits = MACH_MSGH_BITS_SET(
        MACH_MSG_TYPE_COPY_SEND,
        MACH_MSG_TYPE_MAKE_SEND_ONCE,
        0,
        MACH_MSGH_BITS_REMOTE_MASK | MACH_MSGH_BITS_COMPLEX,
    );
    msg.0.header.msgh_local_port = mig_get_special_reply_port();
    msg.0.header.msgh_remote_port = rini_skylight_sys::SLSServerPort(core::ptr::null_mut());
    msg.0.header.msgh_size = size_of::<create_connection_server_port_message_t>() as u32;
    msg.0.header.msgh_id = CONNECTION_SERVER_PORT_MSG_ID;

    msg.0.magic1 = 2;
    msg.0.magic2 = 0x110000;
    msg.0.magic3 = 0x110000;

    let bundle = b"com.kaievns.rini";
    let copy_len = bundle.len().min(msg.0.bundle_name.len());
    msg.0.bundle_name[..copy_len].copy_from_slice(&bundle[..copy_len]);
    if bundle.len() > copy_len {
        debug!(
            bundle_len = bundle.len(),
            max_len = msg.0.bundle_name.len(),
            "Truncating connection server bundle name"
        );
    }
    msg.0.bundle_size = msg.0.bundle_name.len() as i32;

    let header_ptr = core::ptr::addr_of_mut!(msg.0.header);
    let local_port = core::ptr::read_unaligned(core::ptr::addr_of!(msg.0.header.msgh_local_port));
    let _ = mach_msg(
        header_ptr,
        MACH_SEND_MSG
            | MACH_SEND_SYNC_OVERRIDE
            | MACH_SEND_PROPAGATE_QOS
            | MACH_RCV_MSG
            | MACH_RCV_SYNC_WAIT,
        size_of::<create_connection_server_port_message_t>() as u32,
        CONNECTION_SERVER_PORT_RECV_SIZE,
        local_port,
        0,
        0,
    );

    core::ptr::read_unaligned(core::ptr::addr_of!(msg.0.server_port))
}
const WINDOW_SUB_LEVEL_REQUEST_PRE_TAHOE: i32 = 0x73C3;
const WINDOW_SUB_LEVEL_REQUEST_POST_TAHOE: i32 = 0x76E3;
const WINDOW_SUB_LEVEL_RESPONSE_PRE_TAHOE: i32 = 0x7427;
const WINDOW_SUB_LEVEL_RESPONSE_POST_TAHOE: i32 = 0x7747;
pub unsafe fn mach_get_window_sub_level(wid: u32) -> c_int {
    if g_server_port == MACH_PORT_NULL {
        g_server_port = create_connection_server_port();
        if g_server_port == MACH_PORT_NULL {
            return 0;
        }
    }

    let mut request = WINDOW_SUB_LEVEL_REQUEST_PRE_TAHOE;
    let mut response = WINDOW_SUB_LEVEL_RESPONSE_PRE_TAHOE;
    if objc2::available!(macos = 26.0) {
        request = WINDOW_SUB_LEVEL_REQUEST_POST_TAHOE;
        response = WINDOW_SUB_LEVEL_RESPONSE_POST_TAHOE;
    }

    let mut msg = aligned_message_t(zeroed::<get_window_sub_level_message_t>());
    msg.0.info.NDR_record = NDR_record;
    msg.0.info.header.msgh_remote_port = g_server_port;
    msg.0.info.header.msgh_local_port = mig_get_special_reply_port();
    msg.0.info.header.msgh_bits = MACH_MSGH_BITS_SET(
        MACH_MSG_TYPE_COPY_SEND,
        MACH_MSG_TYPE_MAKE_SEND_ONCE,
        0,
        MACH_MSGH_BITS_REMOTE_MASK,
    );
    msg.0.info.header.msgh_id = request;
    msg.0.payload.wid = wid as i32;

    let send_size =
        (size_of::<window_sub_level_info_t>() + size_of::<window_sub_level_payload_t>()) as u32;
    let recv_size = size_of::<get_window_sub_level_message_t>() as u32;

    let header_ptr = core::ptr::addr_of_mut!(msg.0.info.header);
    let local_port =
        core::ptr::read_unaligned(core::ptr::addr_of!(msg.0.info.header.msgh_local_port));
    let error = mach_msg(
        header_ptr,
        MACH_SEND_MSG
            | MACH_SEND_SYNC_OVERRIDE
            | MACH_SEND_PROPAGATE_QOS
            | MACH_RCV_MSG
            | MACH_RCV_SYNC_WAIT,
        send_size,
        recv_size,
        local_port,
        MACH_MSG_TIMEOUT_NONE,
        MACH_PORT_NULL,
    );

    if error != KERN_SUCCESS {
        if !window_sub_level_error_logged {
            let server_port = g_server_port;
            eprintln!(
                "SubLevel: Error receiving message (kr={}, server_port={}).",
                error, server_port
            );
            window_sub_level_error_logged = true;
        }
        mig_dealloc_special_reply_port(local_port);
        return 0;
    }

    if msg.0.info.header.msgh_id != response {
        if !window_sub_level_invalid_msg_logged {
            let received_id =
                core::ptr::read_unaligned(core::ptr::addr_of!(msg.0.info.header.msgh_id));
            eprintln!(
                "SubLevel: Invalid message received (id=0x{:x}, expected=0x{:x}).",
                received_id, response
            );
            window_sub_level_invalid_msg_logged = true;
        }
        mach_msg_destroy(header_ptr);
        return 0;
    }

    msg.0.response.sub_level
}

/// The sub-level of `wid`, or 0 when the query cannot be answered.
pub fn window_sub_level(wid: u32) -> c_int {
    unsafe { mach_get_window_sub_level(wid) }
}
