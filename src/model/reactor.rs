use objc2_core_foundation::CGRect;
pub use rini_protocol::{DisplaySelector, ReactorCommand};

use rini_windows::app::AppInfo;
use rini_windows::app_actor::AppThreadHandle;
use rini_windows::ids::{WindowId, pid_t};
use crate::model::WindowStore;
use rini_displays::ids::SpaceId;

/// All mutable domain state is owned by the reactor thread.
///
/// Workspace topology is still carried by the layout coordinator during this
/// migration, but window identity, native-space observations, and workspace
/// assignments have one explicit owner here. Cross-store operations receive
/// this store by reference instead of retaining an alias to it.
#[derive(Debug, Default)]
pub struct RiniState {
    pub windows: WindowStore,
}


pub use rini_config::commands::Command;

#[derive(Debug, Clone)]
pub struct DragSession {
    pub(crate) window: WindowId,
    pub(crate) last_frame: CGRect,
    pub(crate) origin_space: Option<SpaceId>,
    pub(crate) settled_space: Option<SpaceId>,
    pub(crate) layout_dirty: bool,
}

#[derive(Debug, Clone)]
pub enum DragState {
    Inactive,
    Active {
        session: DragSession,
    },
    PendingSwap {
        session: DragSession,
        target: WindowId,
    },
}

#[derive(Debug, Clone)]
pub enum MissionControlState {
    Inactive,
    Active,
    Transitioning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuState {
    Closed,
    Open(pid_t),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceSwitchState {
    Inactive,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSwitchOrigin {
    Manual,
    Auto,
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleCleanupState {
    Enabled,
    Suppressed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RefocusState {
    None,
    Pending(SpaceId),
}

#[derive(Debug)]
pub(crate) struct AppState {
    #[allow(unused)]
    pub(crate) info: AppInfo,
    pub(crate) handle: AppThreadHandle,
}

pub use rini_windows::state::{WindowFilter, WindowState};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReactorError {
    #[error("App communication failed: {0}")]
    AppCommunicationFailed(#[from] tokio::sync::mpsc::error::SendError<rini_windows::app_actor::Request>),
    #[error("Raise manager communication failed: {0}")]
    RaiseManagerCommunicationFailed(
        #[from] tokio::sync::mpsc::error::SendError<crate::actor::raise_manager::Event>,
    ),
}

#[cfg(test)]
mod tests {
    use rini_protocol::{RestoreScope, RestoreSource};

    use super::*;

    #[test]
    fn legacy_restore_command_defaults_to_portable_source_policy() {
        let mut serialized = serde_json::to_value(ReactorCommand::RestoreLayout {
            path: "layout.ron".into(),
            scope: RestoreScope::Workspace,
            source: RestoreSource::CurrentSpace,
        })
        .unwrap();
        serialized
            .get_mut("restore_layout")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("source");

        let restored: ReactorCommand = serde_json::from_value(serialized).unwrap();

        assert_eq!(
            restored,
            ReactorCommand::RestoreLayout {
                path: "layout.ron".into(),
                scope: RestoreScope::Workspace,
                source: RestoreSource::SavedActiveSpace,
            }
        );
    }
}
