//! Which windows rini takes on, and what it refuses.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::WindowServerId;
use test_log::test;

use super::fixtures::*;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
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
    let (mut reactor, wid, _wsid, _space1, _space2, _frame) = reactor_with_window_on_space1();

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
        .dispatch_workflow(Event::WindowFrameChanged(
            unknown,
            CGRect::default(),
            None,
            Requested(false),
            Some(MouseState::Up),
        ))
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

    reactor.handle_events(apps.make_app_with_opts(1, make_windows(1), None, true, false));

    let state_before = apps.windows.clone();
    let _events = apps.simulate_events();
    assert_eq!(state_before, apps.windows, "Window should not have been moved",);

    // Make sure it doesn't choke on destroyed events for ignored windows.
    reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 1)));
    reactor.handle_event(Event::WindowCreated(
        WindowId::new(1, 2),
        make_window(2),
        None,
        Some(MouseState::Up),
    ));
    reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 2)));
}

#[test]
fn session_gate_ignores_discovery_and_replays_one_refresh_after_unlock() {
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
        .state
        .windows
        .get_window_server_info(wsid)
        .expect("test window should have native metadata");
    assert!(reactor.state.windows.is_window_visible(wsid));

    let snapshot = |suitable, ordered_in| window_discovery::StaleCleanupSnapshot {
        pending_refresh: false,
        suppressed: false,
        mission_control_active: false,
        drag_active: false,
        inactive_windows: Default::default(),
        server_observations: [(wsid, window_discovery::StaleWindowObservation {
            info: Some(info),
            suitable,
            ordered_in,
        })]
        .into_iter()
        .collect(),
    };

    let (ordered_stale, _) = window_discovery::identify_stale_windows(
        &reactor.state,
        wid.pid,
        &[],
        &snapshot(Some(true), Some(true)),
    );
    assert!(
        ordered_stale.is_empty(),
        "temporary AX omission must preserve an ordered-in window"
    );

    let (closed_stale, _) = window_discovery::identify_stale_windows(
        &reactor.state,
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
        &reactor.state,
        wid.pid,
        &[],
        &snapshot(Some(true), None),
    );
    assert!(
        unknown_stale.is_empty(),
        "an unavailable ordered-state query must not remove a valid layout node",
    );

    let (unknown_suitability_stale, _) = window_discovery::identify_stale_windows(
        &reactor.state,
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

    set_space_membership(&[(space, &[901, 902])]);
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
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .last_focused_window(space, workspaces[0]),
        Some(second),
        "the workspace must still remember which window was focused in it"
    );
}

/// A closed window disappears: the reactor sends the engine exactly one `ForgetWindow` for it
/// and no flight of its own, on the AX path. See "A closed window disappears" in
/// `src/animation/docs/animation-smoothness.md`.
#[test]
fn a_destroyed_window_is_forgotten_once_and_flies_nothing_of_its_own() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.config.settings.animate = true;

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    apps.requests();

    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.workspace_animation_tx = Some(animation_tx);

    let wid = WindowId::new(1, 1);
    reactor.handle_event(Event::WindowDestroyed(wid));

    let mut forgotten = 0usize;
    while let Ok((_, event)) = animation_rx.try_recv() {
        match event {
            crate::animation::platform::engine::Event::ForgetWindow(window) if window == wid => {
                forgotten += 1;
            }
            crate::animation::platform::engine::Event::Animate { windows, .. } => {
                assert!(
                    windows.iter().all(|request| request.window != wid),
                    "the closed window is not composed"
                );
            }
            _ => {}
        }
    }
    assert_eq!(forgotten, 1, "exactly one forget");
    assert!(
        reactor.state.windows.window(wid).is_none(),
        "the window state is still removed"
    );
}

/// The window server reports a close ~15ms before AX does. That path removes the window inside a
/// workflow with no reactor, so the forget rides on the outcome: still exactly one
/// `ForgetWindow`, and the late AX `WindowDestroyed` finds no window and adds nothing.
#[test]
fn a_window_server_promoted_close_is_forgotten_once() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.config.settings.animate = true;

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    apps.requests();

    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.workspace_animation_tx = Some(animation_tx);

    let wid = WindowId::new(1, 1);
    let wsid = reactor.test_window_server_id(wid);

    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, Some(false));
    reactor.handle_event(Event::WindowServerDestroyed(wsid, space, SpaceEventKind::User));
    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, None);
    assert!(
        reactor.state.windows.window(wid).is_none(),
        "the promotion removes the window"
    );

    reactor.handle_event(Event::WindowDestroyed(wid));

    let mut forgotten = 0usize;
    while let Ok((_, event)) = animation_rx.try_recv() {
        match event {
            crate::animation::platform::engine::Event::ForgetWindow(window) if window == wid => {
                forgotten += 1;
            }
            crate::animation::platform::engine::Event::Animate { windows, .. } => {
                assert!(
                    windows.iter().all(|request| request.window != wid),
                    "the closed window is not composed"
                );
            }
            _ => {}
        }
    }
    assert_eq!(
        forgotten, 1,
        "one forget, from the promotion; the AX destruction adds none"
    );
}

#[test]
fn a_windows_context_event_becomes_the_reactor_event_with_its_payload_intact() {
    use crate::windows::event::Event as W;
    let wid = WindowId::new(7, 3);
    let frame = CGRect::new(CGPoint::new(1., 2.), CGSize::new(30., 40.));
    let Event::WindowFrameChanged(got_wid, got_frame, txid, requested, mouse) =
        Event::from(W::WindowFrameChanged(wid, frame, None, Requested(true), None))
    else {
        panic!("wrong variant");
    };
    assert_eq!(
        (got_wid, got_frame, txid, requested.0, mouse),
        (wid, frame, None, true, None)
    );
    assert!(matches!(Event::from(W::MenuClosed(7)), Event::MenuClosed(7)));
}
