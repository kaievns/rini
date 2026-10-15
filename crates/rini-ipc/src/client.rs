//! A small, synchronous Rust client for Rini's Mach IPC API.
//!
//! `rini-cli` is the one client: it sends a request and blocks for the response.

use std::ffi::{CStr, CString, c_int, c_void};
use std::mem::{size_of, zeroed};
use std::ptr::copy_nonoverlapping;
use std::thread;
use std::time::Duration;

use rini_mach_sys::{
    KERN_SUCCESS, MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND, MACH_MSGH_BITS,
    MACH_PORT_LIMITS_INFO, MACH_PORT_LIMITS_INFO_COUNT, MACH_PORT_RIGHT_RECEIVE, MACH_RCV_MSG,
    MACH_SEND_MSG, MAX_MESSAGE_SIZE_USIZE, TASK_BOOTSTRAP_PORT, bootstrap_look_up, kern_return_t,
    mach_buffer_t, mach_message_t, mach_msg, mach_msg_destroy, mach_msg_header_t,
    mach_port_allocate, mach_port_deallocate, mach_port_insert_right, mach_port_limits,
    mach_port_mod_refs, mach_port_set_attributes, mach_port_t, mach_task_self,
    task_get_special_port,
};
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::protocol::{RiniRequest, RiniResponse};

const MAX_MESSAGE_SIZE: usize = MAX_MESSAGE_SIZE_USIZE;
const DEFAULT_SERVICE_NAME: &str = "git.kaievns.rini";

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("Rini's Mach service is not registered")]
    ServiceUnavailable,
    #[error("invalid Mach service name")]
    InvalidServiceName,
    #[error("Mach operation {operation} failed with code {code}")]
    Mach {
        operation: &'static str,
        code: kern_return_t,
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

#[derive(Debug)]
struct ReplyPort {
    name: mach_port_t,
}

struct ServicePort {
    name: mach_port_t,
}

impl ServicePort {
    fn new(name: mach_port_t) -> Self {
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

            let limits = mach_port_limits { mpl_qlimit: queue_limit };
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

unsafe fn lookup_service(name: &CStr) -> Result<mach_port_t, ClientError> {
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
    let mut message: mach_message_t = unsafe { zeroed() };
    message.header.msgh_remote_port = service_port.name;
    message.header.msgh_local_port = reply_port;
    message.header.msgh_id = reply_port as i32;
    message.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND);
    message.header.msgh_size = (size_of::<mach_msg_header_t>() + aligned_len) as u32;
    unsafe {
        copy_nonoverlapping(payload.as_ptr(), message.data.as_mut_ptr(), payload.len());
    }

    let result = unsafe {
        mach_msg(
            &mut message.header,
            MACH_SEND_MSG,
            message.header.msgh_size,
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

unsafe fn receive_message(reply_port: mach_port_t) -> Result<Vec<u8>, ClientError> {
    let mut buffer: mach_buffer_t = unsafe { zeroed() };
    let result = unsafe {
        mach_msg(
            &mut buffer.message.header,
            MACH_RCV_MSG,
            0,
            size_of::<mach_buffer_t>() as u32,
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
        buffer.message.header.msgh_size.saturating_sub(size_of::<mach_msg_header_t>() as u32) as usize;
    if payload_len > MAX_MESSAGE_SIZE {
        unsafe { mach_msg_destroy(&mut buffer.message.header) };
        return Err(ClientError::MessageTooLarge);
    }
    let payload = buffer.message.data[..payload_len].to_vec();
    unsafe { mach_msg_destroy(&mut buffer.message.header) };
    Ok(payload)
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
