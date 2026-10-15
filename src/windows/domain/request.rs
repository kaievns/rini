//! What rini asks an app to do, and the handle it asks through.
//!
//! The port into `crate::windows::platform::app_actor`: the per-app thread receives these and turns
//! them into Accessibility and window-server calls. Declared here so the rules that decide what to
//! ask for do not have to reach into the adapter.

use std::fmt::Debug;

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{WindowId, WindowServerId, pid_t};
use rini_runloop::channel as channels;
use tokio_util::sync::CancellationToken;

use crate::windows::domain::transaction::TransactionId;

#[derive(Clone)]
pub struct AppThreadHandle {
    requests_tx: channels::Sender<Request>,
}

impl AppThreadHandle {
    /// A handle over any request channel: what tests stand in for a live app thread.
    pub fn from_sender(requests_tx: channels::Sender<Request>) -> Self {
        AppThreadHandle { requests_tx }
    }

    pub fn send(&self, req: Request) -> anyhow::Result<()> {
        self.requests_tx.send(req);
        Ok(())
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

    SetWindowFrame {
        window: WindowId,
        frame: CGRect,
        txid: TransactionId,
        enhanced_ui: SuppressEnhancedUi,
    },
    SetBatchWindowFrame(Vec<(WindowId, CGRect)>, TransactionId, SuppressEnhancedUi),
    /// Position-only batch reserved for virtual workspace switches.
    SetWorkspaceSwitchPositions(Vec<(WindowId, CGPoint)>, TransactionId, SuppressEnhancedUi),
    /// Raise the windows within a single space, in the given order. All windows must be
    /// in the same space, or they will not be raised correctly.
    ///
    /// Events attributed to this request will use the provided [`Quiet`]
    /// parameter for the last window only. Events for other windows will be
    /// marked `Quiet::Yes` automatically.
    Raise {
        windows: Vec<WindowId>,
        token: CancellationToken,
        sequence_id: RaiseSequence,
        quiet: Quiet,
    },
}

impl Request {
    #[inline]
    pub(crate) fn disables_enhanced_ui(&self) -> bool {
        match self {
            Self::SetWindowFrame { enhanced_ui, .. }
            | Self::SetBatchWindowFrame(_, _, enhanced_ui)
            | Self::SetWorkspaceSwitchPositions(_, _, enhanced_ui) => {
                *enhanced_ui == SuppressEnhancedUi::Yes
            }
            _ => false,
        }
    }
}

/// Whether the app's Enhanced User Interface is turned off for the burst of requests carrying a
/// frame write: one `Yes` in the burst turns it off for all of them.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SuppressEnhancedUi {
    Yes,
    No,
}

/// Which of the raise manager's sequences a raise belongs to, so its completions and timeout are
/// matched to it and not to a sequence that replaced it.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct RaiseSequence(pub u64);

impl RaiseSequence {
    /// The sequence after this one.
    pub fn next(self) -> Self { Self(self.0 + 1) }
}

impl std::fmt::Display for RaiseSequence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

#[derive(Debug, Copy, Clone, Default, PartialEq)]
pub enum Quiet {
    Yes,
    #[default]
    No,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_write(enhanced_ui: SuppressEnhancedUi) -> Request {
        Request::SetWindowFrame {
            window: WindowId::new(1, 1),
            frame: CGRect::ZERO,
            txid: TransactionId::default(),
            enhanced_ui,
        }
    }

    /// Only a frame or position write that asks for it turns Enhanced User Interface off.
    #[test]
    fn a_write_turns_enhanced_ui_off_only_when_it_asks_to() {
        let txid = TransactionId::default();
        let cases = [
            (frame_write(SuppressEnhancedUi::Yes), true),
            (frame_write(SuppressEnhancedUi::No), false),
            (
                Request::SetBatchWindowFrame(Vec::new(), txid, SuppressEnhancedUi::Yes),
                true,
            ),
            (
                Request::SetBatchWindowFrame(Vec::new(), txid, SuppressEnhancedUi::No),
                false,
            ),
            (
                Request::SetWorkspaceSwitchPositions(Vec::new(), txid, SuppressEnhancedUi::Yes),
                true,
            ),
            (
                Request::SetWorkspaceSwitchPositions(Vec::new(), txid, SuppressEnhancedUi::No),
                false,
            ),
            (
                Request::Raise {
                    windows: Vec::new(),
                    token: CancellationToken::new(),
                    sequence_id: RaiseSequence(1),
                    quiet: Quiet::No,
                },
                false,
            ),
            (Request::GetVisibleWindows, false),
        ];
        for (request, disables) in cases {
            assert_eq!(request.disables_enhanced_ui(), disables, "{request:?}");
        }
    }

    #[test]
    fn a_raise_sequence_is_followed_by_the_next_number() {
        assert_eq!(RaiseSequence(1).next(), RaiseSequence(2));
        assert_eq!(RaiseSequence(41).to_string(), "41");
    }
}
