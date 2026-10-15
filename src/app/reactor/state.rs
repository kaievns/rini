use objc2_core_foundation::CGRect;
use rini_core::ids::{SpaceId, WindowId, pid_t};
use rini_geometry::SameAs;
pub use rini_ipc::protocol::{DisplaySelector, ReactorCommand};

use crate::layout::domain::edge_drag::End;
use crate::windows::domain::info::AppInfo;
use crate::windows::domain::request::AppThreadHandle;
use crate::workspaces::WindowStore;
use crate::workspaces::domain::display_memory::DisplayMemory;

/// All mutable domain state is owned by the reactor thread.
///
/// Window identity, native-space observations, and workspace assignments have one explicit owner
/// here. Cross-store operations receive this store by reference instead of retaining an alias to it.
#[derive(Debug, Default)]
pub struct RiniState {
    pub windows: WindowStore,
    /// What rini remembers about the machine: which display owns which space, which display each
    /// window belongs to, and where each application's windows belong. Separate from the layout
    /// because it outlives one: a layout can be discarded without forgetting the hardware.
    pub display_memory: DisplayMemory,
}

#[derive(Debug, Clone)]
pub struct DragSession {
    pub(crate) window: WindowId,
    /// The window's frame before the first report of the drag: what tells which edge it moves.
    pub(crate) start_frame: CGRect,
    /// The top or bottom edge the drag's reports moved last.
    pub(crate) dragged_end: Option<End>,
    pub(crate) last_frame: CGRect,
    pub(crate) origin_space: Option<SpaceId>,
    pub(crate) settled_space: Option<SpaceId>,
    pub(crate) layout_dirty: bool,
}

impl DragSession {
    /// Whether the drag changed its window's size rather than only moving it.
    pub(crate) fn resized(&self) -> bool { !self.start_frame.size.same_as(self.last_frame.size) }
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
    pub(crate) info: AppInfo,
    pub(crate) handle: AppThreadHandle,
}
