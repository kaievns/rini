//! The overview's session on the reactor's side: opened and closed by the tap's hold
//! (`input::domain::switch_session`). Nothing is drawn yet (`specs/overview.md`).

use tracing::info;

use super::Reactor;
use super::events::EventOutcome;
use crate::input::domain::switch_session::OverviewSignal;

/// An overview in progress.
pub(super) struct LiveOverview;

impl Reactor {
    /// Handle the overview's signal from the input thread. Nothing changes on either: every press
    /// inside it runs as it does outside, and its close undoes none of them.
    pub(super) fn handle_overview_signal(&mut self, signal: OverviewSignal) -> EventOutcome {
        info!(?signal, "overview");
        self.live_overview = match signal {
            OverviewSignal::Open => Some(LiveOverview),
            OverviewSignal::Close => None,
        };
        EventOutcome::no_change()
    }
}
