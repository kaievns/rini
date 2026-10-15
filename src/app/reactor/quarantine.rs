//! What the reactor holds back while the displays or the session are unstable: events dropped
//! during display churn and while refreshes are blocked, and the one visible-window refresh owed
//! until every quarantine lifts.
use super::managers::RefreshQuarantineState;
use super::{Event, Reactor};

impl Reactor {
    pub(super) fn should_quarantine_during_display_churn(&self, event: &Event) -> bool {
        if !(self.reads.display_churn)() {
            return false;
        }

        match event {
            Event::WindowCreated(..)
            | Event::WindowDestroyed(..)
            | Event::WindowServerDestroyed(..)
            | Event::WindowServerAppeared(..)
            | Event::WindowFrameChanged(..)
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
            | Event::RegisterWmSender(_)
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

    pub(super) fn refreshes_blocked(&self) -> bool {
        self.refresh_quarantine_manager.blocks_refreshes()
    }

    pub(super) fn defer_visible_refresh(&mut self, track_mission_control_refresh: bool) {
        self.refresh_quarantine_manager.pending_visible_refresh = true;
        self.refresh_quarantine_manager.deferred_refresh_tracks_mission_control |=
            track_mission_control_refresh;
    }

    pub(super) fn flush_deferred_visible_refresh(&mut self) {
        if self.refreshes_blocked() {
            return;
        }

        if self.refresh_quarantine_manager.pending_visible_refresh {
            let track_mission_control_refresh =
                self.refresh_quarantine_manager.deferred_refresh_tracks_mission_control;
            self.refresh_quarantine_manager.pending_visible_refresh = false;
            self.refresh_quarantine_manager.deferred_refresh_tracks_mission_control = false;
            self.request_visible_windows_for_apps(track_mission_control_refresh);
        }
    }

    // All lifecycle churn is upstreamed through the spaces actor. The reactor
    // only remembers that one visibility refresh is owed, then flushes it once
    // every upstream quarantine is lifted.
    pub(super) fn release_post_instability_quarantine_after_authoritative_snapshot(&mut self) {
        let released_wake = self.refresh_quarantine_manager.awaiting_post_wake_snapshot;
        let released_session = self.refresh_quarantine_manager.awaiting_post_session_snapshot;

        if !released_wake && !released_session {
            return;
        }

        self.refresh_quarantine_manager.awaiting_post_wake_snapshot = false;
        self.refresh_quarantine_manager.awaiting_post_session_snapshot = false;
        if released_wake {
            self.refresh_quarantine_manager.sleeping = false;
        }
        if released_session {
            self.refresh_quarantine_manager.session_inactive = false;
        }
        self.flush_deferred_visible_refresh();
    }
}
