//! What rini asks an app to do, and the handle it asks through.
//!
//! The port into `crate::windows::platform::app_actor`: the per-app thread receives these and turns
//! them into Accessibility and window-server calls. Declared here so the rules that decide what to
//! ask for do not have to reach into the adapter.

use std::fmt::Debug;

use objc2_core_foundation::{CGPoint, CGRect};
use rini_runloop::channel as channels;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use rini_core::ids::{WindowId, WindowServerId, pid_t};

use crate::windows::domain::transaction::TransactionId;

#[derive(Clone)]
pub struct AppThreadHandle {
    requests_tx: channels::Sender<Request>,
}

impl AppThreadHandle {
    /// A handle over any request channel: what tests and event replay stand in for a live app thread.
    pub fn from_sender(requests_tx: channels::Sender<Request>) -> Self {
        AppThreadHandle { requests_tx }
    }

    pub fn send(&self, req: Request) -> anyhow::Result<()> {
        Ok(self.requests_tx.send(req))
    }
}

impl Debug for AppThreadHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadHandle").finish()
    }
}

#[derive(Debug)]
pub enum Request {
    Terminate,
    GetVisibleWindows,
    /// Reconcile the authoritative Carbon front-process change with AX state.
    ///
    /// Carbon supplies the activation edge, while the app thread resolves the
    /// focused/main window and the quiet marker before notifying the reactor.
    ApplicationGloballyActivated(pid_t),
    WindowMaybeDestroyed(WindowId),
    CloseWindow(Option<WindowServerId>),

    SetWindowFrame(WindowId, CGRect, TransactionId, bool),
    SetBatchWindowFrame(Vec<(WindowId, CGRect)>, TransactionId, bool),
    /// Position-only batch reserved for virtual workspace switches.
    SetWorkspaceSwitchPositions(Vec<(WindowId, CGPoint)>, TransactionId, bool),
    /// Raise the windows within a single space, in the given order. All windows must be
    /// in the same space, or they will not be raised correctly.
    ///
    /// Events attributed to this request will use the provided [`Quiet`]
    /// parameter for the last window only. Events for other windows will be
    /// marked `Quiet::Yes` automatically.
    Raise(Vec<WindowId>, CancellationToken, u64, Quiet),
}

impl Request {
    #[inline]
    pub(crate) fn disables_enhanced_ui(&self) -> bool {
        match self {
            Self::SetWindowFrame(_, _, _, enabled)
            | Self::SetBatchWindowFrame(_, _, enabled)
            | Self::SetWorkspaceSwitchPositions(_, _, enabled) => *enabled,
            _ => false,
        }
    }
}

#[derive(Debug, Copy, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum Quiet {
    Yes,
    #[default]
    No,
}
