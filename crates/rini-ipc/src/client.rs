//! A small, synchronous Rust client for Rini's Mach IPC API.
//!
//! `rini-cli` is the one client: it sends a request and blocks for the response.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::mem::{size_of, zeroed};
use std::ptr::copy_nonoverlapping;
use std::thread;
use std::time::Duration;

use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::protocol::{RiniRequest, RiniResponse};

const MAX_MESSAGE_SIZE: usize = 262_144;
const DEFAULT_SERVICE_NAME: &str = "git.kaievns.rini";

type KernReturn = c_int;
type MachPort = u32;
type MachMessageSize = u32;
type MachMessageOption = u32;

const KERN_SUCCESS: KernReturn = 0;
const MACH_SEND_MSG: MachMessageOption = 0x0000_0001;
const MACH_RCV_MSG: MachMessageOption = 0x0000_0002;
const MACH_MSG_TYPE_COPY_SEND: u32 = 19;
const MACH_MSG_TYPE_MAKE_SEND: u32 = 20;
const MACH_PORT_RIGHT_RECEIVE: c_int = 1;
const MACH_PORT_LIMITS_INFO: c_int = 1;
const MACH_PORT_LIMITS_INFO_COUNT: u32 = 1;
const TASK_BOOTSTRAP_PORT: c_int = 4;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("Rini's Mach service is not registered")]
    ServiceUnavailable,
    #[error("invalid Mach service name")]
    InvalidServiceName,
    #[error("Mach operation {operation} failed with code {code}")]
    Mach {
        operation: &'static str,
        code: KernReturn,
    },
    #[error("IPC payload exceeds the {MAX_MESSAGE_SIZE}-byte limit")]
    MessageTooLarge,
    #[error("Rini returned an empty response")]
    EmptyResponse,
    #[error("{kind} payload is missing its NUL terminator")]
    MissingTerminator { kind: &'static str },
    #[error("failed to encode request: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("failed to decode {kind}: {source}")]
    Decode {
        kind: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

/// A handle for issuing synchronous requests to the running Rini process.
#[derive(Clone, Copy, Debug, Default)]
pub struct RiniMachClient;

impl RiniMachClient {
    /// Creates a client handle.
    ///
    /// Service discovery happens when a request is sent, allowing callers to
    /// construct the client before Rini has finished starting.
    pub fn connect() -> Result<Self, ClientError> {
        Ok(Self)
    }

    /// Sends one request and blocks until Rini responds.
    pub fn send_request(&self, request: &RiniRequest) -> Result<RiniResponse, ClientError> {
        let request_json = serde_json::to_vec(request).map_err(ClientError::Encode)?;
        let response = unsafe { send_request(&request_json)? };
        parse_json_payload(&response, "response")
    }
}

fn parse_json_payload<T: DeserializeOwned>(
    payload: &[u8],
    kind: &'static str,
) -> Result<T, ClientError> {
    if payload.is_empty() {
        return Err(ClientError::EmptyResponse);
    }
    let bytes = CStr::from_bytes_until_nul(payload)
        .map_err(|_| ClientError::MissingTerminator { kind })?
        .to_bytes();
    serde_json::from_slice(bytes).map_err(|source| ClientError::Decode { kind, source })
}

fn service_name() -> Result<CString, ClientError> {
    let name = std::env::var("RINI_BS_NAME").unwrap_or_else(|_| DEFAULT_SERVICE_NAME.to_owned());
    CString::new(name).map_err(|_| ClientError::InvalidServiceName)
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct MachMessageHeader {
    bits: u32,
    size: MachMessageSize,
    remote_port: MachPort,
    local_port: MachPort,
    voucher_port: MachPort,
    id: i32,
}

#[repr(C)]
struct InlineMessage {
    header: MachMessageHeader,
    data: [u8; MAX_MESSAGE_SIZE],
}

#[repr(C)]
struct ReceiveBuffer {
    message: InlineMessage,
    trailer: [u8; 512],
}

#[repr(C)]
struct MachPortLimits {
    queue_limit: u32,
}

#[link(name = "System", kind = "framework")]
unsafe extern "C" {
    fn mach_task_self() -> MachPort;
    fn task_get_special_port(task: MachPort, which: c_int, port: *mut MachPort) -> KernReturn;
    fn mach_port_allocate(task: MachPort, right: c_int, name: *mut MachPort) -> KernReturn;
    fn mach_port_insert_right(
        task: MachPort,
        name: MachPort,
        poly: MachPort,
        disposition: c_int,
    ) -> KernReturn;
    fn mach_port_mod_refs(task: MachPort, name: MachPort, right: c_int, delta: c_int)
    -> KernReturn;
    fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
    fn mach_port_set_attributes(
        task: MachPort,
        name: MachPort,
        flavor: c_int,
        info: *const c_void,
        count: u32,
    ) -> KernReturn;
    fn mach_msg(
        message: *mut MachMessageHeader,
        option: MachMessageOption,
        send_size: MachMessageSize,
        receive_size: MachMessageSize,
        receive_name: MachPort,
        timeout: u32,
        notify: MachPort,
    ) -> KernReturn;
    fn mach_msg_destroy(message: *mut MachMessageHeader) -> KernReturn;
    fn bootstrap_look_up(
        bootstrap_port: MachPort,
        service_name: *const c_char,
        service_port: *mut MachPort,
    ) -> KernReturn;
}

#[derive(Debug)]
struct ReplyPort {
    name: MachPort,
}

struct ServicePort {
    name: MachPort,
}

impl ServicePort {
    fn new(name: MachPort) -> Self {
        Self { name }
    }
}

impl Drop for ServicePort {
    fn drop(&mut self) {
        unsafe {
            let _ = mach_port_deallocate(mach_task_self(), self.name);
        }
    }
}

impl ReplyPort {
    fn allocate(queue_limit: u32) -> Result<Self, ClientError> {
        unsafe {
            let task = mach_task_self();
            let mut name = 0;
            let result = mach_port_allocate(task, MACH_PORT_RIGHT_RECEIVE, &mut name);
            if result != KERN_SUCCESS {
                return Err(ClientError::Mach {
                    operation: "mach_port_allocate",
                    code: result,
                });
            }

            let limits = MachPortLimits { queue_limit };
            let _ = mach_port_set_attributes(
                task,
                name,
                MACH_PORT_LIMITS_INFO,
                &limits as *const _ as *const c_void,
                MACH_PORT_LIMITS_INFO_COUNT,
            );

            let result = mach_port_insert_right(task, name, name, MACH_MSG_TYPE_MAKE_SEND as c_int);
            if result != KERN_SUCCESS {
                let _ = mach_port_mod_refs(task, name, MACH_PORT_RIGHT_RECEIVE, -1);
                let _ = mach_port_deallocate(task, name);
                return Err(ClientError::Mach {
                    operation: "mach_port_insert_right",
                    code: result,
                });
            }
            Ok(Self { name })
        }
    }
}

impl Drop for ReplyPort {
    fn drop(&mut self) {
        unsafe {
            let task = mach_task_self();
            let _ = mach_port_mod_refs(task, self.name, MACH_PORT_RIGHT_RECEIVE, -1);
            let _ = mach_port_deallocate(task, self.name);
        }
    }
}

unsafe fn lookup_service(name: &CStr) -> Result<MachPort, ClientError> {
    let mut bootstrap_port = 0;
    let result = unsafe {
        task_get_special_port(mach_task_self(), TASK_BOOTSTRAP_PORT, &mut bootstrap_port)
    };
    if result != KERN_SUCCESS {
        return Err(ClientError::Mach {
            operation: "task_get_special_port",
            code: result,
        });
    }

    let mut service_port = 0;
    let result = unsafe { bootstrap_look_up(bootstrap_port, name.as_ptr(), &mut service_port) };
    if result != KERN_SUCCESS {
        return Err(ClientError::ServiceUnavailable);
    }
    Ok(service_port)
}

unsafe fn find_service_with_retry() -> Result<ServicePort, ClientError> {
    let name = service_name()?;
    for attempt in 0..5 {
        if let Ok(port) = unsafe { lookup_service(&name) } {
            return Ok(ServicePort::new(port));
        }
        thread::sleep(Duration::from_millis(50 * (1 << attempt)));
    }
    Err(ClientError::ServiceUnavailable)
}

unsafe fn send_request(payload: &[u8]) -> Result<Vec<u8>, ClientError> {
    if payload.len() > MAX_MESSAGE_SIZE {
        return Err(ClientError::MessageTooLarge);
    }

    let service_port = unsafe { find_service_with_retry()? };
    let owned_reply_port = ReplyPort::allocate(1)?;
    let reply_port = owned_reply_port.name;

    let aligned_len = (payload.len() + 3) & !3;
    let mut message: InlineMessage = unsafe { zeroed() };
    message.header.remote_port = service_port.name;
    message.header.local_port = reply_port;
    message.header.id = reply_port as i32;
    message.header.bits = message_bits(MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND);
    message.header.size = (size_of::<MachMessageHeader>() + aligned_len) as u32;
    unsafe {
        copy_nonoverlapping(payload.as_ptr(), message.data.as_mut_ptr(), payload.len());
    }

    let result = unsafe {
        mach_msg(
            &mut message.header,
            MACH_SEND_MSG,
            message.header.size,
            0,
            0,
            0,
            0,
        )
    };
    if result != KERN_SUCCESS {
        return Err(ClientError::Mach {
            operation: "mach_msg(send)",
            code: result,
        });
    }

    unsafe { receive_message(reply_port) }
}

unsafe fn receive_message(reply_port: MachPort) -> Result<Vec<u8>, ClientError> {
    let mut buffer: ReceiveBuffer = unsafe { zeroed() };
    let result = unsafe {
        mach_msg(
            &mut buffer.message.header,
            MACH_RCV_MSG,
            0,
            size_of::<ReceiveBuffer>() as u32,
            reply_port,
            0,
            0,
        )
    };
    if result != KERN_SUCCESS {
        return Err(ClientError::Mach {
            operation: "mach_msg(receive)",
            code: result,
        });
    }

    let payload_len =
        buffer.message.header.size.saturating_sub(size_of::<MachMessageHeader>() as u32) as usize;
    if payload_len > MAX_MESSAGE_SIZE {
        unsafe { mach_msg_destroy(&mut buffer.message.header) };
        return Err(ClientError::MessageTooLarge);
    }
    let payload = buffer.message.data[..payload_len].to_vec();
    unsafe { mach_msg_destroy(&mut buffer.message.header) };
    Ok(payload)
}

const fn message_bits(remote: u32, local: u32) -> u32 {
    remote | (local << 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nul_terminated_response_with_alignment_padding() {
        let response: RiniResponse =
            parse_json_payload(b"{\"data\":true}\0\0\0", "response").unwrap();
        assert_eq!(response, RiniResponse::Success { data: serde_json::Value::Bool(true) });
    }

    #[test]
    fn rejects_payload_without_nul() {
        let result = parse_json_payload::<RiniResponse>(b"{\"data\":true}", "response");
        assert!(matches!(result, Err(ClientError::MissingTerminator { .. })));
    }

    #[test]
    fn request_uses_the_existing_wire_format() {
        let request = RiniRequest::GetWindows { space_id: Some(7) };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({ "get_windows": { "space_id": 7 } })
        );
    }

    #[test]
    fn response_is_untagged() {
        let response = RiniResponse::Success {
            data: serde_json::json!({ "ok": true }),
        };
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({ "data": { "ok": true } })
        );
    }
}
