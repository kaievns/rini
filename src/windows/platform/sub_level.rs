#![allow(non_camel_case_types, non_snake_case, reason = "SkyLight's C names")]
//! A window's sub-level, asked of SkyLight's private connection server port over Mach. The port
//! comes from a Mach message to `SLSServerPort`; there is no public API for either.
use core::mem::{size_of, zeroed};
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use rini_mach_sys::*;
use tracing::{debug, warn};

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
static SERVER_PORT: AtomicU32 = AtomicU32::new(MACH_PORT_NULL);
static RECEIVE_FAILURE_LOGGED: AtomicBool = AtomicBool::new(false);
static WRONG_REPLY_LOGGED: AtomicBool = AtomicBool::new(false);

pub fn init_window_sub_level_server_port() { let _ = server_port(); }

/// The connection server port, made on first use and made again after a failed attempt. Two
/// threads racing to make it keep the first one stored and give the other back.
fn server_port() -> Option<mach_port_t> {
    let port = SERVER_PORT.load(Ordering::Acquire);
    if port != MACH_PORT_NULL {
        return Some(port);
    }
    // SAFETY: a Mach message whose reply lands in a stack buffer of the size it is sent with.
    let made = unsafe { create_connection_server_port() };
    if made == MACH_PORT_NULL {
        return None;
    }
    match SERVER_PORT.compare_exchange(MACH_PORT_NULL, made, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Some(made),
        Err(stored) => {
            // SAFETY: `made` is a send right of this task's that nothing else holds.
            unsafe { mach_port_deallocate(mach_task_self(), made) };
            Some(stored)
        }
    }
}

/// True only the first time it is asked of `flag`.
fn first_time(flag: &AtomicBool) -> bool { !flag.swap(true, Ordering::Relaxed) }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Receive(kern_return_t),
    WrongReply { id: i32, expected: i32 },
}

/// What a sub-level request's reply says: the sub-level, or why there is none.
fn answer(kr: kern_return_t, id: i32, expected: i32, sub_level: c_int) -> Result<c_int, Failure> {
    if kr != KERN_SUCCESS {
        return Err(Failure::Receive(kr));
    }
    if id != expected {
        return Err(Failure::WrongReply { id, expected });
    }
    Ok(sub_level)
}

unsafe fn create_connection_server_port() -> mach_port_t {
    // SAFETY: an all-integer message, zeroed, sent with its own size and received into itself.
    unsafe {
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
        let local_port =
            core::ptr::read_unaligned(core::ptr::addr_of!(msg.0.header.msgh_local_port));
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
}
const WINDOW_SUB_LEVEL_REQUEST_PRE_TAHOE: i32 = 0x73C3;
const WINDOW_SUB_LEVEL_REQUEST_POST_TAHOE: i32 = 0x76E3;
const WINDOW_SUB_LEVEL_RESPONSE_PRE_TAHOE: i32 = 0x7427;
const WINDOW_SUB_LEVEL_RESPONSE_POST_TAHOE: i32 = 0x7747;
/// The sub-level of `wid`, none when the window server gives no answer. Each kind of failure is
/// logged once.
pub fn window_sub_level(wid: u32) -> Option<c_int> {
    let port = server_port()?;
    // SAFETY: a Mach message whose reply lands in a stack buffer of the size it is received with.
    match unsafe { request_sub_level(port, wid) } {
        Ok(sub_level) => Some(sub_level),
        Err(Failure::Receive(kr)) => {
            if first_time(&RECEIVE_FAILURE_LOGGED) {
                warn!(kr, server_port = port, "sub-level: no reply received");
            }
            None
        }
        Err(Failure::WrongReply { id, expected }) => {
            if first_time(&WRONG_REPLY_LOGGED) {
                warn!(
                    id = format_args!("{id:#x}"),
                    expected = format_args!("{expected:#x}"),
                    "sub-level: unexpected reply"
                );
            }
            None
        }
    }
}

unsafe fn request_sub_level(port: mach_port_t, wid: u32) -> Result<c_int, Failure> {
    let (request, response) = if objc2::available!(macos = 26.0) {
        (
            WINDOW_SUB_LEVEL_REQUEST_POST_TAHOE,
            WINDOW_SUB_LEVEL_RESPONSE_POST_TAHOE,
        )
    } else {
        (
            WINDOW_SUB_LEVEL_REQUEST_PRE_TAHOE,
            WINDOW_SUB_LEVEL_RESPONSE_PRE_TAHOE,
        )
    };

    // SAFETY: every field is an integer, so all zeroes is a valid message.
    let mut msg = aligned_message_t(unsafe { zeroed::<get_window_sub_level_message_t>() });
    // SAFETY: NDR_record is a constant libsystem exports.
    msg.0.info.NDR_record = unsafe { NDR_record };
    msg.0.info.header.msgh_remote_port = port;
    // SAFETY: the thread's special reply port, given back below when the reply fails.
    msg.0.info.header.msgh_local_port = unsafe { mig_get_special_reply_port() };
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
    // SAFETY: an unaligned read of a field of a packed struct this function owns.
    let local_port = unsafe {
        core::ptr::read_unaligned(core::ptr::addr_of!(msg.0.info.header.msgh_local_port))
    };
    // SAFETY: `header_ptr` heads a buffer of `recv_size` bytes, at least `send_size`.
    let kr = unsafe {
        mach_msg(
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
        )
    };
    // SAFETY: unaligned reads of fields of a packed struct this function owns.
    let id = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(msg.0.info.header.msgh_id)) };
    let sub_level =
        unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(msg.0.response.sub_level)) };
    let answer = answer(kr, id, response, sub_level);
    match answer {
        // SAFETY: the reply port the failed receive left unusable.
        Err(Failure::Receive(_)) => unsafe { mig_dealloc_special_reply_port(local_port) },
        // SAFETY: a message received whole into `msg`, whose rights are released.
        Err(Failure::WrongReply { .. }) => unsafe {
            mach_msg_destroy(header_ptr);
        },
        Ok(_) => {}
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_is_logged_the_first_time_only() {
        let flag = AtomicBool::new(false);
        assert!(first_time(&flag));
        assert!(!first_time(&flag));
        assert!(!first_time(&flag));
    }

    #[test]
    fn a_reply_answers_only_when_received_with_the_expected_id() {
        assert_eq!(answer(KERN_SUCCESS, 0x7747, 0x7747, 3), Ok(3));
        assert_eq!(answer(KERN_SUCCESS, 0x7747, 0x7747, 0), Ok(0));
        assert_eq!(
            answer(0x10004003, 0x7747, 0x7747, 3),
            Err(Failure::Receive(0x10004003))
        );
        assert_eq!(
            answer(KERN_SUCCESS, 0x7427, 0x7747, 3),
            Err(Failure::WrongReply { id: 0x7427, expected: 0x7747 })
        );
    }
}
