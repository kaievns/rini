//! Virtual workspaces and what they own. Windows are assigned to a
//! workspace and arranged on its strips (`crate::layout`) by `LayoutEngine`; the arrangement is
//! persisted to `layout.ron`, remembered per app across launches, and homed per display.
//! `WindowStore` is the catalogue from `crate::windows` plus each window's assignment.

pub mod broadcast;
pub mod domain;
pub mod engine;
pub mod settings;

pub use domain::app_rules::{AppRuleEffects, AppRuleResult};
pub use domain::display_affinity::DisplayAffinity;
pub use engine::{
    EventResponse, LayoutCommand, LayoutEngine, LayoutEvent, LayoutEventOutcome, RestoreReport,
    RestoreRequest, RestoreScope, RestoreSource, RestoreWarning,
};
pub use domain::floating::FloatingManager;
pub use domain::floating_position_store::FloatingPositionStore;
pub use domain::hidden_window_placement::{HiddenWindowPlacement, HideCorner};
pub use rini_ipc::protocol::{Direction, ResizeOrientation};
pub use crate::layout::{LayoutId, LayoutSystem, LayoutSystemKind, ScrollingLayoutSystem};
pub use domain::virtual_workspace::{VirtualWorkspace, VirtualWorkspaceId, WorkspaceStats, WorkspaceStore};
pub use domain::window_store::{
    PendingWindowOperation, WindowPlacement, WindowRecord, WindowStore, WindowVisibility,
    WindowWorkspaceInfo,
};
pub use domain::workspaces::WorkspaceLayouts;
