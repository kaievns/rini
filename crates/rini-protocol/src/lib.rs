//! Shared, platform-neutral protocol types for Rini.
//!
//! The server owns the runtime model and the client owns the Mach transport,
//! but both use these types at the wire boundary. JSON encoding remains an
//! implementation detail of the transport crates.

mod commands;
mod events;
mod layout;
mod queries;
mod selectors;
mod transport;

pub use commands::{
    Command,
    ConfigCommand, LayoutCommand, MetricsCommand, ReactorCommand, RiniCommand,
};
pub use events::{EventKind, RiniEvent, WorkspaceId};
pub use layout::{Direction, LayoutKind, ResizeOrientation};
pub use queries::{
    ApplicationData, ContainerNodeType, ContainerTreeNode, DiagnosticCensusWindow,
    DiagnosticDisplayCensus, DiagnosticSpace, DiagnosticWindow, DiagnosticsData, DisplayData,
    LayoutStateData, Point, Rect, Size, WindowData, WindowId, WorkspaceData, WorkspaceLayoutData,
};
pub use selectors::{DisplaySelector, RestoreScope, RestoreSource, WorkspaceSelector};
pub use transport::{JsonRiniResponse, RiniRequest, RiniResponse};
