use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_runloop::channel;

use super::schedule::Recorded;
use super::*;
use crate::displays::domain::settling::{DISPLAY_STABILIZE_MAX_ATTEMPTS, REFRESH_MAX_RETRIES};
use crate::displays::domain::snapshot::TEST_FULLSCREEN_SPACE;
use crate::displays::domain::topology::{DisplaySet, Topology};
use crate::displays::event::Event as OutEvent;
use crate::windows::platform::window_server::FakeWindowServer;

impl LiveDisplays {
    /// A reader that knows nothing, which is what a test gets unless it says otherwise. Every rule
    /// over these has a path for "the window server did not answer", and that is the path a test
    /// takes by default. A window server that has reported nothing is a quiet one, as it is to the
    /// production reader before the first window event.
    fn silent() -> Self {
        Self {
            active_menu_bar_display: || None,
            active_space: || None,
            spaces_per_display: HashMap::default,
            window_server_quiet: || true,
        }
    }
}

impl SpacesActor {
    /// A test actor: no screen cache, a window server that knows nothing, and a schedule that only
    /// records.
    fn for_test(events: Box<dyn EventSink>, space_kinds: SpaceKinds, live: LiveDisplays) -> Self {
        let (_, receiver) = channel::channel();
        Self {
            receiver,
            events,
            schedule: Box::new(Recorded::default()),
            window_server: Arc::new(FakeWindowServer::default()),
            state: AuthorityState::new(space_kinds, live, None),
        }
    }

    fn new_for_tests(events: Box<dyn EventSink>) -> Self {
        Self::for_test(events, SpaceKinds::for_tests(), LiveDisplays::silent())
    }

    /// The window server this actor reads from now on, for the test to state answers into.
    fn hand_window_server(&mut self) -> Arc<FakeWindowServer> {
        let ws = Arc::new(FakeWindowServer::default());
        self.window_server = ws.clone();
        ws
    }

    /// The schedule this actor asks from now on, for the test to read back.
    fn hand_schedule(&mut self) -> Recorded {
        let scheduled = Recorded::default();
        self.schedule = Box::new(scheduled.clone());
        scheduled
    }
}

/// Splits the actor's one event stream so a test reads each part on its own: topology snapshots one
/// way, everything else the other, each in the order the actor sent them. The application sends
/// the whole stream to the reactor's queue.
struct Split {
    topology: channel::Sender<OutEvent>,
    rest: channel::Sender<OutEvent>,
}
impl EventSink for Split {
    fn send(&self, event: OutEvent) {
        match event {
            OutEvent::SpaceStateUpdated(..) => self.topology.send(event),
            _ => self.rest.send(event),
        }
    }
}

fn make_screen(space: Option<SpaceId>) -> ScreenInfo {
    ScreenInfo {
        is_builtin: false,
        id: rini_core::ids::ScreenId::new(1),
        frame: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 800.0)),
        bounds: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1000.0, 800.0)),
        display_uuid: "display-1".to_string(),
        name: Some("Display".to_string()),
        space,
        scale: 2.0,
    }
}

fn make_screen_with(
    screen_id: u32,
    display_uuid: &str,
    origin_x: f64,
    width: f64,
    space: Option<SpaceId>,
) -> ScreenInfo {
    ScreenInfo {
        is_builtin: false,
        id: rini_core::ids::ScreenId::new(screen_id),
        frame: CGRect::new(CGPoint::new(origin_x, 0.0), CGSize::new(width, 800.0)),
        bounds: CGRect::new(CGPoint::new(origin_x, 0.0), CGSize::new(width, 800.0)),
        display_uuid: display_uuid.to_string(),
        name: Some(display_uuid.to_string()),
        space,
        scale: 2.0,
    }
}

fn fullscreen_space_for(user_space: SpaceId) -> SpaceId {
    SpaceId::new(0x400000000 + user_space.get())
}

fn recv_wm(rx: &mut channel::Receiver<OutEvent>) -> OutEvent {
    rx.try_recv().expect("expected wm event").1
}

fn recv_reactor(rx: &mut channel::Receiver<OutEvent>) -> OutEvent {
    rx.try_recv().expect("expected reactor event").1
}

fn assert_no_wm_event(rx: &mut channel::Receiver<OutEvent>) {
    assert!(rx.try_recv().is_err(), "expected no wm event");
}

fn assert_no_reactor_event(rx: &mut channel::Receiver<OutEvent>) {
    assert!(rx.try_recv().is_err(), "expected no reactor event");
}

/// An actor that calls one specific space id a login space: neither fullscreen nor a user space.
fn build_actor_with_a_non_user_space() -> (
    SpacesActor,
    channel::Receiver<OutEvent>,
    channel::Receiver<OutEvent>,
) {
    let (wm_tx, wm_rx) = channel::channel();
    let (reactor_tx, reactor_rx) = channel::channel();
    let kinds = SpaceKinds {
        is_fullscreen: |space| space.get() >= TEST_FULLSCREEN_SPACE,
        is_user: |space| space.get() != NON_USER_SPACE,
    };
    let actor = SpacesActor::for_test(
        Box::new(Split {
            topology: wm_tx,
            rest: reactor_tx,
        }),
        kinds,
        LiveDisplays::silent(),
    );
    (actor, wm_rx, reactor_rx)
}

/// The id `build_actor_with_a_non_user_space` treats as a login space.
const NON_USER_SPACE: u64 = 7;

fn build_actor() -> (
    SpacesActor,
    channel::Receiver<OutEvent>,
    channel::Receiver<OutEvent>,
) {
    let (wm_tx, wm_rx) = channel::channel();
    let (reactor_tx, reactor_rx) = channel::channel();
    let actor = SpacesActor::new_for_tests(Box::new(Split {
        topology: wm_tx,
        rest: reactor_tx,
    }));
    (actor, wm_rx, reactor_rx)
}

#[test]
fn active_display_changed_skips_refresh_when_display_is_already_active() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(SpaceId::new(1))),
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(SpaceId::new(2))),
        ],
        CoordinateConverter::default(),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.handle_active_display_changed_for(Some("display-left"));
    assert_no_wm_event(&mut wm_rx);
    assert_no_reactor_event(&mut reactor_rx);

    actor.handle_active_display_changed_for(Some("display-right"));
    assert_no_wm_event(&mut wm_rx);
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::ActiveDisplayChanged {
            menu_bar_space: Some(space),
            command_space: Some(command_space),
        } if space == SpaceId::new(2) && command_space == SpaceId::new(2)
    ));
}

#[test]
fn confirmed_window_move_forwards_membership_without_space_switch() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    let origin = SpaceId::new(11);
    let destination = SpaceId::new(12);
    let wsid = WindowServerId::new(77);

    actor.state.forwarded.screens = vec![make_screen(Some(origin))];
    actor.state.last_sent_spaces = Some(vec![Some(origin)]);
    actor.state.forwarded.visible_window_spaces.insert(wsid, origin);
    actor.hand_window_server().set_window_spaces(wsid, vec![destination]);

    actor.handle_event(Notification::WindowServerDestroyed(wsid, origin));

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert!(state.active_window_spaces.is_empty());
            assert_eq!(state.screens[0].space, Some(origin));
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
    assert_no_reactor_event(&mut reactor_rx);
}

/// The actor's own classifier picks a window's home among its spaces, so a window listed on a
/// login space before the user space it was destroyed on has not moved.
#[test]
fn a_window_destroyed_on_its_user_space_while_also_on_a_login_space_is_destroyed_there() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor_with_a_non_user_space();
    let origin = SpaceId::new(11);
    let wsid = WindowServerId::new(77);

    actor.state.forwarded.screens = vec![make_screen(Some(origin))];
    actor.state.last_sent_spaces = Some(vec![Some(origin)]);
    actor.state.forwarded.visible_window_spaces.insert(wsid, origin);
    actor
        .hand_window_server()
        .set_window_spaces(wsid, vec![SpaceId::new(NON_USER_SPACE), origin]);

    actor.handle_event(Notification::WindowServerDestroyed(wsid, origin));

    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::WindowServerDestroyed(id, space, SpaceEventKind::User)
            if id == wsid && space == origin
    ));
    assert_no_wm_event(&mut wm_rx);
}

#[test]
fn active_space_changed_waits_for_confirmed_refresh() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    actor.state.last_sent_spaces = Some(vec![Some(SpaceId::new(11))]);

    actor.handle_event(Notification::ActiveSpaceChanged);

    assert!(actor.state.refresh.awaits_space_switch());
    assert_no_wm_event(&mut wm_rx);
    assert_no_reactor_event(&mut reactor_rx);
}

#[test]
fn active_space_changed_forwards_immediately_when_space_snapshot_changes() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    let old_space = SpaceId::new(11);
    let new_space = SpaceId::new(12);

    actor.state.forwarded.screens = vec![make_screen(Some(new_space))];
    actor.state.last_sent_spaces = Some(vec![Some(old_space)]);

    actor.handle_event(Notification::ActiveSpaceChanged);

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(
                state.screens.iter().map(|screen| screen.space).collect::<Vec<_>>(),
                vec![Some(new_space)]
            );
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
    assert!(!actor.state.refresh.awaits_space_switch());
    assert_no_reactor_event(&mut reactor_rx);
}

#[test]
fn quarantines_window_space_events_during_sleep_before_churn_begins() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    let appeared = WindowServerId::new(7);
    let existing = WindowServerId::new(8);
    let existing_space = SpaceId::new(4);
    actor.state.forwarded.visible_window_spaces.insert(existing, existing_space);

    actor.handle_event(Notification::SystemWillSleep);
    actor.handle_event(Notification::WindowServerAppeared(appeared, SpaceId::new(3)));
    actor.handle_event(Notification::WindowServerDestroyed(existing, existing_space));
    actor.handle_event(Notification::SpaceCreated(SpaceId::new(5)));
    actor.handle_event(Notification::SpaceDestroyed(SpaceId::new(6)));

    assert_eq!(actor.state.quarantine_stats, QuarantineStats {
        appeared_dropped: 1,
        destroyed_dropped: 1
    });
    assert!(!actor.state.forwarded.visible_window_spaces.contains_key(&appeared));
    assert_eq!(
        actor.state.forwarded.visible_window_spaces.get(&existing),
        Some(&existing_space)
    );
    assert_no_wm_event(&mut wm_rx);
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::SystemWillSleep
    ));
    assert_no_reactor_event(&mut reactor_rx);
}

#[test]
fn timed_refresh_does_not_forward_while_session_is_inactive() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    let unlocked = SpaceId::new(41);
    let locked = SpaceId::new(141);

    actor.forward_screen_parameters(
        vec![make_screen(Some(unlocked))],
        CoordinateConverter::default(),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.handle_event(Notification::SessionDidResignActive);
    actor.state.forwarded.screens = vec![make_screen(Some(locked))];
    actor.handle_event(Notification::ProcessScreenRefresh { attempt: 0 });

    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::SessionDidResignActive
    ));
    assert_no_wm_event(&mut wm_rx);

    actor.handle_event(Notification::SessionDidBecomeActive);
    actor.forward_screen_parameters(
        vec![make_screen(Some(unlocked))],
        CoordinateConverter::default(),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(
                state.screens.iter().map(|screen| screen.space).collect::<Vec<_>>(),
                vec![Some(unlocked)]
            );
            assert!(
                state.releases_lifecycle_refresh_quarantine,
                "the first post-unlock forwarded snapshot should release the reactor quarantine"
            );
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::SessionDidBecomeActive
    ));
}

#[test]
fn wake_into_display_churn_forwards_nothing_until_churn_settles_then_releases_the_quarantine() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    let left = SpaceId::new(71);
    let right = SpaceId::new(72);
    actor.forward_screen_parameters(
        vec![make_screen_with(1, "display-left", 0.0, 1000.0, Some(left))],
        CoordinateConverter::default(),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.handle_event(Notification::SystemWillSleep);
    actor.handle_event(Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::ADD,
    });
    actor.handle_event(Notification::SystemDidWake);
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::SystemWillSleep
    ));
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::DisplayChurnBegin
    ));
    assert!(matches!(recv_reactor(&mut reactor_rx), OutEvent::SystemWoke));

    actor.state.forwarded.screens = vec![
        make_screen_with(1, "display-left", 0.0, 1000.0, Some(left)),
        make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right)),
    ];
    actor.handle_event(Notification::ProcessScreenRefresh { attempt: 0 });
    actor.handle_event(Notification::ActiveSpaceChanged);
    assert_no_wm_event(&mut wm_rx);

    let epoch = actor.state.churn.epoch();
    actor.handle_event(Notification::CheckDisplayStabilization {
        expected_epoch: epoch,
        attempt: 0,
    });
    actor.handle_event(Notification::CheckDisplayStabilization {
        expected_epoch: epoch,
        attempt: 1,
    });

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(
                state.screens.iter().map(|screen| screen.space).collect::<Vec<_>>(),
                vec![Some(left), Some(right)]
            );
            assert!(
                state.releases_lifecycle_refresh_quarantine,
                "the first snapshot forwarded after wake should release the reactor quarantine"
            );
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
    assert!(!actor.state.churn.is_active());

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(left)),
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right)),
        ],
        CoordinateConverter::default(),
    );
    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert!(!state.releases_lifecycle_refresh_quarantine)
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

#[test]
fn quarantines_space_lifecycle_events_during_churn_until_snapshot() {
    let (mut actor, _wm_rx, mut reactor_rx) = build_actor();
    let space = SpaceId::new(61);

    actor.handle_event(Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::MOVED,
    });
    actor.handle_event(Notification::SpaceCreated(space));
    actor.handle_event(Notification::SpaceDestroyed(space));

    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::DisplayChurnBegin
    ));
    assert_no_reactor_event(&mut reactor_rx);
}

#[test]
fn display_setting_reconfig_starts_churn() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();

    actor.handle_event(Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::BEGIN_CONFIGURATION
            | rini_skylight_sys::DisplayReconfigFlags::SET_MAIN
            | rini_skylight_sys::DisplayReconfigFlags::DESKTOP_SHAPE_CHANGED,
    });

    assert!(actor.state.churn.is_active());
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::DisplayChurnBegin
    ));
    assert_no_wm_event(&mut wm_rx);
}

#[test]
fn benign_display_reconfig_does_not_start_churn() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();

    actor.handle_event(Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::BEGIN_CONFIGURATION,
    });

    assert!(!actor.state.churn.is_active());
    assert_no_wm_event(&mut wm_rx);
    assert_no_reactor_event(&mut reactor_rx);
}

#[test]
fn physical_display_reconfig_starts_churn() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();

    actor.handle_event(Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::MOVED,
    });

    assert!(actor.state.churn.is_active());
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::DisplayChurnBegin
    ));
    assert_no_wm_event(&mut wm_rx);
}

#[test]
fn topology_delta_uses_last_forwarded_screens_as_diff_base() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();

    actor.forward_screen_parameters(
        vec![make_screen_with(
            1,
            "display-1",
            0.0,
            1000.0,
            Some(SpaceId::new(1)),
        )],
        CoordinateConverter::from_height(800.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-1", 0.0, 1000.0, Some(SpaceId::new(1))),
            make_screen_with(2, "display-2", 1000.0, 1200.0, Some(SpaceId::new(2))),
        ],
        CoordinateConverter::from_height(800.0),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(state.topology, Topology::Remap(DisplaySet::Changed));
            assert_eq!(state.resized_spaces.len(), 1);
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
    assert_no_reactor_event(&mut reactor_rx);
}

#[test]
fn space_inventory_changes_force_a_fresh_forwarded_snapshot() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    let space = SpaceId::new(72);

    actor.forward_screen_parameters(
        vec![make_screen(Some(space))],
        CoordinateConverter::from_height(900.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.handle_event(Notification::SpaceInventoryChanged);

    assert!(matches!(
        recv_wm(&mut wm_rx),
        OutEvent::SpaceStateUpdated(state, _)
            if state.screens.iter().map(|screen| screen.space).collect::<Vec<_>>() == vec![Some(space)]
    ));
    assert_no_reactor_event(&mut reactor_rx);
}

#[test]
fn fullscreen_transition_is_normalized_before_forwarding() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let left_space_2 = SpaceId::new(12);
    let left_space_1 = SpaceId::new(11);
    let right_space_1 = SpaceId::new(21);
    let right_fullscreen = fullscreen_space_for(right_space_1);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(left_space_2)),
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right_space_1)),
        ],
        CoordinateConverter::from_height(1000.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(left_space_1)),
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right_fullscreen)),
        ],
        CoordinateConverter::from_height(1000.0),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            let spaces: Vec<Option<SpaceId>> =
                state.screens.iter().map(|screen| screen.space).collect();
            assert_eq!(spaces, vec![Some(left_space_1), None]);
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

#[test]
fn topology_change_emits_space_remap_from_display_history() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let original_space = SpaceId::new(31);
    let remapped_space = SpaceId::new(41);

    actor.forward_screen_parameters(
        vec![make_screen_with(
            1,
            "display-1",
            0.0,
            1000.0,
            Some(original_space),
        )],
        CoordinateConverter::from_height(800.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-1", 0.0, 1000.0, Some(remapped_space)),
            make_screen_with(2, "display-2", 1000.0, 1000.0, Some(SpaceId::new(51))),
        ],
        CoordinateConverter::from_height(800.0),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(state.space_remaps, vec![(original_space, remapped_space)]);
            assert!(state.topology.allows_space_remap());
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

#[test]
fn wake_transient_cannot_steal_another_displays_space_history() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let builtin_space = SpaceId::new(3);
    let external_space_before_sleep = SpaceId::new(506);
    let external_space_after_wake = SpaceId::new(517);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "builtin", 0.0, 1000.0, Some(builtin_space)),
            make_screen_with(2, "external", 1000.0, 1000.0, Some(external_space_before_sleep)),
        ],
        CoordinateConverter::from_height(800.0),
    );
    let _ = recv_wm(&mut wm_rx);

    // WindowServer can publish this transient while the built-in display is
    // absent. The external display must not adopt the built-in display's space.
    actor.forward_screen_parameters(
        vec![make_screen_with(
            2,
            "external",
            0.0,
            1000.0,
            Some(builtin_space),
        )],
        CoordinateConverter::from_height(800.0),
    );
    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert!(state.space_remaps.is_empty());
            assert_eq!(
                state.last_user_space_by_display.get("external"),
                Some(&external_space_before_sleep)
            );
        }
        other => panic!("unexpected wm event: {other:?}"),
    }

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "builtin", 0.0, 1000.0, Some(builtin_space)),
            make_screen_with(2, "external", 1000.0, 1000.0, Some(external_space_after_wake)),
        ],
        CoordinateConverter::from_height(800.0),
    );
    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(state.space_remaps, vec![(
                external_space_before_sleep,
                external_space_after_wake
            )]);
            assert!(!state.space_remaps.contains(&(builtin_space, external_space_after_wake)));
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

#[test]
fn topology_window_delta_is_emitted_when_windows_leave_space_during_churn_without_space_change() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let space = SpaceId::new(301);
    let wsid = WindowServerId::new(77);

    actor.state.forwarded.visible_window_spaces.insert(wsid, space);

    actor.forward_screen_parameters(
        vec![make_screen(Some(space))],
        CoordinateConverter::from_height(800.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.hand_window_server().set_space_window_list(space.get(), Some(vec![]));
    actor.synthesize_topology_window_delta(
        9,
        rini_skylight_sys::DisplayReconfigFlags::MOVED,
        HashMap::from_iter([(wsid, space)]),
        &[make_screen(Some(space))],
    );
    actor.forward_screen_parameters(
        vec![make_screen(Some(space))],
        CoordinateConverter::from_height(800.0),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            let delta = state.topology_window_delta.expect("expected topology window delta");
            assert_eq!(delta.epoch, 9);
            assert!(delta.appeared.is_empty());
            assert_eq!(delta.disappeared, vec![(wsid, space)]);
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

#[test]
fn first_empty_post_wake_snapshot_preserves_known_visible_windows() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let space = SpaceId::new(302);
    let wsid = WindowServerId::new(79);

    actor.state.forwarded.screens = vec![make_screen(Some(space))];
    actor.state.forwarded.visible_window_spaces.insert(wsid, space);
    actor.state.lifecycle = Lifecycle::Releasing(None);
    actor.hand_window_server().set_space_window_list(space.get(), Some(vec![]));

    actor.forward_screen_parameters(
        vec![make_screen(Some(space))],
        CoordinateConverter::from_height(800.0),
    );
    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(state.active_window_spaces.get(&wsid), Some(&space));
            assert!(state.releases_lifecycle_refresh_quarantine);
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

#[test]
fn topology_window_delta_treats_same_window_space_move_as_remove_then_add() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let old_space = SpaceId::new(311);
    let new_space = SpaceId::new(312);
    let wsid = WindowServerId::new(78);

    actor.state.forwarded.visible_window_spaces.insert(wsid, old_space);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(old_space)),
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(new_space)),
        ],
        CoordinateConverter::from_height(800.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.state.forwarded.visible_window_spaces.clear();
    actor.state.forwarded.visible_window_spaces.insert(wsid, new_space);
    let ws = actor.hand_window_server();
    ws.set_space_window_list(new_space.get(), Some(vec![wsid.as_u32()]));
    actor.synthesize_topology_window_delta(
        10,
        rini_skylight_sys::DisplayReconfigFlags::MOVED,
        HashMap::from_iter([(wsid, old_space)]),
        &[
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(old_space)),
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(new_space)),
        ],
    );
    ws.set_space_window_list(new_space.get(), None);
    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(old_space)),
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(new_space)),
        ],
        CoordinateConverter::from_height(800.0),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            let delta = state.topology_window_delta.expect("expected topology window delta");
            assert_eq!(delta.disappeared, vec![(wsid, old_space)]);
            assert_eq!(delta.appeared, vec![(wsid, new_space)]);
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

/// The churn's own epoch, flags and windows reach the forwarded delta, not ones a caller hands in.
#[test]
fn a_settled_churn_forwards_the_windows_that_left_since_it_began_with_its_epoch_and_flags() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    let space = SpaceId::new(321);
    let wsid = WindowServerId::new(81);
    actor.forward_screen_parameters(
        vec![make_screen(Some(space))],
        CoordinateConverter::from_height(800.0),
    );
    let _ = recv_wm(&mut wm_rx);
    actor.state.forwarded.visible_window_spaces.insert(wsid, space);

    actor.handle_event(Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::MOVED,
    });
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::DisplayChurnBegin
    ));
    actor.hand_window_server().set_space_window_list(space.get(), Some(vec![]));
    let epoch = actor.state.churn.epoch();
    for attempt in 0..2 {
        actor.handle_event(Notification::CheckDisplayStabilization {
            expected_epoch: epoch,
            attempt,
        });
    }

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            let delta = state.topology_window_delta.expect("expected topology window delta");
            assert_eq!(delta.epoch, epoch);
            assert_eq!(delta.flags, rini_skylight_sys::DisplayReconfigFlags::MOVED);
            assert!(delta.appeared.is_empty());
            assert_eq!(delta.disappeared, vec![(wsid, space)]);
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::DisplayChurnEnd
    ));
    assert!(!actor.state.churn.is_active());
}

#[test]
fn display_order_change_is_topology_change_without_display_set_change() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let left_space = SpaceId::new(401);
    let right_space = SpaceId::new(402);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(left_space)),
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right_space)),
        ],
        CoordinateConverter::from_height(800.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right_space)),
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(left_space)),
        ],
        CoordinateConverter::from_height(800.0),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(state.topology, Topology::Remap(DisplaySet::Same));
            assert!(state.space_remaps.is_empty());
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

#[test]
fn normal_refresh_retries_duplicate_user_space_snapshot_until_valid() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let left = SpaceId::new(521);
    let right = SpaceId::new(522);

    actor.state.forwarded.screens = vec![
        make_screen_with(1, "display-left", 0.0, 1000.0, Some(left)),
        make_screen_with(2, "display-right", 1000.0, 1000.0, Some(left)),
    ];
    actor.state.refresh = actor.state.refresh.next(RefreshEdge::Scheduled);

    actor.process_screen_refresh(0);
    assert_no_wm_event(&mut wm_rx);
    assert!(
        actor.state.refresh.is_pending(),
        "an invalid duplicate-space snapshot should stay pending so the refresh can retry"
    );

    actor.state.forwarded.screens = vec![
        make_screen_with(1, "display-left", 0.0, 1000.0, Some(left)),
        make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right)),
    ];

    actor.process_screen_refresh(1);

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(
                state.screens.iter().map(|screen| screen.space).collect::<Vec<_>>(),
                vec![Some(left), Some(right)]
            );
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
    assert!(
        !actor.state.refresh.is_pending(),
        "refresh should complete once the authoritative snapshot becomes valid"
    );
}

#[test]
fn authoritative_snapshot_rejects_duplicate_user_space_snapshot() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let left = SpaceId::new(531);

    actor.state.forwarded.screens = vec![
        make_screen_with(1, "display-left", 0.0, 1000.0, Some(left)),
        make_screen_with(2, "display-right", 1000.0, 1000.0, Some(left)),
    ];

    assert!(
        !actor.try_forward_authoritative_snapshot(true),
        "authoritative refresh path must reject duplicate user-space snapshots"
    );
    assert_no_wm_event(&mut wm_rx);
}

#[test]
fn fullscreen_transition_tracks_display_identity_across_reordered_screens() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let left_space_2 = SpaceId::new(12);
    let left_space_1 = SpaceId::new(11);
    let right_space_1 = SpaceId::new(21);
    let right_fullscreen = fullscreen_space_for(right_space_1);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(left_space_2)),
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right_space_1)),
        ],
        CoordinateConverter::from_height(1000.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right_fullscreen)),
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(left_space_1)),
        ],
        CoordinateConverter::from_height(1000.0),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(state.screens[0].display_uuid, "display-right");
            assert_eq!(state.screens[0].space, None);
            assert_eq!(state.screens[1].display_uuid, "display-left");
            assert_eq!(state.screens[1].space, Some(left_space_1));
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

#[test]
fn fullscreen_transition_rewrites_cross_display_space_contamination_only() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let left_space = SpaceId::new(212);
    let right_space = SpaceId::new(221);
    let right_fullscreen = fullscreen_space_for(right_space);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(left_space)),
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right_space)),
        ],
        CoordinateConverter::from_height(1000.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(right_space)),
            make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right_fullscreen)),
        ],
        CoordinateConverter::from_height(1000.0),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            let spaces: Vec<Option<SpaceId>> =
                state.screens.iter().map(|screen| screen.space).collect();
            assert_eq!(spaces, vec![Some(left_space), None]);
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

#[test]
fn display_churn_stabilization_rejects_duplicate_space_snapshot_until_valid() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    let left = SpaceId::new(601);
    let right = SpaceId::new(602);

    actor.handle_event(Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::MOVED,
    });
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::DisplayChurnBegin
    ));

    let epoch = actor.state.churn.epoch();
    actor.state.forwarded.screens = vec![
        make_screen_with(1, "display-left", 0.0, 1000.0, Some(left)),
        make_screen_with(2, "display-right", 1000.0, 1000.0, Some(left)),
    ];

    actor.attempt_finish_display_churn(epoch, 0);
    actor.attempt_finish_display_churn(epoch, 1);
    assert_no_wm_event(&mut wm_rx);

    actor.state.forwarded.screens = vec![
        make_screen_with(1, "display-left", 0.0, 1000.0, Some(left)),
        make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right)),
    ];

    actor.attempt_finish_display_churn(epoch, 2);
    actor.attempt_finish_display_churn(epoch, 3);

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(
                state.screens.iter().map(|screen| screen.space).collect::<Vec<_>>(),
                vec![Some(left), Some(right)]
            );
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::DisplayChurnEnd
    ));
    assert_no_reactor_event(&mut reactor_rx);
}

/// The churn ends on the actor's own reader of window-server activity, never the process-wide clock
/// behind the production one, which every reactor test running beside this one sets.
#[test]
fn display_churn_waits_for_a_quiet_window_server_as_its_reader_reports_it() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    actor.state.live.window_server_quiet = || false;
    let (left, right) = (SpaceId::new(611), SpaceId::new(612));

    actor.handle_event(Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::MOVED,
    });
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::DisplayChurnBegin
    ));
    let epoch = actor.state.churn.epoch();
    actor.state.forwarded.screens = vec![
        make_screen_with(1, "display-left", 0.0, 1000.0, Some(left)),
        make_screen_with(2, "display-right", 1000.0, 1000.0, Some(right)),
    ];

    actor.attempt_finish_display_churn(epoch, 0);
    actor.attempt_finish_display_churn(epoch, 1);
    assert_no_wm_event(&mut wm_rx);
    assert!(
        actor.state.churn.is_active(),
        "a busy window server holds the churn open"
    );

    crate::windows::platform::window_server::note_windowserver_activity(611);
    actor.state.live.window_server_quiet = || true;
    actor.attempt_finish_display_churn(epoch, 2);
    assert!(matches!(recv_wm(&mut wm_rx), OutEvent::SpaceStateUpdated(..)));
    assert!(!actor.state.churn.is_active());
}

/// Safe beside every other test: they only move the clock later, which keeps it busy too.
#[test]
fn the_production_reader_reports_a_window_server_that_just_moved_as_busy() {
    crate::windows::platform::window_server::note_windowserver_activity(1);
    assert!(!(LiveDisplays::from_window_server().window_server_quiet)());
}

#[test]
fn duplicate_visible_spaces_disable_remaps_and_layout_forcing() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();

    actor.forward_screen_parameters(
        vec![make_screen_with(
            1,
            "display-1",
            0.0,
            1000.0,
            Some(SpaceId::new(91)),
        )],
        CoordinateConverter::from_height(800.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-1", 0.0, 1000.0, Some(SpaceId::new(92))),
            make_screen_with(2, "display-2", 1000.0, 1000.0, Some(SpaceId::new(92))),
        ],
        CoordinateConverter::from_height(800.0),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(state.topology, Topology::Relayout(DisplaySet::Changed));
            assert!(state.space_remaps.is_empty());
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

#[test]
fn resize_updates_are_treated_as_topology_changes_and_report_resized_spaces() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let space = SpaceId::new(101);

    actor.forward_screen_parameters(
        vec![make_screen_with(1, "display-1", 0.0, 1000.0, Some(space))],
        CoordinateConverter::from_height(800.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.forward_screen_parameters(
        vec![make_screen_with(1, "display-1", 0.0, 1200.0, Some(space))],
        CoordinateConverter::from_height(800.0),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(state.topology, Topology::Remap(DisplaySet::Same));
            assert_eq!(state.resized_spaces, vec![(space, CGSize::new(1200.0, 800.0))]);
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

#[test]
fn display_origin_change_is_treated_as_topology_change() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let space = SpaceId::new(111);

    actor.forward_screen_parameters(
        vec![make_screen_with(1, "display-1", 0.0, 1000.0, Some(space))],
        CoordinateConverter::from_height(800.0),
    );
    let _ = recv_wm(&mut wm_rx);

    actor.forward_screen_parameters(
        vec![make_screen_with(1, "display-1", 200.0, 1000.0, Some(space))],
        CoordinateConverter::from_height(800.0),
    );

    match recv_wm(&mut wm_rx) {
        OutEvent::SpaceStateUpdated(state, _) => {
            assert_eq!(state.topology, Topology::Remap(DisplaySet::Same));
            assert!(state.resized_spaces.is_empty());
        }
        other => panic!("unexpected wm event: {other:?}"),
    }
}

/// A login or system space must never reach the reactor as a display's space.
///
/// `SLSSpaceGetType == 0` is the only thing that counts as a user space. This rule was unreachable
/// from a test until the classifier became injectable: the old `#[cfg(test)]` fork of `is_user_space`
/// returned `true` for every id, so under test there was no such thing as a non-user space.
#[test]
fn a_non_user_space_is_nulled_out_before_it_is_forwarded() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor_with_a_non_user_space();

    actor.forward_screen_parameters(
        vec![make_screen(Some(SpaceId::new(NON_USER_SPACE)))],
        CoordinateConverter::default(),
    );

    let OutEvent::SpaceStateUpdated(state, _) = recv_wm(&mut wm_rx) else {
        panic!("expected a topology snapshot");
    };
    assert_eq!(
        state.screens.iter().map(|screen| screen.space).collect::<Vec<_>>(),
        vec![None],
        "the login space is nulled out, so the reactor never assigns a window to it"
    );
}

/// The same snapshot with an ordinary id keeps its space, which is what makes the test above a test
/// of the rule rather than of the screen plumbing.
#[test]
fn a_user_space_is_forwarded_unchanged() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor_with_a_non_user_space();
    let space = SpaceId::new(NON_USER_SPACE + 1);

    actor.forward_screen_parameters(vec![make_screen(Some(space))], CoordinateConverter::default());

    let OutEvent::SpaceStateUpdated(state, _) = recv_wm(&mut wm_rx) else {
        panic!("expected a topology snapshot");
    };
    assert_eq!(
        state.screens.iter().map(|screen| screen.space).collect::<Vec<_>>(),
        vec![Some(space)]
    );
}

/// A reader that answers, built from the two ids below.
fn build_actor_seeing_display_and_space() -> (
    SpacesActor,
    channel::Receiver<OutEvent>,
    channel::Receiver<OutEvent>,
) {
    let (wm_tx, wm_rx) = channel::channel();
    let (reactor_tx, reactor_rx) = channel::channel();
    let live = LiveDisplays {
        active_menu_bar_display: || Some(ACTIVE_DISPLAY.to_string()),
        active_space: || Some(SpaceId::new(ACTIVE_SPACE)),
        spaces_per_display: || {
            let mut map = HashMap::default();
            map.insert(ACTIVE_DISPLAY.to_string(), vec![
                SpaceId::new(ACTIVE_SPACE),
                SpaceId::new(99),
            ]);
            map
        },
        window_server_quiet: || true,
    };
    let actor = SpacesActor::for_test(
        Box::new(Split {
            topology: wm_tx,
            rest: reactor_tx,
        }),
        SpaceKinds::for_tests(),
        live,
    );
    (actor, wm_rx, reactor_rx)
}

const ACTIVE_DISPLAY: &str = "display-right";
const ACTIVE_SPACE: u64 = 77;

/// The command space is the space on the display whose menu bar is active, not the first screen in
/// the snapshot. This rule was unreachable from a test: `resolve_command_space` had a
/// `#[cfg(test)]` body that discarded both arguments and ran a different algorithm.
#[test]
fn the_command_space_is_the_active_display_s_space_not_the_first_screen_s() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor_seeing_display_and_space();

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(SpaceId::new(1))),
            make_screen_with(
                2,
                ACTIVE_DISPLAY,
                1000.0,
                1000.0,
                Some(SpaceId::new(ACTIVE_SPACE)),
            ),
        ],
        CoordinateConverter::default(),
    );

    let OutEvent::SpaceStateUpdated(state, _) = recv_wm(&mut wm_rx) else {
        panic!("expected a topology snapshot");
    };
    assert_eq!(
        state.command_space,
        Some(SpaceId::new(ACTIVE_SPACE)),
        "the active display's space, even though display-left is first in the list"
    );
}

/// The menu bar space is the active space when a screen actually has it.
#[test]
fn the_menu_bar_space_is_the_active_space_when_a_screen_has_it() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor_seeing_display_and_space();

    actor.forward_screen_parameters(
        vec![
            make_screen_with(1, "display-left", 0.0, 1000.0, Some(SpaceId::new(1))),
            make_screen_with(
                2,
                ACTIVE_DISPLAY,
                1000.0,
                1000.0,
                Some(SpaceId::new(ACTIVE_SPACE)),
            ),
        ],
        CoordinateConverter::default(),
    );

    let OutEvent::SpaceStateUpdated(state, _) = recv_wm(&mut wm_rx) else {
        panic!("expected a topology snapshot");
    };
    assert_eq!(state.menu_bar_space, Some(SpaceId::new(ACTIVE_SPACE)));
}

/// And it is ignored when no screen has it. The window server can name a space that belongs to a
/// display the snapshot does not carry, mid-reconfiguration; believing it puts the menu bar on a
/// screen that is not there.
#[test]
fn an_active_space_no_screen_has_is_not_the_menu_bar_space() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor_seeing_display_and_space();

    actor.forward_screen_parameters(
        vec![make_screen_with(
            1,
            "display-left",
            0.0,
            1000.0,
            Some(SpaceId::new(1)),
        )],
        CoordinateConverter::default(),
    );

    let OutEvent::SpaceStateUpdated(state, _) = recv_wm(&mut wm_rx) else {
        panic!("expected a topology snapshot");
    };
    assert_eq!(
        state.menu_bar_space,
        Some(SpaceId::new(1)),
        "the only screen's space, not the space the window server named"
    );
}

/// Which spaces a display owns comes from the window server, which knows about spaces no screen is
/// currently showing. Deriving it from the snapshot can only ever find the one space per display
/// that the snapshot already names.
#[test]
fn the_spaces_a_display_owns_come_from_the_window_server() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor_seeing_display_and_space();

    actor.forward_screen_parameters(
        vec![make_screen_with(
            2,
            ACTIVE_DISPLAY,
            0.0,
            1000.0,
            Some(SpaceId::new(ACTIVE_SPACE)),
        )],
        CoordinateConverter::default(),
    );

    let OutEvent::SpaceStateUpdated(state, _) = recv_wm(&mut wm_rx) else {
        panic!("expected a topology snapshot");
    };
    assert_eq!(
        state.display_space_ids.get(ACTIVE_DISPLAY),
        Some(&vec![SpaceId::new(ACTIVE_SPACE), SpaceId::new(99)]),
        "both spaces, including the one nothing is showing"
    );
    assert!(!state.display_space_ids_from_screens);
}

/// With a silent window server the snapshot stands in, so a display still owns the space it is
/// showing rather than owning nothing.
#[test]
fn a_silent_window_server_falls_back_to_the_snapshot() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let space = SpaceId::new(5);

    actor.forward_screen_parameters(
        vec![make_screen_with(
            1,
            "display-left",
            0.0,
            1000.0,
            Some(space),
        )],
        CoordinateConverter::default(),
    );

    let OutEvent::SpaceStateUpdated(state, _) = recv_wm(&mut wm_rx) else {
        panic!("expected a topology snapshot");
    };
    assert_eq!(state.display_space_ids.get("display-left"), Some(&vec![space]));
    assert!(
        state.display_space_ids_from_screens,
        "not a listing a save may prune by"
    );
}

/// Feeds back every refresh the actor schedules until it schedules none, and gives the attempt of
/// each one, in order.
fn run_refreshes(actor: &mut SpacesActor, scheduled: &Recorded) -> Vec<u8> {
    let mut attempts = Vec::new();
    loop {
        let due = scheduled.take();
        if due.is_empty() {
            return attempts;
        }
        for (_, notification) in due {
            let Notification::ProcessScreenRefresh { attempt } = notification else {
                panic!("only refreshes are scheduled here, not {notification:?}");
            };
            attempts.push(attempt);
            if attempts.len() > 3 * usize::from(REFRESH_MAX_RETRIES) {
                return attempts;
            }
            actor.handle_event(notification);
        }
    }
}

/// A snapshot that never becomes one the actor can forward is asked for again
/// `REFRESH_MAX_RETRIES` times, and then the refresh stops.
#[test]
fn a_refresh_asks_again_for_a_snapshot_it_cannot_forward_up_to_its_cap_then_stops() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let scheduled = actor.hand_schedule();
    let space = SpaceId::new(701);
    actor.state.forwarded.screens = vec![
        make_screen_with(1, "display-left", 0.0, 1000.0, Some(space)),
        make_screen_with(2, "display-right", 1000.0, 1000.0, Some(space)),
    ];

    actor.handle_event(Notification::ScreenRefreshRequested);

    assert_eq!(
        run_refreshes(&mut actor, &scheduled),
        (0..=REFRESH_MAX_RETRIES).collect::<Vec<_>>(),
        "the first refresh, then one retry per attempt up to the cap"
    );
    assert!(!actor.state.refresh.is_pending());
    assert_no_wm_event(&mut wm_rx);
}

/// A refresh owed to a wake starts over once its retries run out, since only a forwarded snapshot
/// releases the reactor's lifecycle quarantine.
#[test]
fn a_refresh_owed_to_a_wake_starts_over_once_its_retries_run_out() {
    let (mut actor, _wm_rx, _reactor_rx) = build_actor();
    let scheduled = actor.hand_schedule();
    actor.state.lifecycle = Lifecycle::Releasing(None);
    actor.state.refresh = actor.state.refresh.next(RefreshEdge::Scheduled);

    actor.handle_event(Notification::ProcessScreenRefresh { attempt: REFRESH_MAX_RETRIES });

    let due = scheduled.take();
    assert!(
        matches!(due.as_slice(), [(_, Notification::ProcessScreenRefresh {
            attempt: 0
        })]),
        "{due:?}"
    );
    assert!(actor.state.refresh.is_pending());
}

/// While a space switch is still unconfirmed, a snapshot showing the spaces already forwarded is
/// asked for again up to the cap, and the last one is forwarded as it is.
#[test]
fn an_unconfirmed_space_switch_asks_again_up_to_the_cap_then_forwards_what_it_has() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let scheduled = actor.hand_schedule();
    let space = SpaceId::new(711);
    actor.state.forwarded.screens = vec![make_screen(Some(space))];
    actor.state.last_sent_spaces = Some(vec![Some(space)]);
    actor.state.refresh = actor.state.refresh.next(RefreshEdge::SpaceSwitched);
    actor.state.refresh = actor.state.refresh.next(RefreshEdge::Scheduled);
    actor.handle_event(Notification::ProcessScreenRefresh { attempt: 0 });
    assert_no_wm_event(&mut wm_rx);

    let attempts = run_refreshes(&mut actor, &scheduled);

    assert_eq!(attempts, (1..=REFRESH_MAX_RETRIES).collect::<Vec<_>>());
    assert!(matches!(recv_wm(&mut wm_rx), OutEvent::SpaceStateUpdated(..)));
    assert!(!actor.state.refresh.awaits_space_switch());
}

/// Feeds back every churn check the actor schedules until it schedules none, and gives the attempt
/// of each one, in order, and what else it scheduled.
fn run_checks(actor: &mut SpacesActor, scheduled: &Recorded) -> (Vec<u8>, Vec<Notification>) {
    let (mut attempts, mut rest) = (Vec::new(), Vec::new());
    loop {
        let due = scheduled.take();
        if due.is_empty() {
            return (attempts, rest);
        }
        for (_, notification) in due {
            match notification {
                Notification::CheckDisplayStabilization { attempt, .. } => {
                    attempts.push(attempt);
                    if attempts.len() > 3 * usize::from(DISPLAY_STABILIZE_MAX_ATTEMPTS) {
                        return (attempts, rest);
                    }
                    actor.handle_event(notification);
                }
                other => rest.push(other),
            }
        }
    }
}

/// A churn whose window server never goes quiet is checked again `DISPLAY_STABILIZE_MAX_ATTEMPTS`
/// times, then ends unsettled with a refresh, forwarding nothing itself.
#[test]
fn a_churn_that_never_settles_is_checked_up_to_its_cap_then_ends_with_a_refresh() {
    let (mut actor, mut wm_rx, mut reactor_rx) = build_actor();
    let scheduled = actor.hand_schedule();
    actor.state.live.window_server_quiet = || false;
    actor.state.forwarded.screens = vec![make_screen(Some(SpaceId::new(721)))];

    actor.handle_event(Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::ADD,
    });
    let (attempts, rest) = run_checks(&mut actor, &scheduled);

    assert_eq!(
        attempts,
        (0..=DISPLAY_STABILIZE_MAX_ATTEMPTS).collect::<Vec<_>>()
    );
    assert!(!actor.state.churn.is_active());
    assert!(
        matches!(rest.as_slice(), [Notification::ProcessScreenRefresh {
            attempt: 0
        }]),
        "{rest:?}"
    );
    assert!(matches!(
        recv_reactor(&mut reactor_rx),
        OutEvent::DisplayChurnBegin
    ));
    assert!(
        matches!(recv_reactor(&mut reactor_rx), OutEvent::DisplayChurnEnd),
        "a churn given up on tells the reactor it ended, with no snapshot ahead of it"
    );
    assert_no_wm_event(&mut wm_rx);
}

/// With no screens to sample at all, the churn is checked up to the same cap.
#[test]
fn a_churn_with_no_screens_is_checked_up_to_its_cap_too() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let scheduled = actor.hand_schedule();

    actor.handle_event(Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::REMOVE,
    });
    let (attempts, _) = run_checks(&mut actor, &scheduled);

    assert_eq!(
        attempts,
        (0..=DISPLAY_STABILIZE_MAX_ATTEMPTS).collect::<Vec<_>>()
    );
    assert!(!actor.state.churn.is_active());
    assert_no_wm_event(&mut wm_rx);
}

/// The reactor reads its churn in the order of this one stream: a settled churn's snapshot after
/// its begin and ahead of its end, and the next churn's begin after that end.
#[test]
fn a_settled_churn_sends_its_begin_its_snapshot_and_its_end_in_that_order() {
    let (events, mut stream) = channel::channel::<OutEvent>();
    let mut actor = SpacesActor::new_for_tests(Box::new(events));
    let scheduled = actor.hand_schedule();
    actor.state.forwarded.screens = vec![make_screen(Some(SpaceId::new(741)))];
    let moved = Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::MOVED,
    };

    actor.handle_event(moved.clone());
    run_checks(&mut actor, &scheduled);
    actor.handle_event(moved);

    let mut sent = Vec::new();
    while let Ok((_, event)) = stream.try_recv() {
        sent.push(event);
    }
    assert!(
        matches!(sent.as_slice(), [
            OutEvent::DisplayChurnBegin,
            OutEvent::SpaceStateUpdated(..),
            OutEvent::DisplayChurnEnd,
            OutEvent::DisplayChurnBegin,
        ]),
        "{sent:?}"
    );
}

/// A settled churn forwards on its second check, and schedules nothing after it.
#[test]
fn a_churn_that_settles_forwards_on_its_second_check_and_asks_nothing_more() {
    let (mut actor, mut wm_rx, _reactor_rx) = build_actor();
    let scheduled = actor.hand_schedule();
    actor.state.forwarded.screens = vec![make_screen(Some(SpaceId::new(731)))];

    actor.handle_event(Notification::DisplayReconfigured {
        flags: rini_skylight_sys::DisplayReconfigFlags::MOVED,
    });
    let (attempts, rest) = run_checks(&mut actor, &scheduled);

    assert_eq!(attempts, vec![0, 1]);
    assert!(rest.is_empty(), "{rest:?}");
    assert!(matches!(recv_wm(&mut wm_rx), OutEvent::SpaceStateUpdated(..)));
    assert!(!actor.state.churn.is_active());
}
