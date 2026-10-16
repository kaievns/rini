//! The Mach IPC server: decodes `RiniRequest`s and answers them through a [`Backend`] the window
//! manager implements.

pub mod client;
pub mod mach;
pub mod protocol;

use std::ffi::c_char;

pub use client::RiniMachClient;
use mach::{is_mach_server_registered, mach_server_run, send_mach_reply};
pub use protocol::*;
use rini_mach_sys::mach_msg_header_t;
use serde::Serialize;
use tracing::{error, info, trace};

/// What the window manager answers over IPC. The daemon implements it in `src/app/api/backend.rs`;
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
    fn applications(&self) -> Vec<crate::protocol::ApplicationData>;
    fn metrics(&self) -> serde_json::Value;
    fn diagnostics(&self) -> crate::protocol::DiagnosticsData;
    /// Queue a command; `Err` only when the reactor is gone.
    fn execute(&self, command: crate::protocol::Command) -> Result<(), String>;
    /// The live configuration, as the KDL a config file would hold. `Err` when the config actor
    /// did not answer.
    fn config(&self) -> Result<String, String>;
    /// Apply a config command. Outer `Err` when the config actor did not answer; inner `Err` is
    /// the actor's own rejection.
    fn apply_config(&self, command: ConfigCommand) -> Result<Result<(), String>, String>;
}

pub fn run_mach_server<B: Backend>(reactor: B) -> Result<(), String> {
    if is_mach_server_registered() {
        return Err(
            "Another Rini instance is already running; quit it before starting another.".into(),
        );
    }
    info!("Spawning background Mach server thread");

    std::thread::spawn(move || {
        let handler = MachHandler { reactor };
        unsafe {
            mach_server_run(
                Box::into_raw(Box::new(handler)) as *mut _,
                handle_mach_request_c::<B>,
            );
        }
    });

    Ok(())
}

struct MachHandler<B: Backend> {
    reactor: B,
}

impl<B: Backend> MachHandler<B> {
    fn handle_request(&self, request: RiniRequest) -> RiniResponse {
        trace!("Handling request: {:?}", request);

        match request {
            RiniRequest::GetWorkspaces { space_id } => success(self.reactor.workspaces(space_id)),
            RiniRequest::GetDisplays => success(self.reactor.displays()),
            RiniRequest::GetWindows { space_id } => success(self.reactor.windows(space_id)),

            RiniRequest::GetWindowInfo { window_id } => match self.reactor.window(window_id) {
                Some(window) => success(window),
                None => RiniResponse::Error {
                    error: serde_json::json!({ "message": "Window not found" }),
                },
            },

            RiniRequest::GetLayoutState { space_id, workspace_id } => {
                match self.reactor.layout_state(space_id, workspace_id) {
                    Some(layout_state) => success(layout_state),
                    None => RiniResponse::Error {
                        error: serde_json::json!({ "message": "Space or workspace not found" }),
                    },
                }
            }
            RiniRequest::GetApplications => success(self.reactor.applications()),
            RiniRequest::GetMetrics => RiniResponse::Success { data: self.reactor.metrics() },
            RiniRequest::GetDiagnostics => success(self.reactor.diagnostics()),

            RiniRequest::GetConfig => match self.reactor.config() {
                Ok(text) => RiniResponse::Success {
                    data: serde_json::Value::String(text),
                },
                Err(e) => {
                    error!("{}", e);
                    RiniResponse::Error {
                        error: serde_json::json!({ "message": "Failed to get config response", "details": e.to_string() }),
                    }
                }
            },

            RiniRequest::ExecuteCommand { command } => match command {
                RiniCommand::Config(command) => match self.reactor.apply_config(command) {
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
                RiniCommand::Layout(command) => self.execute(Command::Layout(command)),
                RiniCommand::Reactor(command) => self.execute(Command::Reactor(command)),
            },
        }
    }

    fn execute(&self, command: Command) -> RiniResponse {
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

    let response = handler.handle_request(request);
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

    use super::*;
    use crate::protocol::{LayoutCommand, RiniCommand};

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

        fn windows(&self, _: Option<u64>) -> Vec<crate::protocol::WindowData> { vec![] }

        fn window(&self, window: WindowId) -> Option<crate::protocol::WindowData> {
            (window.pid == 1).then(|| crate::protocol::WindowData {
                id: window,
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

        fn displays(&self) -> Vec<crate::protocol::DisplayData> { vec![] }

        fn layout_state(
            &self,
            _: Option<u64>,
            _: Option<usize>,
        ) -> Option<crate::protocol::LayoutStateData> {
            None
        }

        fn applications(&self) -> Vec<crate::protocol::ApplicationData> { vec![] }

        fn metrics(&self) -> serde_json::Value { serde_json::json!({ "m": 1 }) }

        fn diagnostics(&self) -> crate::protocol::DiagnosticsData {
            crate::protocol::DiagnosticsData {
                spaces: vec![],
                census: vec![],
                orphaned_workspaces: vec![],
                stale_homes: vec![],
                windows_managed: 0,
            }
        }

        fn execute(&self, command: crate::protocol::Command) -> Result<(), String> {
            self.executed.lock().unwrap().push(command);
            Ok(())
        }

        fn config(&self) -> Result<String, String> { Ok("fake true\n".into()) }

        fn apply_config(&self, _: ConfigCommand) -> Result<Result<(), String>, String> {
            Ok(Ok(()))
        }
    }

    fn handler() -> (MachHandler<std::sync::Arc<Fake>>, std::sync::Arc<Fake>) {
        let fake = std::sync::Arc::new(Fake::default());
        (MachHandler { reactor: fake.clone() }, fake)
    }

    #[test]
    fn queries_are_answered_from_the_backend() {
        let (handler, _) = handler();
        let RiniResponse::Success { data } =
            handler.handle_request(RiniRequest::GetWorkspaces { space_id: Some(4) })
        else {
            panic!("expected success")
        };
        assert_eq!(data[0]["name"], "space Some(4)");
        let RiniResponse::Success { data } = handler.handle_request(RiniRequest::GetMetrics) else {
            panic!("expected success")
        };
        assert_eq!(data["m"], 1);
    }

    /// `config get` is answered with the backend's KDL as one string, for the CLI to print as it is.
    #[test]
    fn the_config_is_answered_as_its_text() {
        let (handler, _) = handler();
        let RiniResponse::Success { data } = handler.handle_request(RiniRequest::GetConfig) else {
            panic!("expected success")
        };
        assert_eq!(data, serde_json::json!("fake true\n"));
    }

    #[test]
    fn a_missing_window_is_an_error_and_a_zero_index_never_reaches_the_handler() {
        let (handler, _) = handler();
        let unknown = crate::protocol::WindowId::new(2, 5).unwrap();
        assert!(matches!(
            handler.handle_request(RiniRequest::GetWindowInfo { window_id: unknown }),
            RiniResponse::Error { .. }
        ));
        assert!(serde_json::from_str::<crate::protocol::WindowId>(r#"{"pid":1,"idx":0}"#).is_err());
        assert!(matches!(
            handler.handle_request(RiniRequest::GetWindowInfo {
                window_id: crate::protocol::WindowId::new(1, 5).unwrap()
            }),
            RiniResponse::Success { .. }
        ));
    }

    #[test]
    fn layout_commands_reach_the_backend_decoded() {
        let (handler, fake) = handler();
        let response = handler.handle_request(RiniRequest::ExecuteCommand {
            command: RiniCommand::Layout(LayoutCommand::SwitchToWorkspace(2)),
        });
        assert!(matches!(response, RiniResponse::Success { .. }));
        assert_eq!(fake.executed.lock().unwrap().as_slice(), [
            crate::protocol::Command::Layout(LayoutCommand::SwitchToWorkspace(2))
        ]);
    }
}
