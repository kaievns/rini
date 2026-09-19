//! The Mach IPC server: decodes `RiniRequest`s, answers them through a [`Backend`] the window
//! manager implements, and fans `RiniEvent`s out to subscribed clients and CLI hooks.

use std::ffi::c_char;
use std::time::Duration;

use r#continue::continuation;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::{error, info, trace};

pub mod cli_exec;
pub mod subscriptions;

use rini_protocol::{RiniRequest, RiniResponse};
pub use rini_client::{ClientError as RiniMachClientError, RiniMachClient, RiniMachSubscription};
use rini_shared::ids::{SpaceId, WindowId};

use rini_config::actor as config_actor;
use crate::subscriptions::SharedServerState;
use rini_runloop::dispatch::block_on;
use rini_macos::mach::{
    is_mach_server_registered, mach_msg_header_t, mach_server_run, send_mach_reply,
};

type ClientPort = u32;

/// What the window manager answers over IPC. `rini-wm` implements it for its reactor handle;
/// every method blocks the Mach server thread until the reactor replies.
pub trait Backend: Send + 'static {
    fn workspaces(&self, space: Option<SpaceId>) -> Vec<rini_protocol::WorkspaceData>;
    fn windows(&self, space: Option<SpaceId>) -> Vec<rini_protocol::WindowData>;
    fn window(&self, window: WindowId) -> Option<rini_protocol::WindowData>;
    fn displays(&self) -> Vec<rini_protocol::DisplayData>;
    fn layout_state(
        &self,
        space: Option<u64>,
        workspace: Option<usize>,
    ) -> Option<rini_protocol::LayoutStateData>;
    fn workspace_layouts(
        &self,
        space: Option<SpaceId>,
        workspace: Option<usize>,
    ) -> Vec<rini_protocol::WorkspaceLayoutData>;
    fn applications(&self) -> Vec<rini_protocol::ApplicationData>;
    fn metrics(&self) -> serde_json::Value;
    fn diagnostics(&self) -> rini_protocol::DiagnosticsData;
    /// Queue a command; `Err` only when the reactor is gone.
    fn execute(&self, command: rini_config::Command) -> Result<(), String>;
}

pub fn run_mach_server<B: Backend>(
    reactor: B,
    config_tx: config_actor::Sender,
) -> Result<SharedServerState, String> {
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
        let handler = MachHandler::new(reactor, config_tx, thread_state.clone());
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
    config_tx: config_actor::Sender,
    server_state: SharedServerState,
}

impl<B: Backend> MachHandler<B> {
    fn new(reactor: B, config_tx: config_actor::Sender, server_state: SharedServerState) -> Self {
        Self {
            reactor,
            config_tx,
            server_state,
        }
    }

    fn forget_config_query_sender(event: config_actor::Event) {
        match event {
            config_actor::Event::QueryConfig(response) => std::mem::forget(response),
            config_actor::Event::ApplyConfig { response, .. } => std::mem::forget(response),
        }
    }

    fn perform_config_query<T>(
        &self,
        make_event: impl FnOnce(r#continue::Sender<T>) -> config_actor::Event,
    ) -> Result<T, String>
    where
        T: Send + 'static,
    {
        let (cont_tx, cont_fut) = continuation::<T>();
        let event = make_event(cont_tx);

        if let Err(e) = self.config_tx.try_send(event) {
            let msg = format!("{e}");
            let tokio::sync::mpsc::error::SendError((_span, event)) = e;
            Self::forget_config_query_sender(event);
            return Err(format!("Failed to send config query: {msg}"));
        }

        match block_on(cont_fut, Duration::from_secs(5)) {
            Ok(res) => Ok(res),
            Err(e) => Err(format!("Failed to get response: {}", e)),
        }
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
                success(self.reactor.workspaces(space_id.map(SpaceId::new)))
            }
            RiniRequest::GetDisplays => success(self.reactor.displays()),
            RiniRequest::GetWindows { space_id } => {
                success(self.reactor.windows(space_id.map(SpaceId::new)))
            }

            RiniRequest::GetWindowInfo { window_id } => {
                let window_id = WindowId::new(window_id.pid, window_id.idx);

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
                self.reactor.workspace_layouts(space_id.map(SpaceId::new), workspace_id),
            ),
            RiniRequest::GetApplications => success(self.reactor.applications()),
            RiniRequest::GetMetrics => RiniResponse::Success { data: self.reactor.metrics() },
            RiniRequest::GetDiagnostics => success(self.reactor.diagnostics()),

            RiniRequest::GetConfig => {
                match self.perform_config_query(|tx| config_actor::Event::QueryConfig(tx)) {
                    Ok(config) => match serde_json::to_value(&config) {
                        Ok(value) => RiniResponse::Success { data: value },
                        Err(e) => {
                            error!("Failed to serialize config: {}", e);
                            RiniResponse::Error {
                                error: serde_json::json!({ "message": "Failed to serialize config", "details": format!("{}", e) }),
                            }
                        }
                    },
                    Err(e) => {
                        error!("{}", e);
                        RiniResponse::Error {
                            error: serde_json::json!({ "message": "Failed to get config response", "details": format!("{}", e) }),
                        }
                    }
                }
            }

            RiniRequest::ExecuteCommand { command } => match command {
                rini_protocol::RiniCommand::Config(command) => match decode_protocol(command) {
                    Ok(command) => match self.perform_config_query(|tx| {
                        config_actor::Event::ApplyConfig { cmd: command, response: tx }
                    }) {
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
                rini_protocol::RiniCommand::Layout(command) => {
                    self.send_typed_reactor_command(command)
                }
                rini_protocol::RiniCommand::Metrics(command) => {
                    self.send_typed_reactor_command(command)
                }
                rini_protocol::RiniCommand::Reactor(command) => {
                    self.send_typed_reactor_command(command)
                }
            },
            _ => RiniResponse::Error {
                error: serde_json::json!({ "message": "Unsupported request" }),
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

    use rini_protocol::{LayoutCommand, RiniCommand};

    use super::*;

    #[derive(Default)]
    struct Fake {
        executed: Mutex<Vec<rini_config::Command>>,
    }

    impl Backend for std::sync::Arc<Fake> {
        fn workspaces(&self, space: Option<SpaceId>) -> Vec<rini_protocol::WorkspaceData> {
            vec![rini_protocol::WorkspaceData {
                id: "ws".into(),
                index: 0,
                name: format!("space {:?}", space.map(|s| s.get())),
                is_active: true,
                window_count: 0,
                windows: vec![],
            }]
        }
        fn windows(&self, _: Option<SpaceId>) -> Vec<rini_protocol::WindowData> {
            vec![]
        }
        fn window(&self, window: WindowId) -> Option<rini_protocol::WindowData> {
            (window.pid == 1).then(|| rini_protocol::WindowData {
                id: window.into(),
                title: "t".into(),
                frame: rini_protocol::Rect {
                    origin: rini_protocol::Point { x: 0.0, y: 0.0 },
                    size: rini_protocol::Size { width: 1.0, height: 1.0 },
                },
                is_floating: false,
                is_focused: false,
                bundle_id: None,
                app_name: None,
                window_server_id: None,
            })
        }
        fn displays(&self) -> Vec<rini_protocol::DisplayData> {
            vec![]
        }
        fn layout_state(&self, _: Option<u64>, _: Option<usize>) -> Option<rini_protocol::LayoutStateData> {
            None
        }
        fn workspace_layouts(&self, _: Option<SpaceId>, _: Option<usize>) -> Vec<rini_protocol::WorkspaceLayoutData> {
            vec![]
        }
        fn applications(&self) -> Vec<rini_protocol::ApplicationData> {
            vec![]
        }
        fn metrics(&self) -> serde_json::Value {
            serde_json::json!({ "m": 1 })
        }
        fn diagnostics(&self) -> rini_protocol::DiagnosticsData {
            rini_protocol::DiagnosticsData { spaces: vec![], census: vec![], orphaned_workspaces: vec![], stale_homes: vec![], windows_managed: 0 }
        }
        fn execute(&self, command: rini_config::Command) -> Result<(), String> {
            self.executed.lock().unwrap().push(command);
            Ok(())
        }
    }

    fn handler() -> (MachHandler<std::sync::Arc<Fake>>, std::sync::Arc<Fake>) {
        let fake = std::sync::Arc::new(Fake::default());
        let (config_tx, _config_rx) = rini_runloop::channel::channel();
        let state: SharedServerState =
            std::sync::Arc::new(parking_lot::RwLock::new(subscriptions::ServerState::new()));
        (MachHandler::new(fake.clone(), config_tx, state), fake)
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
        let unknown = rini_protocol::WindowId::new(2, 5).unwrap();
        assert!(matches!(
            handler.handle_request(RiniRequest::GetWindowInfo { window_id: unknown }, 1),
            RiniResponse::Error { .. }
        ));
        assert!(serde_json::from_str::<rini_protocol::WindowId>(r#"{"pid":1,"idx":0}"#).is_err());
        assert!(matches!(
            handler.handle_request(
                RiniRequest::GetWindowInfo { window_id: rini_protocol::WindowId::new(1, 5).unwrap() },
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
            [rini_config::Command::Layout(LayoutCommand::SwitchToWorkspace(2))]
        );
    }
}
