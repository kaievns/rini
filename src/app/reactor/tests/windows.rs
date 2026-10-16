//! Which windows rini takes on, and what it refuses.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::WindowServerId;
use test_log::test;

use super::fixtures::*;
use crate::app::reactor::events::window_discovery;
use crate::app::reactor::managers::QuarantineSignal;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::windows::domain::manageable::Liveness;
use crate::windows::domain::request::Request;
use crate::workspaces::{LayoutCommand, LayoutEvent};

#[test]
fn it_sends_writes_when_stale_read_state_looks_same_as_written_state() {
    let (mut apps, mut reactor) = test_context();
    reactor.handle_event(space_state_event(
        vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
        vec![Some(SpaceId::new(1))],
    ));

    reactor.handle_events(apps.make_app(1, make_windows(2)));
    let events_1 = apps.simulate_events();
    let state_1 = apps.windows.clone();
    assert!(!state_1.is_empty());

    for event in events_1 {
        reactor.handle_event(event);
    }
    assert!(apps.requests().is_empty());

    reactor.handle_events(apps.make_app(2, make_windows(1)));
    let _events_2 = apps.simulate_events();

    reactor.handle_event(Event::WindowDestroyed(WindowId::new(2, 1)));
    let _events_3 = apps.simulate_events();
    let state_3 = apps.windows;

    // These should be the same, because we should have resized the first
    // two windows both at the beginning, and at the end when the third
    // window was destroyed.
    for (wid, state) in dbg!(state_1) {
        assert!(state_3.contains_key(&wid), "{wid:?} not in {state_3:#?}");
        assert_eq!(state.frame, state_3[&wid].frame);
    }
}

#[test]
fn duplicate_minimize_deminimize_and_unknown_window_events_do_not_arrange() {
    let OneWindow { mut reactor, window: wid, .. } = reactor_with_window_on_space1();

    reactor.dispatch_workflow(Event::WindowMinimized(wid)).unwrap();
    let duplicate_minimize = reactor.dispatch_workflow(Event::WindowMinimized(wid)).unwrap();
    assert!(!duplicate_minimize.arrange.requested);

    reactor.dispatch_workflow(Event::WindowDeminiaturized(wid)).unwrap();
    let duplicate_deminimize = reactor.dispatch_workflow(Event::WindowDeminiaturized(wid)).unwrap();
    assert!(!duplicate_deminimize.arrange.requested);

    let unknown = WindowId::new(wid.pid + 100, wid.idx.get());
    let unknown_minimize = reactor.dispatch_workflow(Event::WindowMinimized(unknown)).unwrap();
    let unknown_deminimize =
        reactor.dispatch_workflow(Event::WindowDeminiaturized(unknown)).unwrap();
    let unknown_frame = reactor
        .dispatch_workflow(Event::WindowFrameChanged {
            window: unknown,
            frame: CGRect::default(),
            last_seen: None,
            requested: Requested(false),
            mouse_state: Some(MouseState::Up),
        })
        .unwrap();

    assert!(!unknown_minimize.arrange.requested);
    assert!(!unknown_deminimize.arrange.requested);
    assert!(!unknown_frame.arrange.requested);
}

#[test]
fn it_ignores_windows_on_nonzero_layers() {
    let (mut apps, mut reactor) = test_context();
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(SpaceId::new(1))]));

    reactor.handle_events(apps.make_app_with_opts(
        1,
        make_windows(1),
        AppLaunch::frontmost(None).without_ws_info(),
    ));

    let state_before = apps.windows.clone();
    let _events = apps.simulate_events();
    assert_eq!(state_before, apps.windows, "Window should not have been moved",);

    // Make sure it doesn't choke on destroyed events for ignored windows.
    reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 1)));
    reactor.handle_event(Event::WindowCreated {
        window: WindowId::new(1, 2),
        info: make_window(2),
        server_info: None,
        mouse_state: Some(MouseState::Up),
    });
    reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 2)));
}

#[test]
fn session_lock_quarantine_ignores_discovery_and_replays_one_refresh_after_unlock() {
    let (mut apps, mut reactor) = test_context_with_workspace_count(2);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));

    let workspaces = reactor.test_workspace_ids(space);
    let secondary_workspace = workspaces[1];

    assert!(reactor.assign_test_window_to_workspace(space, wid, secondary_workspace));
    assert!(reactor.set_test_active_workspace(space, secondary_workspace));

    assert!(apps.requests().is_empty());

    reactor.handle_event(Event::SessionDidResignActive);
    reactor.discover_test_windows(1, vec![], vec![]);
    reactor.handle_event(Event::ApplicationGloballyActivated(1));

    let requests = apps.requests();
    assert!(
        requests.iter().all(|request| !matches!(request, Request::GetVisibleWindows)),
        "locked-session discovery should defer visible-window enumeration: {requests:?}"
    );
    assert!(
        requests.iter().any(
            |request| matches!(request, Request::ApplicationGloballyActivated(pid) if *pid == 1)
        ),
        "Carbon activation should still be reconciled by the app thread: {requests:?}"
    );
    assert_eq!(
        reactor.test_workspace_for_window(space, wid),
        Some(secondary_workspace),
        "ignored lock-session discovery must not reassign the window back to the default workspace"
    );

    reactor.handle_event(Event::SessionDidBecomeActive);
    assert!(
        apps.requests().is_empty(),
        "unlock should stay quarantined until the spaces actor publishes a fresh post-unlock snapshot"
    );
    let stale_snapshot = space_state_event(vec![screen], vec![Some(space)]);
    reactor.handle_event(stale_snapshot);
    assert!(
        apps.requests().is_empty(),
        "an older queued WM snapshot must not release the unlock quarantine"
    );

    let fresh_snapshot = space_state_event_with(vec![screen], vec![Some(space)], |state| {
        state.releases_lifecycle_refresh_quarantine = true
    });
    reactor.handle_event(fresh_snapshot);

    let requests = apps.requests();
    assert_eq!(
        requests
            .into_iter()
            .filter(|request| matches!(request, Request::GetVisibleWindows))
            .count(),
        1,
        "the first fresh post-unlock snapshot should flush exactly one deferred visibility refresh"
    );
}

#[test]
fn stale_cleanup_uses_ordered_state_instead_of_cached_visibility() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    let wsid = reactor.test_window_server_id(wid);
    let info = reactor
        .windows()
        .get_window_server_info(wsid)
        .expect("test window should have native metadata");
    assert!(reactor.windows().is_window_visible(wsid));

    let snapshot = |suitable, ordered_in| window_discovery::StaleCleanupSnapshot {
        pending_refresh: false,
        mid_change: None,
        inactive_windows: Default::default(),
        server_observations: [(wsid, window_discovery::StaleWindowObservation {
            info: Some(info),
            liveness: Liveness { suitable, ordered_in },
        })]
        .into_iter()
        .collect(),
    };

    let (ordered_stale, _) = window_discovery::identify_stale_windows(
        reactor.rini_state(),
        wid.pid,
        &[],
        &snapshot(Some(true), Some(true)),
    );
    assert!(
        ordered_stale.is_empty(),
        "temporary AX omission must preserve an ordered-in window"
    );

    let (closed_stale, _) = window_discovery::identify_stale_windows(
        reactor.rini_state(),
        wid.pid,
        &[],
        &snapshot(Some(true), Some(false)),
    );
    assert_eq!(
        closed_stale,
        vec![wid],
        "an ordered-out window must be retired even when cached visibility is stale",
    );

    let (unknown_stale, _) = window_discovery::identify_stale_windows(
        reactor.rini_state(),
        wid.pid,
        &[],
        &snapshot(Some(true), None),
    );
    assert!(
        unknown_stale.is_empty(),
        "an unavailable ordered-state query must not remove a valid layout node",
    );

    let (unknown_suitability_stale, _) = window_discovery::identify_stale_windows(
        reactor.rini_state(),
        wid.pid,
        &[],
        &snapshot(None, Some(true)),
    );
    assert!(
        unknown_suitability_stale.is_empty(),
        "an unavailable suitability query must not remove a valid layout node",
    );
}

/// Switching away from a workspace must not erase where focus was in it.
///
/// apply_focus_response cleared the workspace's remembered focus whenever the focused window
/// was not one of its members — which is every ordinary focus change to another display or
/// workspace. Switching back then fell through to the first column, reported as always
/// landing on the first window and reading as visual chaos.
///
/// CAVEAT: this test passes with that clearing restored, so it documents the intended
/// property rather than pinning the fix. The clearing branch is only reached when focus lands
/// on a window outside the workspace being applied, and the test harness drives focus through
/// paths that do not produce that combination. Left in place because the property is worth
/// stating; do not read a green result here as proof the fix works.
#[test]
fn switching_away_and_back_returns_to_the_same_window() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);
    let first = WindowId::new(1, 1);
    let second = WindowId::new(1, 2);

    set_space_membership(&reactor, &[(space, &[901, 902])]);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.add_test_app(1);
    let workspaces = reactor.test_workspace_ids(space);
    for (window, wsid) in [(first, 901u32), (second, 902)] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), screen);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspaces[0]));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
    }

    // Sit on the SECOND window, then leave the workspace and come back.
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, second));
    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(1));
    // Focus something that is NOT a member of workspace 0 while away. This is the case that
    // used to erase the memory: apply_focus_response cleared it whenever the focused window
    // was not one of the target workspace's own windows.
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, first));
    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(0));

    assert_eq!(
        reactor
            .layout()
            .virtual_workspace_manager()
            .last_focused_window(space, workspaces[0]),
        Some(second),
        "the workspace must still remember which window was focused in it"
    );
}

/// A closed window disappears: the reactor tells the pictures service to forget it exactly once, and
/// sends the engine no flight of its own, on the AX path. See "A closed window disappears" in
/// `src/transitions/docs/coordination.md`.
#[test]
fn a_destroyed_window_is_forgotten_once_and_flies_nothing_of_its_own() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.settings_mut().animate = true;

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    apps.requests();

    let mut animation_rx = reactor.connect_test_transitions();
    let mut pictures = reactor.connect_test_pictures();

    let wid = WindowId::new(1, 1);
    reactor.handle_event(Event::WindowDestroyed(wid));

    let forgotten = forgotten(&mut pictures);
    while let Ok((_, event)) = animation_rx.try_recv() {
        if let crate::transitions::platform::engine::Event::Animate { windows, .. } = event {
            assert!(
                windows.iter().all(|request| request.window != wid),
                "the closed window is not composed"
            );
        }
    }
    assert_eq!(forgotten, vec![wid], "exactly one forget");
    assert!(
        reactor.windows().window(wid).is_none(),
        "the window state is still removed"
    );
}

/// The window server reports a close ~15ms before AX does: still exactly one forget, and the late
/// AX `WindowDestroyed` finds no window and adds nothing.
#[test]
fn a_window_server_promoted_close_is_forgotten_once() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.settings_mut().animate = true;

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    apps.requests();

    let mut animation_rx = reactor.connect_test_transitions();
    let mut pictures = reactor.connect_test_pictures();

    let wid = WindowId::new(1, 1);
    let wsid = reactor.test_window_server_id(wid);

    reactor.window_server().set_window_ordered_in(wsid, Some(false));
    reactor.handle_event(Event::WindowServerDestroyed(wsid, space, SpaceEventKind::User));
    assert!(
        reactor.windows().window(wid).is_none(),
        "the promotion removes the window"
    );

    reactor.handle_event(Event::WindowDestroyed(wid));

    let forgotten = forgotten(&mut pictures);
    while let Ok((_, event)) = animation_rx.try_recv() {
        if let crate::transitions::platform::engine::Event::Animate { windows, .. } = event {
            assert!(
                windows.iter().all(|request| request.window != wid),
                "the closed window is not composed"
            );
        }
    }
    assert_eq!(
        forgotten,
        vec![wid],
        "one forget, from the promotion; the AX destruction adds none"
    );
}

/// Every window the pictures service was told to forget, in order.
fn forgotten(pictures: &mut PicturesAsked) -> Vec<WindowId> {
    pictures_asked(pictures)
        .into_iter()
        .filter_map(|command| match command {
            crate::pictures::platform::service::Command::Forget(window) => Some(window),
            _ => None,
        })
        .collect()
}

/// An application that quits takes its windows with it, and no Accessibility destruction follows
/// once its thread has stopped: each window is forgotten once there, and its record goes.
#[test]
fn an_application_that_quits_forgets_each_of_its_windows_once() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    apps.make_app_and_settle_on_screen(&mut reactor, screen, SpaceId::new(1), 1, make_windows(2));
    apps.make_app_and_settle(&mut reactor, 2, make_windows(1));
    apps.requests();
    let mut pictures = reactor.connect_test_pictures();

    reactor.handle_event(Event::ApplicationThreadTerminated(1));
    reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 1)));

    let mut forgotten = forgotten(&mut pictures);
    forgotten.sort();
    assert_eq!(
        forgotten,
        vec![WindowId::new(1, 1), WindowId::new(1, 2)],
        "each window of the application once, and none of another's"
    );
    assert_eq!(
        reactor.windows().window_ids_for_pid(1).count(),
        0,
        "its records go"
    );
    assert!(reactor.windows().window(WindowId::new(2, 1)).is_some());
}

/// An application that quits with a window in native fullscreen, or one macOS has put there before
/// rini knew the window, takes those records too: macOS hands the window-server ids they are keyed
/// by to other windows.
#[test]
fn an_application_that_quits_takes_its_native_fullscreen_records() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    let fullscreen_space = SpaceId::new(0x400000001);
    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    apps.requests();
    let wid = WindowId::new(1, 1);
    let wsid = reactor.test_window_server_id(wid);
    let unbound = WindowServerId::new(4242);
    let _ = reactor.windows_mut().suspend_window_to_native_fullscreen(
        wid,
        Some(wsid),
        Some(space),
        fullscreen_space,
    );
    let _ = reactor.windows_mut().suspend_window_server_to_native_fullscreen(
        1,
        unbound,
        Some(space),
        fullscreen_space,
    );

    reactor.handle_event(Event::ApplicationThreadTerminated(1));

    let windows = reactor.windows();
    assert!(windows.native_fullscreen_record_for_window(wid).is_none());
    assert!(!windows.is_window_server_id_native_fullscreen_suspended(wsid));
    assert_eq!(
        windows.pending_native_fullscreen_pid_for_window_server_id(unbound),
        None
    );
}

/// An application rini never watched has no thread to answer its quit, and the native-fullscreen
/// record the window server alone gave it still goes.
#[test]
fn an_unwatched_application_that_quits_takes_its_native_fullscreen_records() {
    let (_apps, mut reactor) = test_context();
    let unbound = WindowServerId::new(4242);
    let _ = reactor.windows_mut().suspend_window_server_to_native_fullscreen(
        7,
        unbound,
        Some(SpaceId::new(1)),
        SpaceId::new(0x400000001),
    );

    reactor.handle_event(Event::ApplicationTerminated(7));

    assert_eq!(
        reactor.windows().pending_native_fullscreen_pid_for_window_server_id(unbound),
        None
    );
}

/// A window the application no longer lists and the window server has ordered out is retired by
/// discovery, and forgotten once.
#[test]
fn a_window_stale_cleanup_retires_is_forgotten_once() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    apps.make_app_and_settle_on_screen(&mut reactor, screen, SpaceId::new(1), 1, make_windows(2));
    apps.requests();
    let mut pictures = reactor.connect_test_pictures();
    let (gone, kept) = (WindowId::new(1, 1), WindowId::new(1, 2));
    let wsid = reactor.test_window_server_id(gone);

    reactor.window_server().set_window_ordered_in(wsid, Some(false));
    reactor.discover_test_windows(1, Vec::new(), vec![kept]);
    assert!(
        reactor.windows().window(gone).is_none(),
        "the cleanup retires it"
    );
    reactor.handle_event(Event::WindowDestroyed(gone));

    assert_eq!(forgotten(&mut pictures), vec![gone]);
}

/// A minimised window is no longer managed, and the window server closing it still forgets it once.
#[test]
fn a_minimised_window_the_window_server_closes_is_forgotten_once() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(2));
    let wid = WindowId::new(1, 1);
    reactor.handle_event(Event::WindowMinimized(wid));
    apps.requests();
    let mut pictures = reactor.connect_test_pictures();
    let wsid = reactor.test_window_server_id(wid);

    reactor.window_server().set_window_ordered_in(wsid, Some(false));
    reactor.handle_event(Event::WindowServerDestroyed(wsid, space, SpaceEventKind::User));
    assert!(
        reactor.windows().window(wid).is_none(),
        "the promotion removes the window"
    );
    reactor.handle_event(Event::WindowDestroyed(wid));

    assert_eq!(forgotten(&mut pictures), vec![wid]);
}

/// A window found again under a new id for the same server window leaves its old id behind: the
/// old id is forgotten once, and the new one is not.
#[test]
fn a_rekeyed_window_forgets_its_old_id_once() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    apps.make_app_and_settle_on_screen(&mut reactor, screen, SpaceId::new(1), 1, make_windows(1));
    apps.requests();
    let mut pictures = reactor.connect_test_pictures();
    let (old, new) = (WindowId::new(1, 1), WindowId::new(1, 99));

    rekey_window(&mut reactor, old, new);
    assert!(reactor.windows().window(old).is_none());
    assert!(reactor.windows().window(new).is_some());
    reactor.handle_event(Event::WindowDestroyed(old));

    assert_eq!(forgotten(&mut pictures), vec![old]);
}

/// A window found again under a new id keeps its place in the order the switchers go by, rather
/// than dropping behind windows not touched since.
#[test]
fn a_rekeyed_window_keeps_its_place_in_the_focus_order() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(3));
    apps.requests();
    for idx in 1..=3 {
        reactor.handle_event(Event::WindowServerFocusChanged(WindowId::new(1, idx), space));
    }
    let (old, new) = (WindowId::new(1, 2), WindowId::new(1, 99));

    rekey_window(&mut reactor, old, new);

    let order = reactor.focus_tracker().focus_order();
    assert_eq!(order.position(new), Some(1), "second, where the old id was");
    assert_eq!(order.position(old), None);
}

/// An activation after a rekey returns to the window under its new id, the one the reactor knows.
#[test]
fn an_activation_after_a_rekey_targets_the_new_id() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(2));
    apps.requests();
    let (old, new) = (WindowId::new(1, 1), WindowId::new(1, 99));
    reactor.handle_event(Event::WindowServerFocusChanged(old, space));
    reactor.handle_event(Event::ApplicationGloballyActivated(2));

    rekey_window(&mut reactor, old, new);
    reactor.handle_event(Event::ApplicationGloballyActivated(1));

    assert_eq!(reactor.focus_tracker().peek_activation_target(1), Some(new));
}

/// An Accessibility destruction during a refresh quarantine can be an element replaced while the
/// window lives on, so the window keeps its pictures.
#[test]
fn a_window_destroyed_during_refresh_quarantine_is_not_forgotten() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    apps.make_app_and_settle_on_screen(&mut reactor, screen, SpaceId::new(1), 1, make_windows(2));
    apps.requests();
    let mut pictures = reactor.connect_test_pictures();
    let wid = WindowId::new(1, 1);

    reactor.quarantine_on(QuarantineSignal::WillSleep);
    reactor.handle_event(Event::WindowDestroyed(wid));

    assert!(reactor.windows().window(wid).is_some(), "the window stays");
    assert_eq!(forgotten(&mut pictures), Vec::<WindowId>::new());
}

#[test]
fn a_windows_context_event_becomes_the_reactor_event_with_its_payload_intact() {
    use crate::windows::event::Event as W;
    let wid = WindowId::new(7, 3);
    let frame = CGRect::new(CGPoint::new(1., 2.), CGSize::new(30., 40.));
    let Event::WindowFrameChanged {
        window: got_wid,
        frame: got_frame,
        last_seen: txid,
        requested,
        mouse_state,
    } = Event::from(W::WindowFrameChanged {
        window: wid,
        frame,
        last_seen: None,
        requested: Requested(true),
        mouse_state: None,
    })
    else {
        panic!("wrong variant");
    };
    assert_eq!(
        (got_wid, got_frame, txid, requested.0, mouse_state),
        (wid, frame, None, true, None)
    );
    assert!(matches!(Event::from(W::MenuClosed(7)), Event::MenuClosed(7)));
}

/// A close naming no window goes to the focused window's application unnumbered, so it closes
/// whatever is key there; one naming a window goes to that window's application with its number,
/// and one naming a window rini does not track goes nowhere.
#[test]
fn a_close_goes_to_the_focused_window_or_the_one_it_names() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = Some(SpaceId::new(1));
    let mut first = reactor.add_test_app_listening(1, "com.test.first", "First");
    let mut second = reactor.add_test_app_listening(2, "com.test.second", "Second");
    reactor.add_test_window(WindowId::new(1, 1), WindowServerId::new(901), space, screen);
    reactor.add_test_window(WindowId::new(2, 1), WindowServerId::new(902), space, screen);
    reactor.set_test_focus(WindowId::new(1, 1));
    let close = |window_server_id| {
        Event::Command(Command::Reactor(ReactorCommand::CloseWindow {
            window_server_id,
        }))
    };
    let closes = |app: &mut channels::Receiver<Request>| -> Vec<Option<WindowServerId>> {
        std::iter::from_fn(|| app.try_recv().ok())
            .filter_map(|(_, request)| match request {
                Request::CloseWindow(target) => Some(target),
                _ => None,
            })
            .collect()
    };

    reactor.handle_event(close(None));
    assert_eq!(closes(&mut first), [None]);
    assert!(closes(&mut second).is_empty());

    reactor.handle_event(close(Some(902)));
    assert!(closes(&mut first).is_empty());
    assert_eq!(closes(&mut second), [Some(WindowServerId::new(902))]);

    reactor.handle_event(close(Some(99)));
    assert!(closes(&mut first).is_empty() && closes(&mut second).is_empty());
}
