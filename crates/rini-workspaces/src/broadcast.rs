/// The actor broadcast channel uses the protocol event directly. This keeps
/// the server and client on one event definition instead of maintaining a
/// runtime copy that must be translated before IPC.
pub use rini_ipc::protocol::RiniEvent as BroadcastEvent;
use slotmap::Key;

use rini_windows::ids::WindowId;
use crate::virtual_workspace::VirtualWorkspaceId;

pub type BroadcastSender = rini_runloop::channel::Sender<BroadcastEvent>;
pub type BroadcastReceiver = rini_runloop::channel::Receiver<BroadcastEvent>;

pub fn protocol_workspace_id(id: VirtualWorkspaceId) -> rini_ipc::protocol::WorkspaceId {
    let value = id.data().as_ffi();
    rini_ipc::protocol::WorkspaceId {
        idx: value as u32,
        version: (value >> 32) as u32,
    }
}

pub fn protocol_window_id(id: WindowId) -> rini_ipc::protocol::WindowId {
    id.into()
}
