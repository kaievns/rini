pub mod engine;
mod floating;
pub mod systems;
pub mod utils;
mod workspaces;

pub use engine::{
    EventResponse, LayoutCommand, LayoutEngine, LayoutEvent, LayoutEventOutcome, RestoreReport,
    RestoreRequest, RestoreScope, RestoreSource, RestoreWarning,
};
pub(crate) use floating::FloatingManager;
pub use rini_protocol::{Direction, ResizeOrientation};
pub(crate) use systems::LayoutId;
pub use systems::{LayoutSystem, LayoutSystemKind, ScrollingLayoutSystem};
pub(crate) use workspaces::WorkspaceLayouts;

pub use crate::model::virtual_workspace::{VirtualWorkspaceId, WorkspaceStats, WorkspaceStore};
