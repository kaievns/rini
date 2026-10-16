//! The Mach IPC server: decodes `RiniRequest`s and answers them through a [`Backend`] the window
//! manager implements.

pub mod client;
pub mod mach;
pub mod protocol;

pub use client::RiniMachClient;
use mach::{is_mach_server_registered, mach_server_run};
pub use protocol::*;
use serde::Serialize;
use tracing::{error, info, trace};

/// What the window manager answers over IPC. The daemon implements it in `src/app/api/backend.rs`.
/// Every method is called on the main run loop and blocks the main thread until it returns, so none
/// may wait on the main thread.
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
    info!("Registering the Mach service on a background thread, to be served on the main run loop");

    std::thread::spawn(move || {
        let handler = MachHandler { reactor };
        mach_server_run(Box::new(move |payload| handler.answer(payload)));
    });

    Ok(())
}

struct MachHandler<B: Backend> {
    reactor: B,
}

impl<B: Backend> MachHandler<B> {
    /// The NUL-terminated JSON reply to a request's `payload`, read up to its first NUL; `None`, so
    /// no reply, when the payload is empty or not UTF-8.
    fn answer(&self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.is_empty() {
            return None;
        }
        let trimmed = match payload.iter().position(|&b| b == 0) {
            Some(pos) => &payload[..pos],
            None => payload,
        };
        let message = match std::str::from_utf8(trimmed) {
            Ok(message) => message,
            Err(e) => {
                let lossy = String::from_utf8_lossy(trimmed);
                error!(
                    "Invalid UTF-8 in message after trimming NULs: {}. Contents (lossy): {}",
                    e, lossy
                );
                return None;
            }
        };
        let response = match serde_json::from_str(message) {
            Ok(request) => self.handle_request(request),
            Err(e) => {
                error!("Failed to parse request: {}", e);
                RiniResponse::Error {
                    error: serde_json::json!({ "message": format!("Invalid request format: {}", e) }),
                }
            }
        };
        let mut response = serde_json::to_vec(&response).unwrap();
        if response.last().copied() != Some(0) {
            response.push(0);
        }
        Some(response)
    }

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

    #[test]
    fn a_payload_is_answered_nul_terminated_and_one_rini_cannot_read_is_not_answered() {
        let (handler, _) = handler();
        let reply = handler.answer(b"\"get_metrics\"\0\0\0").expect("a reply");
        let (json, nul) = reply.split_at(reply.len() - 1);
        assert_eq!(nul, b"\0");
        let reply: serde_json::Value = serde_json::from_slice(json).unwrap();
        assert_eq!(reply["data"]["m"], 1);

        let reply = handler.answer(b"nonsense\0").expect("an error reply");
        let reply: serde_json::Value = serde_json::from_slice(&reply[..reply.len() - 1]).unwrap();
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("Invalid request format")
        );

        assert_eq!(handler.answer(b""), None);
        assert_eq!(handler.answer(b"\xff\xfe\0"), None);
    }
}
