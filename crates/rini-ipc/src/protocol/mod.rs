//! Shared, platform-neutral protocol types for Rini.
//!
//! The server owns the runtime model and the client owns the Mach transport,
//! but both use these types at the wire boundary. JSON encoding remains an
//! implementation detail of the transport crates.

mod commands;
mod layout;
mod queries;
mod selectors;
mod transport;

pub use commands::{
    Command, ConfigCommand, LayoutCommand, ReactorCommand, RiniCommand, SwitchScope,
};
pub use layout::{Direction, LayoutKind, ResizeOrientation};
pub use queries::{
    ApplicationData, ContainerNodeType, ContainerTreeNode, DiagnosticCensusWindow,
    DiagnosticDisplayCensus, DiagnosticSpace, DiagnosticWindow, DiagnosticsData, DisplayData,
    LayoutStateData, Point, Rect, Size, WindowData, WindowId, WorkspaceData,
};
pub use selectors::{DisplaySelector, RelativeDisplay, RelativeWorkspace, WorkspaceSelector};
pub use transport::{RiniRequest, RiniResponse};
