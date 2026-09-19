//! The layout domain: the scrolling strip, virtual workspaces, floating windows, app rules, the
//! window record store the layout is projected from, and `layout.ron` persistence.
//! See `docs/architecture.md`.

pub mod app_rules;
pub mod broadcast;
pub mod display_affinity;
pub mod engine;
mod floating;
pub mod floating_position_store;
pub mod hidden_window_placement;
pub mod launch_memory;
pub mod virtual_workspace;
pub mod window_store;
mod workspaces;

pub use app_rules::{AppRuleEffects, AppRuleResult};
pub use display_affinity::DisplayAffinity;
pub use engine::{
    EventResponse, LayoutCommand, LayoutEngine, LayoutEvent, LayoutEventOutcome, RestoreReport,
    RestoreRequest, RestoreScope, RestoreSource, RestoreWarning,
};
pub use floating::FloatingManager;
pub use floating_position_store::FloatingPositionStore;
pub use hidden_window_placement::{HiddenWindowPlacement, HideCorner};
pub use rini_protocol::{Direction, ResizeOrientation};
pub use rini_tiling::{LayoutId, LayoutSystem, LayoutSystemKind, ScrollingLayoutSystem};
pub use virtual_workspace::{VirtualWorkspace, VirtualWorkspaceId, WorkspaceStats, WorkspaceStore};
pub use window_store::{
    PendingWindowOperation, WindowPlacement, WindowRecord, WindowStore, WindowVisibility,
    WindowWorkspaceInfo,
};
pub use workspaces::WorkspaceLayouts;
