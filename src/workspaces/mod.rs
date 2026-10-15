//! Virtual workspaces and what they own. Windows are assigned to a
//! workspace and arranged on its strips (`crate::layout`) by `LayoutEngine`; the arrangement is
//! persisted to `layout.ron`, remembered per app across launches, and homed per display.
//! `WindowStore` is the catalogue from `crate::windows` plus each window's assignment.

pub mod domain;
pub mod engine;
pub mod settings;

pub use crate::layout::{LayoutId, ScrollingLayoutSystem};
pub use domain::app_rules::{AppRuleEffects, AppRuleResult};
pub use domain::floating::FloatingManager;
pub use domain::floating_position_store::FloatingPositionStore;
pub use domain::hidden_window_placement::{HiddenWindowPlacement, HideCorner};
pub use domain::virtual_workspace::{VirtualWorkspace, VirtualWorkspaceId, WorkspaceStore};
pub use domain::window_store::{WindowStore, WindowWorkspaceInfo};
pub use domain::workspaces::WorkspaceLayouts;
pub use engine::{EventResponse, LayoutCommand, LayoutEngine, LayoutEvent};
pub use rini_ipc::protocol::Direction;
