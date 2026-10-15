#![allow(non_camel_case_types, non_snake_case, unsafe_op_in_unsafe_fn)]
#![allow(clippy::missing_safety_doc)]
//! Raw Mach messaging: types, constants, the `mach_msg` family, bootstrap lookup, and the one
//! helper the IPC server's second-instance check calls. No policy.
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};

use tracing::{debug, error};

pub const MAX_MESSAGE_SIZE: u32 = 262_144;

pub type kern_return_t = c_int;
pub type mach_port_t = u32;
pub type mach_port_name_t = u32;
pub type mach_msg_bits_t = u32;
pub type mach_msg_size_t = u32;
pub type mach_msg_option_t = u32;
pub type mach_msg_id_t = i32;

pub const KERN_SUCCESS: kern_return_t = 0;
pub const MACH_MSG_SUCCESS: kern_return_t = 0;

pub const MACH_SEND_MSG: u32 = 0x0000_0001;
pub const MACH_RCV_MSG: u32 = 0x0000_0002;

pub const MACH_MSG_TIMEOUT_NONE: u32 = 0;
pub const MACH_PORT_NULL: u32 = 0;
pub const MACH_SEND_SYNC_OVERRIDE: u32 = 0x0010_0000;
pub const MACH_SEND_PROPAGATE_QOS: u32 = 0x0020_0000;
pub const MACH_RCV_SYNC_WAIT: u32 = 0x0000_4000;
pub const MACH_MSGH_BITS_REMOTE_MASK: u32 = 0x0000_001f;

pub const MACH_MSG_TYPE_COPY_SEND: u32 = 19;
pub const MACH_MSG_TYPE_MAKE_SEND_ONCE: u32 = 21;
pub const MACH_MSGH_BITS_COMPLEX: u32 = 0x8000_0000;
pub const MACH_MSG_TYPE_MAKE_SEND: u32 = 20;

pub const MACH_PORT_RIGHT_RECEIVE: c_int = 1;
pub const MACH_PORT_LIMITS_INFO: c_int = 1;
pub const MACH_PORT_LIMITS_INFO_COUNT: u32 = 1;
pub const MACH_PORT_QLIMIT_LARGE: u32 = 1024;

pub const TASK_BOOTSTRAP_PORT: c_int = 4;

pub const BOOTSTRAP_NOT_PRIVILEGED: kern_return_t = 1100;
pub const BOOTSTRAP_NAME_IN_USE: kern_return_t = 1101;
pub const BOOTSTRAP_UNKNOWN_SERVICE: kern_return_t = 1102;

#[inline]
pub const fn MACH_MSGH_BITS(remote: u32, local: u32) -> u32 { remote | (local << 8) }

#[inline]
pub const fn MACH_MSGH_BITS_SET(remote: u32, local: u32, voucher: u32, other: u32) -> u32 {
    ((remote & MACH_MSGH_BITS_REMOTE_MASK) | (local << 8) | (voucher << 16)) | other
}

#[inline]
pub const fn MACH_MSGH_BITS_REMOTE(bits: u32) -> u32 { bits & 0xff }

#[inline]
pub const fn MACH_MSGH_BITS_LOCAL(bits: u32) -> u32 { (bits >> 8) & 0xff }

#[repr(C)]
#[derive(Copy, Clone)]
pub struct mach_msg_header_t {
    pub msgh_bits: mach_msg_bits_t,
    pub msgh_size: mach_msg_size_t,
    pub msgh_remote_port: mach_port_t,
    pub msgh_local_port: mach_port_t,
    pub msgh_voucher_port: mach_port_name_t,
    pub msgh_id: mach_msg_id_t,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ndr_record_t {
    pub mig_vers: u8,
    pub if_vers: u8,
    pub reserved1: u8,
    pub mig_encoding: u8,
    pub int_rep: u8,
    pub char_rep: u8,
    pub float_rep: u8,
    pub reserved2: u8,
}

#[repr(C, align(8))]
pub struct aligned_message_t<T>(pub T);

#[repr(C)]
pub struct mach_port_limits {
    pub mpl_qlimit: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct mach_msg_body_t {
    pub msgh_descriptor_count: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct mach_msg_ool_descriptor_t {
    pub address: *mut c_void,
    pub size: u32,
    pub deallocate: u8, // boolean_t
    pub copy: u8,       // mach_msg_copy_options_t
    pub pad1: u32,
    pub type_: u32, // MACH_MSG_OOL_DESCRIPTOR = 1
}

#[repr(C)]
pub struct mach_inline_message_t<const N: usize> {
    pub header: mach_msg_header_t,
    pub data: [u8; N],
}

#[repr(C)]
pub struct mach_receive_buffer_t<const N: usize> {
    pub message: mach_inline_message_t<N>,
    pub trailer: [u8; 512],
}

#[link(name = "System", kind = "framework")]
unsafe extern "C" {
    pub fn mach_task_self() -> mach_port_name_t;

    pub fn task_get_special_port(
        task: mach_port_name_t,
        which: c_int,
        special_port: *mut mach_port_t,
    ) -> kern_return_t;

    pub fn mach_port_allocate(
        task: mach_port_name_t,
        right: c_int,
        name: *mut mach_port_name_t,
    ) -> kern_return_t;

    pub fn mach_port_insert_right(
        task: mach_port_name_t,
        name: mach_port_name_t,
        poly: mach_port_t,
        polyPoly: c_int,
    ) -> kern_return_t;

    pub fn mach_port_mod_refs(
        task: mach_port_name_t,
        name: mach_port_name_t,
        right: c_int,
        delta: c_int,
    ) -> kern_return_t;

    pub fn mach_port_deallocate(task: mach_port_name_t, name: mach_port_name_t) -> kern_return_t;

    pub fn mach_port_set_attributes(
        task: mach_port_name_t,
        name: mach_port_name_t,
        flavor: c_int,
        info: *const c_void,
        count: u32,
    ) -> kern_return_t;

    pub fn mach_port_type(
        task: mach_port_name_t,
        name: mach_port_name_t,
        ptype: *mut u32,
    ) -> kern_return_t;

    pub fn mach_msg(
        msg: *mut mach_msg_header_t,
        option: mach_msg_option_t,
        send_size: mach_msg_size_t,
        rcv_size: mach_msg_size_t,
        rcv_name: mach_port_name_t,
        timeout: u32,
        notify: mach_port_name_t,
    ) -> kern_return_t;

    pub fn mach_msg_destroy(msg: *mut mach_msg_header_t) -> kern_return_t;

    pub fn mig_get_special_reply_port() -> mach_port_name_t;
    pub fn mig_dealloc_special_reply_port(reply_port: mach_port_name_t);
    pub static NDR_record: ndr_record_t;

    pub fn bootstrap_look_up(
        bp: mach_port_t,
        service_name: *const c_char,
        sp: *mut mach_port_t,
    ) -> kern_return_t;

    pub fn bootstrap_register(
        bp: mach_port_t,
        service_name: *const c_char,
        sp: mach_port_t,
    ) -> kern_return_t;
}

pub const MAX_MESSAGE_SIZE_USIZE: usize = MAX_MESSAGE_SIZE as usize;
pub type mach_message_t = mach_inline_message_t<MAX_MESSAGE_SIZE_USIZE>;
pub type mach_buffer_t = mach_receive_buffer_t<MAX_MESSAGE_SIZE_USIZE>;

pub unsafe fn mach_get_bs_port(bs_name: &CStr) -> mach_port_t {
    let mut bs_port: mach_port_t = 0;
    if task_get_special_port(mach_task_self(), TASK_BOOTSTRAP_PORT, &mut bs_port) != KERN_SUCCESS {
        error!("mach_get_bs_port: task_get_special_port failed");
        return 0;
    }

    let mut service_port: mach_port_t = 0;
    let result = bootstrap_look_up(bs_port, bs_name.as_ptr(), &mut service_port);
    if result != KERN_SUCCESS {
        if result != BOOTSTRAP_UNKNOWN_SERVICE {
            error!(
                "mach_get_bs_port: bootstrap_look_up failed for {} (kr={})",
                bs_name.to_string_lossy(),
                result
            );
        } else {
            debug!(
                "mach_get_bs_port: {} is not registered yet (kr={})",
                bs_name.to_string_lossy(),
                result
            );
        }
        return 0;
    }
    service_port
}
