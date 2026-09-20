//! The Mach IPC server: decodes `RiniRequest`s, answers them through a [`Backend`] the window
//! manager implements, and fans `RiniEvent`s out to subscribed clients and CLI hooks.

pub mod cli_exec;
pub mod client;
pub mod mach;
pub mod protocol;
pub mod subscriptions;

pub use protocol::*;

use std::ffi::c_char;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::{error, info, trace};


pub use client::{ClientError as RiniMachClientError, RiniMachClient, RiniMachSubscription};
use crate::subscriptions::SharedServerState;
use mach::{is_mach_server_registered, mach_server_run, send_mach_reply};
use rini_mach_sys::mach_msg_header_t;

type ClientPort = u32;

/// What the window manager answers over IPC. `rini-wm` implements it for its reactor handle;
/// every method blocks the Mach server thread until the reactor replies.
pub trait Backend: Send + 'static {
    fn workspaces(&self, space: Option<u64>) -> Vec<crate::protocol::WorkspaceData>;
    fn windows(&self, space: Option<u64>) -> Vec<crate::protocol::WindowData>;
    fn window(&self, window: WindowId) -> Option<crate::protocol::WindowData>;
    fn displays(&self) -> Vec<crate::protocol::DisplayData>;
    fn layout_state(
        &self,
        space: Option<u64>,
        workspace: Option<usize>,
    ) -> Option<crate::protocol::LayoutStateData>;
    fn workspace_layouts(
        &self,
        space: Option<u64>,
        workspace: Option<usize>,
    ) -> Vec<crate::protocol::WorkspaceLayoutData>;
    fn applications(&self) -> Vec<crate::protocol::ApplicationData>;
    fn metrics(&self) -> serde_json::Value;
    fn diagnostics(&self) -> crate::protocol::DiagnosticsData;
    /// Queue a command; `Err` only when the reactor is gone.
    fn execute(&self, command: crate::protocol::Command) -> Result<(), String>;
    /// The live configuration, serialised. `Err` when the config actor did not answer.
    fn config(&self) -> Result<serde_json::Value, String>;
    /// Apply a config command. Outer `Err` when the config actor did not answer; inner `Err` is
    /// the actor's own rejection.
    fn apply_config(&self, command: ConfigCommand) -> Result<Result<(), String>, String>;
}

pub fn run_mach_server<B: Backend>(reactor: B) -> Result<SharedServerState, String> {
    if is_mach_server_registered() {
        return Err(
            "Another Rini instance is already running; quit it before starting another.".into(),
        );
    }
    info!("Spawning background Mach server thread and returning SharedServerState");

    let shared_state: SharedServerState = std::sync::Arc::new(parking_lot::RwLock::new(
        crate::subscriptions::ServerState::new(),
    ));

    let thread_state = shared_state.clone();
    std::thread::spawn(move || {
        let handler = MachHandler::new(reactor, thread_state.clone());
        unsafe {
            mach_server_run(
                Box::into_raw(Box::new(handler)) as *mut _,
                handle_mach_request_c::<B>,
            );
        }
    });

    Ok(shared_state)
}

struct MachHandler<B: Backend> {
    reactor: B,
    server_state: SharedServerState,
}

impl<B: Backend> MachHandler<B> {
    fn new(reactor: B, server_state: SharedServerState) -> Self {
        Self { reactor, server_state }
    }


    fn handle_request(&self, request: RiniRequest, client_port: ClientPort) -> RiniResponse {
        trace!("Handling request: {:?} from client {}", request, client_port);

        match request {
            RiniRequest::Subscribe { event } => {
                let state = self.server_state.read();
                state.subscribe_client(client_port, event.to_string());
                RiniResponse::Success {
                    data: serde_json::json!({ "subscribed": event.to_string() }),
                }
            }
            RiniRequest::Unsubscribe { event } => {
                let state = self.server_state.read();
                state.unsubscribe_client(client_port, event.to_string());
                RiniResponse::Success {
                    data: serde_json::json!({ "unsubscribed": event.to_string() }),
                }
            }
            RiniRequest::SubscribeCli { event, command, args } => {
                let state = self.server_state.read();
                state.subscribe_cli(event.to_string(), command.clone(), args.clone());
                RiniResponse::Success {
                    data: serde_json::json!({
                        "cli_subscribed": event.to_string(),
                        "command": command,
                        "args": args
                    }),
                }
            }
            RiniRequest::UnsubscribeCli { event } => {
                let state = self.server_state.read();
                state.unsubscribe_cli(event.to_string());
                RiniResponse::Success {
                    data: serde_json::json!({ "cli_unsubscribed": event.to_string() }),
                }
            }
            RiniRequest::ListCliSubscriptions => {
                let state = self.server_state.read();
                let data = state.list_cli_subscriptions();
                RiniResponse::Success { data }
            }

            RiniRequest::GetWorkspaces { space_id } => {
                success(self.reactor.workspaces(space_id))
            }
            RiniRequest::GetDisplays => success(self.reactor.displays()),
            RiniRequest::GetWindows { space_id } => {
                success(self.reactor.windows(space_id))
            }

            RiniRequest::GetWindowInfo { window_id } => {
                match self.reactor.window(window_id) {
                    Some(window) => success(window),
                    None => RiniResponse::Error {
                        error: serde_json::json!({ "message": "Window not found" }),
                    },
                }
            }

            RiniRequest::GetLayoutState { space_id, workspace_id } => {
                match self.reactor.layout_state(space_id, workspace_id) {
                    Some(layout_state) => success(layout_state),
                    None => RiniResponse::Error {
                        error: serde_json::json!({ "message": "Space or workspace not found" }),
                    },
                }
            }
            RiniRequest::GetWorkspaceLayouts { space_id, workspace_id } => success(
                self.reactor.workspace_layouts(space_id, workspace_id),
            ),
            RiniRequest::GetApplications => success(self.reactor.applications()),
            RiniRequest::GetMetrics => RiniResponse::Success { data: self.reactor.metrics() },
            RiniRequest::GetDiagnostics => success(self.reactor.diagnostics()),

            RiniRequest::GetConfig => {
                match self.reactor.config() {
                    Ok(value) => RiniResponse::Success { data: value },
                    Err(e) => {
                        error!("{}", e);
                        RiniResponse::Error {
                            error: serde_json::json!({ "message": "Failed to get config response", "details": format!("{}", e) }),
                        }
                    }
                }
            }

            RiniRequest::ExecuteCommand { command } => match command {
                crate::protocol::RiniCommand::Config(command) => match decode_protocol(command) {
                    Ok(command) => match self.reactor.apply_config(command) {
                        Ok(Ok(())) => RiniResponse::Success {
                            data: serde_json::json!("Config applied successfully"),
                        },
                        Ok(Err(msg)) => RiniResponse::Error {
                            error: serde_json::json!({ "message": msg }),
                        },
                        Err(e) => RiniResponse::Error {
                            error: serde_json::json!({ "message": format!("Failed to apply config: {}", e) }),
                        },
                    },
                    Err(e) => RiniResponse::Error {
                        error: serde_json::json!({ "message": format!("Invalid config command: {}", e) }),
                    },
                },
                crate::protocol::RiniCommand::Layout(command) => {
                    self.send_typed_reactor_command(command)
                }
                crate::protocol::RiniCommand::Metrics(command) => {
                    self.send_typed_reactor_command(command)
                }
                crate::protocol::RiniCommand::Reactor(command) => {
                    self.send_typed_reactor_command(command)
                }
            },
        }
    }

    fn send_typed_reactor_command<T>(&self, command: T) -> RiniResponse
    where
        T: Serialize,
    {
        let command = match decode_protocol(command) {
            Ok(command) => command,
            Err(e) => {
                return RiniResponse::Error {
                    error: serde_json::json!({ "message": format!("Invalid command format: {}", e) }),
                };
            }
        };
        if let Err(e) = self.reactor.execute(command) {
            error!("Failed to send command to reactor: {}", e);
            return RiniResponse::Error {
                error: serde_json::json!({ "message": "Failed to execute command", "details": e }),
            };
        }

        RiniResponse::Success {
            data: serde_json::json!("Command executed successfully"),
        }
    }
}

fn success<T: Serialize>(data: T) -> RiniResponse {
    match serde_json::to_value(data) {
        Ok(data) => RiniResponse::Success { data },
        Err(e) => RiniResponse::Error {
            error: serde_json::json!({ "message": "Failed to serialize response", "details": e.to_string() }),
        },
    }
}

fn decode_protocol<T, U>(value: T) -> Result<U, serde_json::Error>
where
    T: Serialize,
    U: DeserializeOwned,
{
    serde_json::from_value(serde_json::to_value(value)?)
}

unsafe extern "C" fn handle_mach_request_c<B: Backend>(
    context: *mut std::ffi::c_void,
    message: *mut c_char,
    len: u32,
    original_msg: *mut mach_msg_header_t,
) {
    if context.is_null() {
        error!("Invalid context pointer");
        return;
    }
    if message.is_null() || len == 0 {
        return;
    }

    let handler = unsafe { &*(context as *const MachHandler<B>) };
    let message_slice = unsafe { std::slice::from_raw_parts(message as *const u8, len as usize) };

    let trimmed_slice = if let Some(pos) = message_slice.iter().position(|&b| b == 0) {
        &message_slice[..pos]
    } else {
        message_slice
    };

    let message_str = match std::str::from_utf8(trimmed_slice) {
        Ok(s) => s,
        Err(e) => {
            let lossy = String::from_utf8_lossy(trimmed_slice);
            error!(
                "Invalid UTF-8 in message after trimming NULs: {}. Contents (lossy): {}",
                e, lossy
            );
            return;
        }
    };

    let client_port = unsafe { (*original_msg).msgh_remote_port };

    let request: RiniRequest = match serde_json::from_str(message_str) {
        Ok(req) => req,
        Err(e) => {
            error!("Failed to parse request: {}", e);
            let error_response = RiniResponse::Error {
                error: serde_json::json!({ "message": format!("Invalid request format: {}", e) }),
            };
            send_response(original_msg, &error_response);
            return;
        }
    };

    let response = handler.handle_request(request, client_port);
    send_response(original_msg, &response);
}

fn send_response(original_msg: *mut mach_msg_header_t, response: &RiniResponse) {
    let mut response_json = serde_json::to_vec(response).unwrap();

    if response_json.last().copied() != Some(0) {
        response_json.push(0);
    }

    unsafe {
        if !send_mach_reply(
            original_msg,
            response_json.as_ptr() as *mut c_char,
            response_json.len() as u32,
        ) {
            error!(
                "Failed to send mach reply for message id {}",
                if original_msg.is_null() {
                    -1
                } else {
                    (*original_msg).msgh_id
                }
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::protocol::{LayoutCommand, RiniCommand};

    use super::*;

    #[derive(Default)]
    struct Fake {
        executed: Mutex<Vec<crate::protocol::Command>>,
    }

    impl Backend for std::sync::Arc<Fake> {
        fn workspaces(&self, space: Option<u64>) -> Vec<crate::protocol::WorkspaceData> {
            vec![crate::protocol::WorkspaceData {
                id: "ws".into(),
                index: 0,
                name: format!("space {:?}", space),
                is_active: true,
                window_count: 0,
                windows: vec![],
            }]
        }
        fn windows(&self, _: Option<u64>) -> Vec<crate::protocol::WindowData> {
            vec![]
        }
        fn window(&self, window: WindowId) -> Option<crate::protocol::WindowData> {
            (window.pid == 1).then(|| crate::protocol::WindowData {
                id: window.into(),
                title: "t".into(),
                frame: crate::protocol::Rect {
                    origin: crate::protocol::Point { x: 0.0, y: 0.0 },
                    size: crate::protocol::Size { width: 1.0, height: 1.0 },
                },
                is_floating: false,
                is_focused: false,
                bundle_id: None,
                app_name: None,
                window_server_id: None,
            })
        }
        fn displays(&self) -> Vec<crate::protocol::DisplayData> {
            vec![]
        }
        fn layout_state(&self, _: Option<u64>, _: Option<usize>) -> Option<crate::protocol::LayoutStateData> {
            None
        }
        fn workspace_layouts(&self, _: Option<u64>, _: Option<usize>) -> Vec<crate::protocol::WorkspaceLayoutData> {
            vec![]
        }
        fn applications(&self) -> Vec<crate::protocol::ApplicationData> {
            vec![]
        }
        fn metrics(&self) -> serde_json::Value {
            serde_json::json!({ "m": 1 })
        }
        fn diagnostics(&self) -> crate::protocol::DiagnosticsData {
            crate::protocol::DiagnosticsData { spaces: vec![], census: vec![], orphaned_workspaces: vec![], stale_homes: vec![], windows_managed: 0 }
        }
        fn execute(&self, command: crate::protocol::Command) -> Result<(), String> {
            self.executed.lock().unwrap().push(command);
            Ok(())
        }
        fn config(&self) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({ "fake": true }))
        }
        fn apply_config(&self, _: ConfigCommand) -> Result<Result<(), String>, String> {
            Ok(Ok(()))
        }
    }

    fn handler() -> (MachHandler<std::sync::Arc<Fake>>, std::sync::Arc<Fake>) {
        let fake = std::sync::Arc::new(Fake::default());
        let state: SharedServerState =
            std::sync::Arc::new(parking_lot::RwLock::new(subscriptions::ServerState::new()));
        (MachHandler::new(fake.clone(), state), fake)
    }

    #[test]
    fn queries_are_answered_from_the_backend() {
        let (handler, _) = handler();
        let RiniResponse::Success { data } =
            handler.handle_request(RiniRequest::GetWorkspaces { space_id: Some(4) }, 1)
        else {
            panic!("expected success")
        };
        assert_eq!(data[0]["name"], "space Some(4)");
        let RiniResponse::Success { data } = handler.handle_request(RiniRequest::GetMetrics, 1)
        else {
            panic!("expected success")
        };
        assert_eq!(data["m"], 1);
    }

    #[test]
    fn a_missing_window_is_an_error_and_a_zero_index_never_reaches_the_handler() {
        let (handler, _) = handler();
        let unknown = crate::protocol::WindowId::new(2, 5).unwrap();
        assert!(matches!(
            handler.handle_request(RiniRequest::GetWindowInfo { window_id: unknown }, 1),
            RiniResponse::Error { .. }
        ));
        assert!(serde_json::from_str::<crate::protocol::WindowId>(r#"{"pid":1,"idx":0}"#).is_err());
        assert!(matches!(
            handler.handle_request(
                RiniRequest::GetWindowInfo { window_id: crate::protocol::WindowId::new(1, 5).unwrap() },
                1
            ),
            RiniResponse::Success { .. }
        ));
    }

    #[test]
    fn layout_commands_reach_the_backend_decoded() {
        let (handler, fake) = handler();
        let response = handler.handle_request(
            RiniRequest::ExecuteCommand {
                command: RiniCommand::Layout(LayoutCommand::SwitchToWorkspace(2)),
            },
            1,
        );
        assert!(matches!(response, RiniResponse::Success { .. }));
        assert_eq!(
            fake.executed.lock().unwrap().as_slice(),
            [crate::protocol::Command::Layout(LayoutCommand::SwitchToWorkspace(2))]
        );
    }
}
