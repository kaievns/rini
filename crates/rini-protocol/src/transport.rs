use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{EventKind, RiniCommand, WindowId};

/// A request accepted by Rini's Mach IPC server.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiniRequest {
    GetWorkspaces {
        space_id: Option<u64>,
    },
    GetDisplays,
    GetWindows {
        space_id: Option<u64>,
    },
    GetWindowInfo {
        window_id: WindowId,
    },
    GetLayoutState {
        space_id: Option<u64>,
        workspace_id: Option<usize>,
    },
    GetWorkspaceLayouts {
        space_id: Option<u64>,
        workspace_id: Option<usize>,
    },
    GetApplications,
    GetMetrics,
    /// One-shot dump of everything needed to reason about strips and displays.
    GetDiagnostics,
    GetConfig,
    ExecuteCommand {
        command: RiniCommand,
    },
    Subscribe {
        event: EventKind,
    },
    Unsubscribe {
        event: EventKind,
    },
    SubscribeCli {
        event: EventKind,
        command: String,
        args: Vec<String>,
    },
    UnsubscribeCli {
        event: EventKind,
    },
    ListCliSubscriptions,
}

/// The response envelope returned by Rini's Mach IPC server.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RiniResponse<T = Value> {
    Success { data: T },
    Error { error: Value },
}

impl<T> RiniResponse<T> {
    pub fn into_result(self) -> Result<T, Value> {
        match self {
            Self::Success { data } => Ok(data),
            Self::Error { error } => Err(error),
        }
    }
}

/// The compatibility response type for callers that intentionally want raw
/// JSON values.
pub type JsonRiniResponse = RiniResponse<Value>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LayoutCommand;

    #[test]
    fn request_uses_typed_command_wire_shape() {
        let request = RiniRequest::ExecuteCommand {
            command: RiniCommand::Layout(LayoutCommand::NextWindow),
        };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "execute_command": { "command": { "layout": "next_window" } }
            })
        );
    }

    #[test]
    fn layout_query_allows_the_server_to_select_the_active_space() {
        let request = RiniRequest::GetLayoutState {
            space_id: None,
            workspace_id: None,
        };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "get_layout_state": { "space_id": null, "workspace_id": null }
            })
        );
        assert_eq!(
            serde_json::from_value::<RiniRequest>(serde_json::json!({ "get_layout_state": {} }))
                .unwrap(),
            RiniRequest::GetLayoutState {
                space_id: None,
                workspace_id: None,
            }
        );
    }

    #[test]
    fn typed_response_decodes_shared_query_types() {
        let response: RiniResponse<Vec<crate::WorkspaceData>> =
            serde_json::from_value(serde_json::json!({ "data": [{
                "id": "workspace-1",
                "index": 0,
                "name": "main",
                "layout_mode": "bsp",
                "is_active": true,
                "window_count": 0,
                "windows": []
            }] }))
            .unwrap();

        assert_eq!(response.into_result().unwrap()[0].name, "main");
    }

    #[test]
    fn legacy_stringified_reactor_commands_still_decode() {
        let request: RiniRequest = serde_json::from_value(serde_json::json!({
            "execute_command": {
                "command": "{\"Reactor\":{\"switch_to_workspace\":5}}",
                "args": []
            }
        }))
        .unwrap();

        assert_eq!(
            request,
            RiniRequest::ExecuteCommand {
                command: RiniCommand::Layout(LayoutCommand::SwitchToWorkspace(5)),
            }
        );
    }

    #[test]
    fn legacy_window_id_strings_still_decode() {
        let request: RiniRequest = serde_json::from_value(serde_json::json!({
            "get_window_info": { "window_id": "WindowId { pid: 42, idx: 7 }" }
        }))
        .unwrap();

        assert_eq!(
            request,
            RiniRequest::GetWindowInfo {
                window_id: WindowId::new(42, 7).unwrap(),
            }
        );
    }
}
