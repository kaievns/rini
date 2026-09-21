//! What the IPC server is allowed to ask the application: reactor queries and commands, and the
//! live configuration. Ids cross this boundary as wire types; the conversion happens here.
use std::time::Duration;

use r#continue::continuation;
use rini_config::actor as config_actor;
use rini_core::ids::SpaceId;
use rini_ipc::protocol::{self, ConfigCommand};
use rini_runloop::dispatch::block_on;
use rini_core::ids::WindowId;

use crate::actor::reactor::{Event, ReactorHandle};

pub struct IpcBackend {
    reactor: ReactorHandle,
    config_tx: config_actor::Sender,
}

impl IpcBackend {
    pub fn new(reactor: ReactorHandle, config_tx: config_actor::Sender) -> Self {
        Self { reactor, config_tx }
    }

    fn ask_config<T: Send + 'static>(
        &self,
        make_event: impl FnOnce(r#continue::Sender<T>) -> config_actor::Event,
    ) -> Result<T, String> {
        let (tx, fut) = continuation::<T>();
        if let Err(e) = self.config_tx.try_send(make_event(tx)) {
            let msg = format!("{e}");
            let tokio::sync::mpsc::error::SendError((_span, event)) = e;
            // A continuation sender dropped without a reply panics its receiver; leak it instead.
            match event {
                config_actor::Event::QueryConfig(response) => std::mem::forget(response),
                config_actor::Event::ApplyConfig { response, .. } => std::mem::forget(response),
            }
            return Err(format!("Failed to send config query: {msg}"));
        }
        block_on(fut, Duration::from_secs(5)).map_err(|e| format!("Failed to get response: {e}"))
    }
}

impl rini_ipc::Backend for IpcBackend {
    fn workspaces(&self, space: Option<u64>) -> Vec<protocol::WorkspaceData> {
        self.reactor
            .query_workspaces(space.map(SpaceId::new))
            .into_iter()
            .map(Into::into)
            .collect()
    }
    fn windows(&self, space: Option<u64>) -> Vec<protocol::WindowData> {
        self.reactor.query_windows(space.map(SpaceId::new)).into_iter().map(Into::into).collect()
    }
    fn window(&self, window: protocol::WindowId) -> Option<protocol::WindowData> {
        self.reactor.query_window_info(WindowId::new(window.pid, window.idx)).map(Into::into)
    }
    fn displays(&self) -> Vec<protocol::DisplayData> {
        self.reactor.query_displays().into_iter().map(Into::into).collect()
    }
    fn layout_state(
        &self,
        space: Option<u64>,
        workspace: Option<usize>,
    ) -> Option<protocol::LayoutStateData> {
        self.reactor.query_layout_state(space, workspace)
    }
    fn workspace_layouts(
        &self,
        space: Option<u64>,
        workspace: Option<usize>,
    ) -> Vec<protocol::WorkspaceLayoutData> {
        self.reactor.query_workspace_layouts(space.map(SpaceId::new), workspace)
    }
    fn applications(&self) -> Vec<protocol::ApplicationData> {
        self.reactor.query_applications()
    }
    fn metrics(&self) -> serde_json::Value {
        self.reactor.query_metrics()
    }
    fn diagnostics(&self) -> protocol::DiagnosticsData {
        self.reactor.query_diagnostics()
    }
    fn execute(&self, command: protocol::Command) -> Result<(), String> {
        self.reactor.try_send(Event::Command(command)).map_err(|e| e.to_string())
    }
    fn config(&self) -> Result<serde_json::Value, String> {
        let config = self.ask_config(config_actor::Event::QueryConfig)?;
        serde_json::to_value(&config).map_err(|e| format!("Failed to serialize config: {e}"))
    }
    fn apply_config(&self, command: ConfigCommand) -> Result<Result<(), String>, String> {
        self.ask_config(|response| config_actor::Event::ApplyConfig { cmd: command, response })
    }
}
