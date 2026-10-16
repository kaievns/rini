//! What the reactor holds back while the displays or the session are unstable: events dropped
//! during display churn and while refreshes are blocked, and the one visible-window refresh owed
//! until every quarantine lifts.
use super::managers::{QuarantineSignal, RefreshQuarantineState, VisibleRefresh};
use super::{Event, Reactor};

impl Reactor {
    pub(super) fn should_quarantine_during_display_churn(&self, event: &Event) -> bool {
        if !self.display_churning() {
            return false;
        }

        match event {
            Event::WindowCreated { .. }
            | Event::WindowDestroyed(..)
            | Event::WindowServerDestroyed(..)
            | Event::WindowServerAppeared(..)
            | Event::WindowFrameChanged { .. }
            | Event::WindowMinimized(..)
            | Event::WindowDeminiaturized(..)
            | Event::WindowTitleChanged(..)
            | Event::WindowsDiscovered { .. }
            | Event::SpaceCreated(..)
            | Event::SpaceDestroyed(..) => true,
            Event::ApplyOverlayFrames(_)
            | Event::SpaceStateChanged(_)
            | Event::ActiveDisplayChanged { .. }
            | Event::ApplicationLaunched { .. }
            | Event::ApplicationTerminated(_)
            | Event::ApplicationThreadTerminated(_)
            | Event::ApplicationActivated(..)
            | Event::ApplicationDeactivated(_)
            | Event::ApplicationGloballyActivated(_)
            | Event::ApplicationGloballyDeactivated(_)
            | Event::ApplicationMainWindowChanged(..)
            | Event::WindowServerFocusChanged(..)
            | Event::MenuOpened(_)
            | Event::MenuClosed(_)
            | Event::MouseUp
            | Event::LeftMouseDown(_)
            | Event::Switch(_)
            | Event::SwitchPicked(_)
            | Event::BarAction(_)
            | Event::MouseMoved(_)
            | Event::SystemWoke
            | Event::ClockChanged
            | Event::SystemWillSleep
            | Event::SessionDidResignActive
            | Event::SessionDidBecomeActive
            | Event::DisplayChurnBegin
            | Event::MissionControlNativeEntered
            | Event::MissionControlNativeExited
            | Event::RaiseCompleted { .. }
            | Event::RaiseTimeout { .. }
            | Event::RaisesIdle { .. }
            | Event::AttentionChanged(_)
            | Event::Query(_)
            | Event::Command(_)
            | Event::RegisterControllerSender(_)
            | Event::ConfigUpdated(_)
            | Event::ConfigFileRead { .. } => false,
        }
    }

    pub(super) fn should_quarantine_space_lifecycle_event(&self, event: &Event) -> bool {
        self.refreshes_blocked()
            && matches!(event, Event::SpaceCreated(..) | Event::SpaceDestroyed(..))
    }

    pub(super) fn refresh_quarantine_state(&self) -> RefreshQuarantineState {
        self.refresh_quarantine_manager.state()
    }

    /// Whether the spaces actor is mid display reconfiguration now, ahead of the events still
    /// queued: a window event already waiting when the churn began is dropped too.
    pub(super) fn display_churning(&self) -> bool { self.reads.display_churn.churning() }

    pub(super) fn refreshes_blocked(&self) -> bool {
        self.refresh_quarantine_manager.blocks_refreshes()
    }

    /// Moves the quarantine, and makes the visible-window refresh it answers is due.
    pub(super) fn quarantine_on(&mut self, signal: QuarantineSignal) {
        if let Some(refresh) = self.refresh_quarantine_manager.on(signal) {
            self.request_visible_windows_for_apps(refresh.tracks_mission_control);
        }
    }

    pub(super) fn defer_visible_refresh(&mut self, tracks_mission_control: bool) {
        self.quarantine_on(QuarantineSignal::RefreshDeferred(VisibleRefresh {
            tracks_mission_control,
        }));
    }
}
