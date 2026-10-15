//! The authority on native displays and spaces: holds back every snapshot through lifecycle churn
//! and forwards one coherent `ForwardedSpaceState` at a time. Why it holds back, and what it nulls
//! out, is in `src/displays/docs/topology.md`; the rules it forwards by are
//! `displays::domain::snapshot` and `displays::domain::settling`, and the state it holds between
//! notifications is `displays::domain::authority`'s.
use std::sync::Arc;

use objc2_foundation::MainThreadMarker;
use rini_core::ids::SpaceId;
use rini_runloop::channel;
use rini_skylight_sys::{DisplayReconfigFlags, WindowServerId};
use rustc_hash::FxHashMap as HashMap;

use crate::displays::domain::authority::{
    Churn, ChurnEdge, ChurnStep, Hold, Lifecycle, LifecycleEdge, Refresh, RefreshEdge,
};
use crate::displays::domain::screen::{CoordinateConverter, ScreenInfo};
use crate::displays::domain::settling::{Settle, begins_display_churn, refresh_retry, settle};
use crate::displays::domain::snapshot::{
    Forwarded, Heard, SpaceKinds, Taken, active_spaces, forward, user_screens,
    visible_window_spaces, window_delta,
};
use crate::displays::domain::topology::{
    ForwardedSpaceState, QuarantineStats, SpaceEventKind, TopologyWindowDelta,
    snapshot_is_committable,
};
use crate::displays::event::{Event as OutEvent, EventSink};
use crate::displays::platform::display_churn;
use crate::displays::screen::ScreenCache;
use crate::windows::platform::window_server::{self, WindowServer};

mod live;
mod schedule;
pub use live::LiveDisplays;
use schedule::{MainQueue, Schedule};

const REFRESH_DEFAULT_DELAY_NS: i64 = 100_000_000;
const REFRESH_SPACE_SWITCH_DELAY_NS: i64 = 50_000_000;
const REFRESH_RETRY_DELAY_NS: i64 = 100_000_000;

const DISPLAY_CHURN_QUIET_NS: i64 = 100_000_000;
const DISPLAY_STABILIZE_RETRY_NS: i64 = 100_000_000;

#[derive(Debug, Clone)]
pub enum Notification {
    SystemWillSleep,
    SystemDidWake,
    SessionDidResignActive,
    SessionDidBecomeActive,
    ActiveDisplayChanged,
    ActiveSpaceChanged,
    ScreenRefreshRequested,
    DisplayReconfigured {
        flags: DisplayReconfigFlags,
    },
    SpaceInventoryChanged,
    SpaceCreated(SpaceId),
    SpaceDestroyed(SpaceId),
    WindowServerAppeared(WindowServerId, SpaceId),
    WindowServerDestroyed(WindowServerId, SpaceId),
    /// Keep this many points clear at the top of every display, over what the menu bar keeps.
    TopBandChanged(f64),
    ProcessScreenRefresh {
        attempt: u8,
    },
    CheckDisplayStabilization {
        expected_epoch: u64,
        attempt: u8,
    },
}

pub type Sender = channel::Sender<Notification>;
pub type Receiver = channel::Receiver<Notification>;

pub struct AuthorityState {
    pub last_sent_spaces: Option<Vec<Option<SpaceId>>>,
    pub quarantine_stats: QuarantineStats,
    lifecycle: Lifecycle,
    churn: Churn,
    refresh: Refresh,
    forwarded: Forwarded,
    screen_cache: Option<ScreenCache>,
    last_converter: CoordinateConverter,
    pending_topology_window_delta: Option<TopologyWindowDelta>,
    space_kinds: SpaceKinds,
    live: LiveDisplays,
}

impl AuthorityState {
    fn new(space_kinds: SpaceKinds, live: LiveDisplays, screen_cache: Option<ScreenCache>) -> Self {
        Self {
            last_sent_spaces: None,
            quarantine_stats: QuarantineStats::default(),
            lifecycle: Lifecycle::default(),
            churn: Churn::default(),
            refresh: Refresh::default(),
            forwarded: Forwarded::default(),
            screen_cache,
            last_converter: CoordinateConverter::default(),
            pending_topology_window_delta: None,
            space_kinds,
            live,
        }
    }

    /// Whether the actor drops what it hears rather than forward it, and samples the window server
    /// again once the quarantine ends.
    ///
    /// Asleep, at the login window, or mid display reconfiguration. In all three the native picture
    /// is part-formed: screens appear without spaces, spaces are reported on the wrong display, and a
    /// window's space is whatever it was before the transition started. Forwarding any of that makes
    /// the reactor act on it.
    pub(crate) fn quarantined(&self) -> bool { self.lifecycle.holds() || self.churn.is_active() }
}

pub struct SpacesActor {
    receiver: Receiver,
    events: Box<dyn EventSink>,
    schedule: Box<dyn Schedule>,
    window_server: Arc<dyn WindowServer>,
    state: AuthorityState,
}

impl SpacesActor {
    /// The actor production runs, over the window server on this machine. `top_band` is what the
    /// screens start with; `Notification::TopBandChanged` changes it.
    pub fn new(
        receiver: Receiver,
        sender: Sender,
        events: Box<dyn EventSink>,
        top_band: f64,
    ) -> Self {
        let mut screen_cache = ScreenCache::new(MainThreadMarker::new().unwrap());
        let _ = screen_cache.set_top_band(top_band);
        let (kinds, live) = (
            SpaceKinds::from_window_server(),
            LiveDisplays::from_window_server(),
        );
        Self {
            receiver,
            events,
            schedule: Box::new(MainQueue(sender)),
            window_server: window_server::live(),
            state: AuthorityState::new(kinds, live, Some(screen_cache)),
        }
    }

    pub async fn run(mut self) {
        while let Some((span, event)) = self.receiver.recv().await {
            let _guard = span.enter();
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: Notification) {
        match event {
            Notification::SystemWillSleep => {
                self.hold(Hold::Sleep, OutEvent::SystemWillSleep);
                self.with_screen_cache(|cache| cache.mark_sleeping(true));
            }
            Notification::SystemDidWake => {
                self.events.send(OutEvent::SystemWoke);
                self.with_screen_cache(|cache| cache.mark_sleeping(false));
                self.resample(Hold::Sleep);
            }
            Notification::SessionDidResignActive => {
                self.hold(Hold::Session, OutEvent::SessionDidResignActive);
            }
            Notification::SessionDidBecomeActive => {
                self.events.send(OutEvent::SessionDidBecomeActive);
                self.resample(Hold::Session);
            }
            Notification::ActiveDisplayChanged => self.handle_active_display_changed(),
            Notification::ActiveSpaceChanged => self.handle_active_space_changed(),
            Notification::ScreenRefreshRequested => {
                // Preference and system screen changes can report stale geometry at first.
                self.with_screen_cache(ScreenCache::mark_dirty);
                self.schedule_screen_refresh();
            }
            Notification::DisplayReconfigured { flags } => {
                self.handle_display_reconfig_event(flags)
            }
            Notification::SpaceInventoryChanged => self.handle_space_inventory_changed(),
            Notification::SpaceCreated(space) => {
                self.forward_space_event(space, OutEvent::SpaceCreated(space))
            }
            Notification::SpaceDestroyed(space) => {
                self.forward_space_event(space, OutEvent::SpaceDestroyed(space))
            }
            Notification::WindowServerAppeared(wsid, sid) => self.handle_window_appeared(wsid, sid),
            Notification::WindowServerDestroyed(wsid, sid) => {
                self.handle_window_destroyed(wsid, sid)
            }
            Notification::TopBandChanged(band) => {
                if self.state.screen_cache.as_mut().is_some_and(|cache| cache.set_top_band(band)) {
                    self.schedule_screen_refresh();
                }
            }
            Notification::ProcessScreenRefresh { attempt } => self.process_screen_refresh(attempt),
            Notification::CheckDisplayStabilization { expected_epoch, attempt } => {
                self.attempt_finish_display_churn(expected_epoch, attempt);
            }
        }
    }

    fn with_screen_cache(&mut self, change: impl FnOnce(&mut ScreenCache)) {
        if let Some(cache) = self.state.screen_cache.as_mut() {
            change(cache);
        }
    }

    /// Enters a sleep or session quarantine: no snapshot releases the reactor's until it ends.
    fn hold(&mut self, hold: Hold, event: OutEvent) {
        self.state.lifecycle = self.state.lifecycle.next(LifecycleEdge::On(hold));
        self.events.send(event);
    }

    /// Leaves a sleep or session quarantine. Wake is unstable, and the login window can replace
    /// every display's current space, so the screens are sampled afresh and the next snapshot
    /// forwarded releases the reactor's quarantine.
    fn resample(&mut self, hold: Hold) {
        self.state.lifecycle = self.state.lifecycle.next(LifecycleEdge::Off(hold));
        self.with_screen_cache(ScreenCache::mark_dirty);
        if self.state.churn.is_active() {
            self.schedule_display_stabilization(
                self.state.churn.epoch(),
                0,
                DISPLAY_CHURN_QUIET_NS,
            );
        }
        self.schedule_screen_refresh();
    }

    fn forward_space_event(&mut self, space: SpaceId, event: OutEvent) {
        if !self.state.quarantined() && self.state.space_kinds.classify(space).is_some() {
            self.events.send(event);
        }
        self.handle_space_inventory_changed();
    }

    fn handle_window_appeared(&mut self, wsid: WindowServerId, sid: SpaceId) {
        if self.state.quarantined() {
            self.state.quarantine_stats.appeared_dropped += 1;
            return;
        }
        self.state.forwarded.visible_window_spaces.insert(wsid, sid);
        if let Some(kind) = self.state.space_kinds.classify(sid) {
            self.events.send(OutEvent::WindowServerAppeared(wsid, sid, kind));
        }
    }

    fn handle_window_destroyed(&mut self, wsid: WindowServerId, sid: SpaceId) {
        if self.state.quarantined() {
            self.state.quarantine_stats.destroyed_dropped += 1;
            return;
        }
        self.state.forwarded.visible_window_spaces.remove(&wsid);
        let kinds = self.state.space_kinds;
        let current_space = self.window_server.window_space(wsid, &|space| kinds.is_user(space));
        let Some(kind) = self.state.space_kinds.classify(sid) else {
            return;
        };
        if kind == SpaceEventKind::User && current_space.is_some_and(|space| space != sid) {
            // The window server reports a confirmed move out of the origin space as a destroy. The
            // selected space can stay the same, so the membership is forwarded as a snapshot.
            self.handle_space_inventory_changed();
            return;
        }
        self.events.send(OutEvent::WindowServerDestroyed(wsid, sid, kind));
    }

    fn handle_active_display_changed(&mut self) {
        let active_display_uuid = (self.state.live.active_menu_bar_display)();
        self.handle_active_display_changed_for(active_display_uuid.as_deref());
    }

    fn handle_active_display_changed_for(&mut self, active_display_uuid: Option<&str>) {
        let known = self.state.forwarded.active_display_uuid.as_deref();
        if active_display_uuid.is_some() && active_display_uuid == known {
            return;
        }
        if self.state.quarantined() {
            self.schedule_screen_refresh_after(0, 0);
            return;
        }
        if let Some(active_display_uuid) = active_display_uuid
            && let Some(active_space) = self
                .state
                .forwarded
                .screens
                .iter()
                .find(|screen| screen.display_uuid == active_display_uuid)
                .and_then(|screen| screen.space)
        {
            self.state.forwarded.active_display_uuid = Some(active_display_uuid.to_string());
            self.events.send(OutEvent::ActiveDisplayChanged {
                menu_bar_space: Some(active_space),
                command_space: Some(active_space),
            });
            return;
        }
        if !self.try_forward_authoritative_snapshot(true, true) {
            self.schedule_screen_refresh_after(0, 0);
        }
    }

    fn handle_active_space_changed(&mut self) {
        self.state.refresh = self.state.refresh.next(RefreshEdge::SpaceSwitched);
        if self.state.quarantined() || !self.try_forward_authoritative_snapshot(false, true) {
            self.schedule_screen_refresh_after(REFRESH_SPACE_SWITCH_DELAY_NS, 0);
        }
    }

    fn handle_space_inventory_changed(&mut self) {
        if self.state.quarantined() {
            self.schedule_screen_refresh_after(0, 0);
            return;
        }
        // Creating or destroying a space, or moving a window between spaces, can change what a
        // display owns while every screen shows the space it showed, so the snapshot is forced.
        if !self.try_forward_authoritative_snapshot(true, true) {
            self.schedule_screen_refresh_after(REFRESH_RETRY_DELAY_NS, 0);
        }
    }

    fn collect_state(&mut self) -> Option<(Vec<ScreenInfo>, CoordinateConverter)> {
        self.state.screen_cache.as_mut().and_then(ScreenCache::refresh).or_else(|| {
            // No live source. The last coherent picture beats no screens at all: an empty screen
            // list is what macOS reports mid-reconfiguration and is never authoritative.
            let screens = &self.state.forwarded.screens;
            (!screens.is_empty()).then(|| (screens.clone(), self.state.last_converter))
        })
    }

    fn forward_screen_parameters(
        &mut self,
        screens: Vec<ScreenInfo>,
        converter: CoordinateConverter,
    ) {
        self.state.last_converter = converter;
        let forwarded = self.build_forwarded_state(screens);
        self.state.last_sent_spaces = Some(spaces_of(&forwarded.screens));
        self.state.refresh = self.state.refresh.next(RefreshEdge::Forwarded);
        self.events.send(OutEvent::SpaceStateUpdated(
            Box::new(forwarded),
            self.state.last_converter,
        ));
    }

    fn build_forwarded_state(&mut self, screens: Vec<ScreenInfo>) -> ForwardedSpaceState {
        let previous = &self.state.forwarded.screens;
        let (screens, fullscreen_spaces) = user_screens(previous, screens, self.state.space_kinds);
        let live = self.state.live;
        let heard = Heard {
            active_display: (live.active_menu_bar_display)(),
            active_space: (live.active_space)(),
            spaces_per_display: (live.spaces_per_display)(),
        };
        let windows = self.visible_window_spaces_for_screens(&screens);
        let releases_lifecycle_refresh_quarantine = self.state.lifecycle.releases();
        self.state.lifecycle = self.state.lifecycle.next(LifecycleEdge::Forwarded);
        let taken = Taken {
            screens,
            fullscreen_spaces,
            heard,
            windows,
            releases_lifecycle_refresh_quarantine,
            topology_window_delta: self.state.pending_topology_window_delta.take(),
        };
        forward(&mut self.state.forwarded, taken)
    }

    /// Each active space's own window list, read one space at a time: a list over every space at
    /// once is not space-aware and can lag one display behind another.
    fn visible_window_spaces_for_screens(
        &self,
        screens: &[ScreenInfo],
    ) -> HashMap<WindowServerId, SpaceId> {
        let (ws, kinds) = (&self.window_server, self.state.space_kinds);
        let listed: Vec<_> = active_spaces(screens)
            .into_iter()
            .map(|space| {
                let ids = ws.space_window_list_for_connection(&[space.get()]);
                let homes = ids
                    .into_iter()
                    .map(WindowServerId::new)
                    .map(|wsid| (wsid, ws.window_space(wsid, &|space| kinds.is_user(space))));
                (space, homes.collect())
            })
            .collect();
        visible_window_spaces(
            &listed,
            &self.state.forwarded.visible_window_spaces,
            self.state.lifecycle.releases(),
        )
    }

    fn synthesize_topology_window_delta(
        &mut self,
        epoch: u64,
        flags: DisplayReconfigFlags,
        previous: HashMap<WindowServerId, SpaceId>,
        screens: &[ScreenInfo],
    ) {
        let current = self.visible_window_spaces_for_screens(screens);
        self.state.pending_topology_window_delta =
            Some(window_delta(epoch, flags, &previous, &current));
        self.state.forwarded.visible_window_spaces = current;
    }

    fn process_screen_refresh(&mut self, attempt: u8) {
        if self.state.quarantined() {
            self.end_refresh();
            return;
        }
        let kinds = self.state.space_kinds;
        let committable = self.collect_state().filter(|(screens, _)| {
            snapshot_is_committable(&spaces_of(screens), true, |space| kinds.is_fullscreen(space))
        });
        let Some((screens, converter)) = committable else {
            match refresh_retry(attempt) {
                Some(next) => self.schedule_screen_refresh_after(REFRESH_RETRY_DELAY_NS, next),
                None => self.finish_screen_refresh_attempts(),
            }
            return;
        };
        if self.state.refresh.awaits_space_switch()
            && self.state.last_sent_spaces.as_ref() == Some(&spaces_of(&screens))
            && let Some(next) = refresh_retry(attempt)
        {
            self.schedule_screen_refresh_after(REFRESH_SPACE_SWITCH_DELAY_NS, next);
            return;
        }
        self.forward_screen_parameters(screens, converter);
        self.end_refresh();
    }

    fn end_refresh(&mut self) { self.state.refresh = self.state.refresh.next(RefreshEdge::Ended); }

    fn finish_screen_refresh_attempts(&mut self) {
        self.end_refresh();
        // Only a forwarded snapshot releases the reactor's lifecycle quarantine, so the retries
        // start over rather than leave the reactor held for good.
        if self.state.lifecycle.releases() {
            self.schedule_screen_refresh_after(REFRESH_RETRY_DELAY_NS, 0);
        }
    }

    fn try_forward_authoritative_snapshot(
        &mut self,
        force: bool,
        require_complete_spaces: bool,
    ) -> bool {
        if self.state.refresh.is_pending() || self.state.churn.is_active() {
            return false;
        }
        let Some((screens, converter)) = self.collect_state() else {
            return false;
        };
        let spaces = spaces_of(&screens);
        let kinds = self.state.space_kinds;
        if !snapshot_is_committable(&spaces, require_complete_spaces, |space| {
            kinds.is_fullscreen(space)
        }) || (!force && self.state.last_sent_spaces.as_ref() == Some(&spaces))
        {
            return false;
        }
        self.forward_screen_parameters(screens, converter);
        true
    }

    fn schedule_screen_refresh(&mut self) {
        self.schedule_screen_refresh_after(REFRESH_DEFAULT_DELAY_NS, 0);
    }

    /// A first attempt waits while a churn is under way, or another refresh is already pending.
    fn schedule_screen_refresh_after(&mut self, delay_ns: i64, attempt: u8) {
        if attempt == 0 && (self.state.churn.is_active() || self.state.refresh.is_pending()) {
            return;
        }
        self.state.refresh = self.state.refresh.next(RefreshEdge::Scheduled);
        self.schedule.after(delay_ns, Notification::ProcessScreenRefresh { attempt });
    }

    fn handle_display_reconfig_event(&mut self, flags: DisplayReconfigFlags) {
        if !begins_display_churn(flags) {
            return;
        }
        let expected_epoch = self.begin_display_churn(flags);
        self.with_screen_cache(ScreenCache::mark_dirty);
        self.schedule_display_stabilization(expected_epoch, 0, DISPLAY_CHURN_QUIET_NS);
    }

    fn begin_display_churn(&mut self, flags: DisplayReconfigFlags) -> u64 {
        let windows = &self.state.forwarded.visible_window_spaces;
        let step = self.state.churn.next(ChurnEdge::Reconfigured(flags, windows));
        self.state.last_sent_spaces = None;
        display_churn::begin();
        if step == ChurnStep::Began {
            self.events.send(OutEvent::DisplayChurnBegin);
        }
        self.state.churn.epoch()
    }

    fn schedule_display_stabilization(&self, expected_epoch: u64, attempt: u8, delay_ns: i64) {
        let check = Notification::CheckDisplayStabilization { expected_epoch, attempt };
        self.schedule.after(delay_ns, check);
    }

    fn attempt_finish_display_churn(&mut self, expected_epoch: u64, attempt: u8) {
        if self.state.churn.run_at(expected_epoch).is_none() {
            return;
        }
        let (screens, converter) = self.collect_state().unwrap_or_default();
        let (kinds, quiet) = (self.state.space_kinds, self.state.live.window_server_quiet);
        let Some(run) = self.state.churn.run_at(expected_epoch) else {
            return;
        };
        match settle(&mut run.stability, &screens, kinds, quiet, attempt) {
            Settle::Again(next) => self.schedule_display_stabilization(
                expected_epoch,
                next,
                DISPLAY_STABILIZE_RETRY_NS,
            ),
            Settle::GiveUp => self.finish_display_churn(expected_epoch, true),
            Settle::Forward => {
                let (flags, before) = (run.flags, std::mem::take(&mut run.windows_before));
                self.synthesize_topology_window_delta(expected_epoch, flags, before, &screens);
                // The settled snapshot acknowledges the reactor's churn quarantine.
                self.forward_screen_parameters(screens, converter);
                self.finish_display_churn(expected_epoch, false);
            }
        }
    }

    fn finish_display_churn(&mut self, expected_epoch: u64, schedule_refresh: bool) {
        if self.state.churn.next(ChurnEdge::Ended(expected_epoch)) == ChurnStep::Stale {
            return;
        }
        display_churn::end();
        if schedule_refresh {
            self.schedule_screen_refresh_after(0, 0);
        }
    }
}

fn spaces_of(screens: &[ScreenInfo]) -> Vec<Option<SpaceId>> {
    screens.iter().map(|screen| screen.space).collect()
}

#[cfg(test)]
mod tests;
