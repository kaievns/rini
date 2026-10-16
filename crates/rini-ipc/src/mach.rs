#![allow(non_camel_case_types, non_snake_case, reason = "Mach's C names")]
//! rini's Mach service: the bootstrap name, the receive port on a CFRunLoop, request dispatch and
//! replies. Built on `rini_mach_sys`.
use core::mem::{offset_of, size_of};
use core::ptr::{null, null_mut};
use std::ffi::CString;
use std::os::raw::{c_int, c_void};

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
pub fn is_mach_server_registered() -> bool { mach_get_bs_port(&bs_name()) != 0 }
type CFIndex = isize;
type Boolean = u8;
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
    #[allow(non_snake_case, reason = "CoreFoundation's field name")]
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
        shouldFreeInfo: *mut Boolean,
    ) -> CFMachPortRef;

    fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: CFIndex,
    ) -> CFRunLoopSourceRef;

    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    safe fn CFRunLoopGetMain() -> CFRunLoopRef;
    safe fn CFRunLoopRun();

    fn CFRelease(obj: *const c_void);

    safe static kCFRunLoopDefaultMode: CFStringRef;
}

/// What the server answers a request's payload with: the reply's bytes, or `None` for no reply.
pub type Handler = Box<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send>;

/// The registered port and what answers it. Leaked: the port's run loop source hands it to every
/// message for as long as the process runs, always on the main run loop.
struct Server {
    port: mach_port_t,
    bs_port: mach_port_t,
    handler: Handler,
}

/// Where a received message's request bytes are.
#[derive(Debug, PartialEq)]
enum Payload<'a> {
    /// In the message itself, after the header.
    Inline(&'a [u8]),
    /// Out of line: `size` bytes the kernel mapped at `address` when the message was received.
    OutOfLine { address: *const u8, size: usize },
}

/// Why a received message holds no payload rini can read.
#[derive(Debug, PartialEq, thiserror::Error)]
enum Malformed {
    #[error("{len} bytes, short of the {needed} its header and descriptors need")]
    Truncated { len: usize, needed: usize },
    #[error("its first descriptor is of type {0}, not out-of-line memory")]
    NotOutOfLine(u8),
}

const HEADER: usize = size_of::<mach_msg_header_t>();
/// `mach_msg_body_t`: the descriptor count a complex message carries after its header.
const BODY: usize = 4;
/// `mach_msg_ool_descriptor_t` as a 64-bit task receives it, packed to 4 by <mach/message.h>: the
/// address, a flags word whose top byte is the descriptor's type, then the size.
const OOL_DESCRIPTOR: usize = 16;
const MACH_MSG_OOL_DESCRIPTOR: u8 = 1;

/// The native-endian `u32` at `offset` of `bytes`, if they reach that far.
fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn truncated(message: &[u8], needed: usize) -> Malformed {
    Malformed::Truncated { len: message.len(), needed }
}

/// The header `message` starts with.
fn header_of(message: &[u8]) -> Result<mach_msg_header_t, Malformed> {
    let field = |offset| u32_at(message, offset).ok_or_else(|| truncated(message, HEADER));
    Ok(mach_msg_header_t {
        msgh_bits: field(offset_of!(mach_msg_header_t, msgh_bits))?,
        msgh_size: field(offset_of!(mach_msg_header_t, msgh_size))?,
        msgh_remote_port: field(offset_of!(mach_msg_header_t, msgh_remote_port))?,
        msgh_local_port: field(offset_of!(mach_msg_header_t, msgh_local_port))?,
        msgh_voucher_port: field(offset_of!(mach_msg_header_t, msgh_voucher_port))?,
        msgh_id: field(offset_of!(mach_msg_header_t, msgh_id))? as mach_msg_id_t,
    })
}

/// The request a received `message` carries: the bytes after a simple message's header, up to its
/// `msgh_size`, or the out-of-line memory a complex message's first descriptor names. A complex
/// message with no descriptor, or one of zero size, carries an empty request.
fn payload_of(message: &[u8]) -> Result<Payload<'_>, Malformed> {
    let header = header_of(message)?;
    if header.msgh_bits & MACH_MSGH_BITS_COMPLEX == 0 {
        let end = (header.msgh_size as usize).max(HEADER);
        let inline = message.get(HEADER..end).ok_or_else(|| truncated(message, end))?;
        return Ok(Payload::Inline(inline));
    }
    let count = u32_at(message, HEADER).ok_or_else(|| truncated(message, HEADER + BODY))?;
    if count == 0 {
        return Ok(Payload::Inline(&[]));
    }
    let end = HEADER + BODY + OOL_DESCRIPTOR;
    let descriptor = message.get(HEADER + BODY..end).ok_or_else(|| truncated(message, end))?;
    let kind = descriptor[11];
    if kind != MACH_MSG_OOL_DESCRIPTOR {
        return Err(Malformed::NotOutOfLine(kind));
    }
    let address = u64::from_ne_bytes(descriptor[..8].try_into().expect("8 bytes"));
    let size = u32::from_ne_bytes(descriptor[12..].try_into().expect("4 bytes")) as usize;
    if address == 0 || size == 0 {
        return Ok(Payload::Inline(&[]));
    }
    Ok(Payload::OutOfLine {
        address: address as usize as *const u8,
        size,
    })
}

/// Every message CFMachPort hands over is answered if it asks to be, then destroyed whether or not:
/// CFMachPort leaves a message's out-of-line memory and port rights to its callout.
extern "C" fn mach_message_callback(
    _port: CFMachPortRef,
    message: *mut c_void,
    _size: CFIndex,
    info: *mut c_void,
) {
    if message.is_null() {
        return;
    }
    // SAFETY: CFMachPort hands over the buffer the kernel received into, valid until this callout
    // returns: a header, whose `msgh_size` counts the bytes of message the kernel wrote (mach_msg(2)).
    let bytes = unsafe {
        let start = message.cast::<u8>();
        let size = start.add(offset_of!(mach_msg_header_t, msgh_size)).cast::<u32>();
        std::slice::from_raw_parts(start, (size.read_unaligned() as usize).max(HEADER))
    };
    if !info.is_null()
        && let Some((header, payload)) = request_of(bytes)
    {
        // SAFETY: `info` is the leaked `Server` `add_to_main_run_loop` gave the port's context.
        // This callout runs only on the main run loop that port's source was added to, so its
        // `Send` but not `Sync` handler is only ever called from that one thread.
        let server = unsafe { &*info.cast::<Server>() };
        match payload {
            Payload::Inline(payload) => answer(server, &header, payload),
            Payload::OutOfLine { address, size } => {
                // SAFETY: the kernel mapped `size` bytes at `address` for this out-of-line
                // descriptor on receipt, and they stay mapped until `mach_msg_destroy` below.
                let payload = unsafe { std::slice::from_raw_parts(address, size) };
                answer(server, &header, payload);
            }
        }
    }
    // SAFETY: `message` is the message just received, destroyed once and read no more, its
    // out-of-line memory included.
    let _ = unsafe { mach_msg_destroy(message.cast()) };
}

/// What a received message asks to be answered: its header and request, when it has a reply port
/// to answer on and a request rini can read, said why when the request is unreadable.
fn request_of(bytes: &[u8]) -> Option<(mach_msg_header_t, Payload<'_>)> {
    let header = header_of(bytes).ok()?;
    if header.msgh_remote_port == 0 {
        return None;
    }
    match payload_of(bytes) {
        Ok(payload) => Some((header, payload)),
        Err(malformed) => {
            error!("mach_message_callback: message {}: {malformed}", header.msgh_id);
            None
        }
    }
}

/// `payload` handed to the server's handler, and its answer, if any, sent back to `request`.
fn answer(server: &Server, request: &mach_msg_header_t, payload: &[u8]) {
    let Some(response) = (server.handler)(payload) else {
        return;
    };
    if !send_mach_reply(request, &response) {
        error!("Failed to send mach reply for message id {}", request.msgh_id);
    }
}

/// The service registered and its port on the main run loop, answered by `handler`; `None` when
/// either failed, said why.
fn mach_server_begin(handler: Handler) -> Option<&'static Server> {
    let (port, bs_port) = claim_port()?;
    let server: &'static Server = Box::leak(Box::new(Server { port, bs_port, handler }));
    if !add_to_main_run_loop(server) {
        return None;
    }

    info!(
        "mach_server_begin: registered '{}' in current bootstrap domain (port={}, bs_port={})",
        bs_name().to_string_lossy(),
        server.port,
        server.bs_port
    );

    Some(server)
}

/// A receive port with a send right of its own, registered under `bs_name()` with the large queue
/// limit, and the bootstrap port it is registered with.
fn claim_port() -> Option<(mach_port_t, mach_port_t)> {
    let task = mach_task_self();
    let mut bs_port = 0;
    // SAFETY: the out-pointer is a live local of the port type it is written as.
    if unsafe { task_get_special_port(task, TASK_BOOTSTRAP_PORT, &mut bs_port) } != KERN_SUCCESS {
        error!("mach_server_begin: task_get_special_port failed");
        return None;
    }

    let service_name = bs_name();

    let mut port = 0;
    // SAFETY: the out-pointer is a live local of the port type it is written as.
    let ar = unsafe { mach_port_allocate(task, MACH_PORT_RIGHT_RECEIVE, &mut port) };
    if ar != KERN_SUCCESS {
        error!("mach_server_begin: mach_port_allocate failed (kr={})", ar);
        return None;
    }

    // SAFETY: names the receive right just allocated; no pointer is passed.
    let ir = unsafe { mach_port_insert_right(task, port, port, MACH_MSG_TYPE_MAKE_SEND as c_int) };
    if ir != KERN_SUCCESS {
        error!(
            "mach_server_begin: mach_port_insert_right (MAKE_SEND) failed (kr={})",
            ir
        );
        return None;
    }

    // SAFETY: `service_name` is NUL-terminated and outlives the call.
    let rr = unsafe { bootstrap_register(bs_port, service_name.as_ptr(), port) };
    if rr != KERN_SUCCESS {
        error!("{}", register_failure(rr, &service_name.to_string_lossy()));
        return None;
    }

    let limits = mach_port_limits {
        mpl_qlimit: MACH_PORT_QLIMIT_LARGE,
    };
    // SAFETY: `info` is a live `mach_port_limits`, the one integer MACH_PORT_LIMITS_INFO_COUNT says.
    let _ = unsafe {
        mach_port_set_attributes(
            task,
            port,
            MACH_PORT_LIMITS_INFO,
            (&raw const limits).cast(),
            MACH_PORT_LIMITS_INFO_COUNT,
        )
    };
    Some((port, bs_port))
}

/// What a failed `bootstrap_register` of `name` returning `kr` is logged as.
fn register_failure(kr: kern_return_t, name: &str) -> String {
    match kr {
        BOOTSTRAP_NAME_IN_USE => {
            format!("mach_server_begin: bootstrap_register: name in use: {name}.")
        }
        BOOTSTRAP_NOT_PRIVILEGED => {
            format!("mach_server_begin: bootstrap_register: not privileged for domain (kr={kr}).")
        }
        _ => format!("mach_server_begin: bootstrap_register failed (kr={kr}) for {name}"),
    }
}

/// The port's messages delivered to `mach_message_callback` on the main run loop.
fn add_to_main_run_loop(server: &'static Server) -> bool {
    let cf_context = CFMachPortContext {
        version: 0,
        info: std::ptr::from_ref(server).cast_mut().cast(),
        retain: None,
        release: None,
        copyDescription: None,
    };
    // SAFETY: the declaration matches CFMachPort.h's; the context is live for the call, which
    // copies it, its `info` is `'static`, and the callout has CFMachPortCallBack's signature.
    let cf_mach_port = unsafe {
        CFMachPortCreateWithPort(
            null(),
            server.port,
            Some(mach_message_callback),
            &cf_context,
            null_mut(),
        )
    };
    if cf_mach_port.is_null() {
        error!(
            "mach_server_begin: CFMachPortCreateWithPort returned null (port={})",
            server.port
        );
        return false;
    }
    // SAFETY: the declaration matches CFMachPort.h's, and `cf_mach_port` is the live CFMachPort
    // just created.
    let source = unsafe { CFMachPortCreateRunLoopSource(null(), cf_mach_port, 0) };
    if source.is_null() {
        error!("mach_server_begin: CFMachPortCreateRunLoopSource returned null");
        // SAFETY: made by a Create call, so this reference is ours, released once.
        unsafe { CFRelease(cf_mach_port) };
        return false;
    }
    // SAFETY: `source` is live and the mode is CoreFoundation's constant; adding a source to
    // another thread's run loop is thread-safe. Both were made by Create calls, so these references
    // are ours, released once: the run loop retains the source, and the source the port.
    unsafe {
        CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopDefaultMode);
        CFRelease(source);
        CFRelease(cf_mach_port);
    }
    true
}

/// `response` sent on the reply port `request` came with. `false`, said why, when it has none,
/// `response` is over one message, or the send failed.
fn send_mach_reply(request: &mach_msg_header_t, response: &[u8]) -> bool {
    if response.len() > MAX_MESSAGE_SIZE_USIZE {
        error!("send_mach_reply: invalid args response_len={}", response.len());
        return false;
    }
    let response_len = response.len() as u32;

    let task = mach_task_self();
    let port_type = |port: mach_port_t| {
        let mut port_type = 0;
        if port != 0 {
            // SAFETY: the out-pointer is a live local of the type it is written as.
            let _ = unsafe { mach_port_type(task, port, &mut port_type) };
        }
        port_type
    };
    let remote_port_type = port_type(request.msgh_remote_port);
    let local_port_type = port_type(request.msgh_local_port);

    let reply_port = request.msgh_remote_port;
    let reply_descriptor = MACH_MSG_TYPE_COPY_SEND;
    if reply_port == 0 {
        error!(
            "send_mach_reply: no available send right (remote_port_type={} local_port_type={} remote_port={} local_port={})",
            remote_port_type, local_port_type, request.msgh_remote_port, request.msgh_local_port
        );
        return false;
    };

    let mut reply = mach_message_t::default();

    let aligned_len = (response_len + 3) & !3;
    let total_size = (size_of::<mach_msg_header_t>() as u32) + aligned_len;

    reply.header.msgh_bits = MACH_MSGH_BITS(reply_descriptor, 0);
    reply.header.msgh_size = total_size;
    reply.header.msgh_remote_port = reply_port;
    reply.header.msgh_id = request.msgh_id;
    reply.data[..response.len()].copy_from_slice(response);

    // SAFETY: `reply` is a whole simple message with its `msgh_size` inside it, a header and at most
    // MAX_MESSAGE_SIZE bytes of data, sent with no receive.
    let result = unsafe {
        mach_msg(
            &mut reply.header,
            MACH_SEND_MSG,
            reply.header.msgh_size,
            0,
            0,
            MACH_MSG_TIMEOUT_NONE,
            0,
        )
    };

    if result != MACH_MSG_SUCCESS {
        error!(
            "send_mach_reply: mach_msg failed result={} reply_port={} port_type={} descriptor={} remote_port_type={} local_port_type={} original_remote={} original_local={}",
            result,
            reply_port,
            port_type(reply_port),
            reply_descriptor,
            remote_port_type,
            local_port_type,
            MACH_MSGH_BITS_REMOTE(request.msgh_bits),
            MACH_MSGH_BITS_LOCAL(request.msgh_bits)
        );
        return false;
    }

    true
}

/// Registers the service and answers each request on the main run loop with what `handler` returns
/// for its payload. `false` when the service could not be registered or scheduled.
pub fn mach_server_run(handler: Handler) -> bool {
    let Some(server) = mach_server_begin(handler) else {
        error!("mach_server_run: mach_server_begin failed, aborting run loop");
        return false;
    };

    debug!(
        "mach_server_run: ports ready (port={}, bs_port={})",
        server.port, server.bs_port
    );
    CFRunLoopRun();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_registration_names_the_service_or_the_code_bootstrap_gave() {
        assert_eq!(
            register_failure(BOOTSTRAP_NAME_IN_USE, "git.kaievns.rini"),
            "mach_server_begin: bootstrap_register: name in use: git.kaievns.rini."
        );
        assert_eq!(
            register_failure(BOOTSTRAP_NOT_PRIVILEGED, "git.kaievns.rini"),
            "mach_server_begin: bootstrap_register: not privileged for domain (kr=1100)."
        );
        assert_eq!(
            register_failure(5, "git.kaievns.rini"),
            "mach_server_begin: bootstrap_register failed (kr=5) for git.kaievns.rini"
        );
    }

    /// A message's bytes as the kernel lays them out: a header of `bits` and `size`, then `rest`.
    fn message(bits: u32, size: usize, rest: &[u8]) -> Vec<u8> {
        let header = [bits, size as u32, 7, 8, 0, 42];
        let mut bytes: Vec<u8> = header.iter().flat_map(|field| field.to_ne_bytes()).collect();
        bytes.extend_from_slice(rest);
        bytes
    }

    /// A complex message whose one descriptor is of type `kind`, naming `size` bytes at `address`.
    fn complex(address: u64, size: u32, kind: u8) -> Vec<u8> {
        let mut rest = 1u32.to_ne_bytes().to_vec();
        rest.extend_from_slice(&address.to_ne_bytes());
        rest.extend_from_slice(&[0, 1, 0, kind]);
        rest.extend_from_slice(&size.to_ne_bytes());
        message(MACH_MSGH_BITS_COMPLEX, HEADER + rest.len(), &rest)
    }

    #[test]
    fn a_simple_message_carries_the_bytes_after_its_header_up_to_its_size() {
        let bytes = message(0, HEADER + 8, b"{\"a\":1}\0trailer");
        assert_eq!(payload_of(&bytes), Ok(Payload::Inline(b"{\"a\":1}\0")));
        let header = header_of(&bytes).unwrap();
        assert_eq!((header.msgh_remote_port, header.msgh_id), (7, 42));
        assert_eq!(payload_of(&message(0, HEADER, &[])), Ok(Payload::Inline(&[])));
    }

    #[test]
    fn a_complex_message_carries_its_first_descriptor_s_out_of_line_memory() {
        assert_eq!(
            payload_of(&complex(0x1234_5678, 10, MACH_MSG_OOL_DESCRIPTOR)),
            Ok(Payload::OutOfLine {
                address: 0x1234_5678 as *const u8,
                size: 10
            })
        );
    }

    #[test]
    fn a_zero_size_or_absent_descriptor_is_an_empty_request() {
        let empty = Ok(Payload::Inline(&[]));
        assert_eq!(
            payload_of(&complex(0x1234_5678, 0, MACH_MSG_OOL_DESCRIPTOR)),
            empty
        );
        assert_eq!(payload_of(&complex(0, 10, MACH_MSG_OOL_DESCRIPTOR)), empty);
        let none = message(MACH_MSGH_BITS_COMPLEX, HEADER + BODY, &0u32.to_ne_bytes());
        assert_eq!(payload_of(&none), empty);
    }

    #[test]
    fn only_a_readable_request_with_a_reply_port_is_answered() {
        let request = message(0, HEADER + 8, b"{\"a\":1}\0");
        let (header, payload) = request_of(&request).unwrap();
        assert_eq!(
            (header.msgh_remote_port, payload),
            (7, Payload::Inline(b"{\"a\":1}\0"))
        );
        let mut no_reply_port = complex(0x1234_5678, 10, MACH_MSG_OOL_DESCRIPTOR);
        no_reply_port[offset_of!(mach_msg_header_t, msgh_remote_port)..][..4].fill(0);
        assert_eq!(request_of(&no_reply_port), None);
        assert_eq!(request_of(&complex(5, 10, 0)), None);
        assert_eq!(request_of(&request[..HEADER - 1]), None);
    }

    #[test]
    fn a_descriptor_other_than_out_of_line_memory_is_refused() {
        assert_eq!(payload_of(&complex(5, 10, 0)), Err(Malformed::NotOutOfLine(0)));
    }

    #[test]
    fn a_truncated_or_garbage_message_is_an_error_and_never_a_read_past_its_bytes() {
        let whole = complex(0x1234_5678, 10, MACH_MSG_OOL_DESCRIPTOR);
        for len in 0..whole.len() {
            assert!(
                matches!(payload_of(&whole[..len]), Err(Malformed::Truncated { .. })),
                "{len} bytes"
            );
        }
        let overlong = message(0, HEADER + 100, b"short");
        assert_eq!(
            payload_of(&overlong),
            Err(Malformed::Truncated {
                len: HEADER + 5,
                needed: HEADER + 100
            })
        );
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2000 {
            let len = (next() % 64) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if let Ok(Payload::Inline(inline)) = payload_of(&bytes) {
                assert!(inline.len() <= len.saturating_sub(HEADER));
            }
        }
    }

    /// A receive right of this task's own, released on drop.
    struct OwnPort(mach_port_t);

    impl OwnPort {
        fn new() -> Self {
            let mut port = 0;
            // SAFETY: the out-pointer is a live local of the port type it is written as.
            let kr =
                unsafe { mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &mut port) };
            assert_eq!(kr, KERN_SUCCESS);
            Self(port)
        }

        /// The message waiting on the port, received into `buffer`, as its `msgh_size` bytes.
        fn receive<'a>(&self, buffer: &'a mut [u64; 256]) -> &'a [u8] {
            // SAFETY: `buffer` is live and as long as the receive size given.
            let kr = unsafe {
                mach_msg(
                    buffer.as_mut_ptr().cast(),
                    MACH_RCV_MSG,
                    0,
                    size_of::<[u64; 256]>() as u32,
                    self.0,
                    0,
                    0,
                )
            };
            assert_eq!(kr, KERN_SUCCESS);
            let buffer: &'a [u64; 256] = buffer;
            let bytes = bytes_of(buffer);
            &bytes[..header_of(bytes).unwrap().msgh_size as usize]
        }
    }

    impl Drop for OwnPort {
        fn drop(&mut self) {
            // SAFETY: names the receive right `new` allocated; no pointer is passed.
            let _ = unsafe {
                mach_port_mod_refs(mach_task_self(), self.0, MACH_PORT_RIGHT_RECEIVE, -1)
            };
        }
    }

    fn bytes_of(buffer: &[u64; 256]) -> &[u8] {
        // SAFETY: every `u64` is eight initialised bytes, borrowed with `buffer` for its whole length.
        unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast(), size_of::<[u64; 256]>()) }
    }

    /// A complex message with one out-of-line descriptor, laid out as <mach/message.h> packs it.
    #[repr(C, packed(4))]
    struct OolMessage {
        header: mach_msg_header_t,
        count: u32,
        address: *const u8,
        flags: [u8; 4],
        size: u32,
    }

    /// The layout `payload_of` reads is the kernel's: messages sent with the payload out of line
    /// and inline, received back from the kernel, parse to the bytes sent.
    #[test]
    fn the_kernel_s_out_of_line_and_inline_messages_parse_to_the_bytes_sent() {
        let port = OwnPort::new();
        let sent = b"{\"get_metrics\":null}\0";
        let mut message = OolMessage {
            header: mach_msg_header_t {
                msgh_bits: MACH_MSGH_BITS(MACH_MSG_TYPE_MAKE_SEND, 0) | MACH_MSGH_BITS_COMPLEX,
                msgh_size: size_of::<OolMessage>() as u32,
                msgh_remote_port: port.0,
                msgh_id: 9,
                ..mach_msg_header_t::default()
            },
            count: 1,
            address: sent.as_ptr(),
            flags: [0, 1, 0, MACH_MSG_OOL_DESCRIPTOR],
            size: sent.len() as u32,
        };
        // SAFETY: `message` is a whole complex message of its `msgh_size` whose descriptor names
        // `sent`, both live across the send, which copies them.
        let kr = unsafe {
            mach_msg(
                (&raw mut message).cast(),
                MACH_SEND_MSG,
                size_of::<OolMessage>() as u32,
                0,
                0,
                0,
                0,
            )
        };
        assert_eq!(kr, KERN_SUCCESS);
        let mut buffer = [0u64; 256];
        let received = port.receive(&mut buffer);
        let Ok(Payload::OutOfLine { address, size }) = payload_of(received) else {
            panic!("{:?}", payload_of(received))
        };
        // SAFETY: the kernel mapped `size` bytes at `address` on receipt, freed only by the destroy
        // below.
        assert_eq!(unsafe { std::slice::from_raw_parts(address, size) }, sent);
        // SAFETY: the message just received, destroyed once and read no more.
        let _ = unsafe { mach_msg_destroy(buffer.as_mut_ptr().cast()) };

        let mut inline = mach_inline_message_t::<24> {
            header: mach_msg_header_t {
                msgh_bits: MACH_MSGH_BITS(MACH_MSG_TYPE_MAKE_SEND, 0),
                msgh_size: (HEADER + 24) as u32,
                msgh_remote_port: port.0,
                ..mach_msg_header_t::default()
            },
            data: [0; 24],
        };
        inline.data[..sent.len()].copy_from_slice(sent);
        // SAFETY: `inline` is a whole simple message of its `msgh_size`, sent with no receive.
        let kr = unsafe {
            mach_msg(
                &mut inline.header,
                MACH_SEND_MSG,
                (HEADER + 24) as u32,
                0,
                0,
                0,
                0,
            )
        };
        assert_eq!(kr, KERN_SUCCESS);
        let mut buffer = [0u64; 256];
        let received = port.receive(&mut buffer);
        assert_eq!(payload_of(received), Ok(Payload::Inline(&inline.data[..])));
        // SAFETY: the message just received, destroyed once and read no more.
        let _ = unsafe { mach_msg_destroy(buffer.as_mut_ptr().cast()) };
    }
}
