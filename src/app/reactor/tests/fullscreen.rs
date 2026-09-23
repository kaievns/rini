//! Full width and height, and macOS taking a window fullscreen itself.
//!
//! Two different things that used to be one. `ctrl-F` sizes a window to the usable space and leaves
//! it on the strip; a native fullscreen window leaves the strip and stops being managed.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::{WindowServerId, pid_t};
use rini_geometry::SameAs;
use test_log::test;

use super::fixtures::*;
use crate::app::config::OuterGaps;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::windows::domain::catalogue::NativeFullscreenTransition;
use crate::windows::domain::info::AppInfo;
use crate::windows::domain::request::{AppThreadHandle, Request};
use crate::workspaces::{Direction, LayoutCommand, LayoutEvent};

#[test]
fn forwarded_space_state_updates_fullscreen_spaces() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let user_space = SpaceId::new(1);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());

    reactor.handle_event(space_state_event_with(
        vec![screen],
        vec![Some(user_space)],
        |state| {
            state.fullscreen_spaces.insert(fullscreen_space);
        },
    ));

    assert!(reactor.space_state.fullscreen_spaces.contains(&fullscreen_space));
}

#[test]
fn known_fullscreen_window_appearance_removes_window_from_layout() {
    let (mut apps, mut reactor) = test_context();

    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let user_space = SpaceId::new(1);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let wid = WindowId::new(1, 1);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(user_space)]));
    make_active_app(&mut apps, &mut reactor, 1, make_windows(1), Some(wid));

    assert!(has_window_in_layout(&mut reactor, user_space, frame, wid));
    let wsid = reactor.state.windows.window(wid).unwrap().info.sys_id.unwrap();

    window_server_appeared(&mut reactor, wsid, fullscreen_space, SpaceEventKind::Fullscreen);

    assert!(
        !has_window_in_layout(&mut reactor, user_space, frame, wid),
        "managed window should be removed from layout when it enters native fullscreen"
    );
    assert!(
        reactor
            .state
            .windows
            .native_fullscreen_record_for_window_server_id(wsid)
            .is_some_and(|record| record.fullscreen_space == fullscreen_space),
        "fullscreen transition should record suspended window state"
    );
}

#[test]
fn known_window_server_appearance_restores_same_workspace_after_fullscreen() {
    let (mut apps, mut reactor) = test_context();

    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let user_space = SpaceId::new(1);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let wid = WindowId::new(1, 1);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(user_space)]));
    make_active_app(&mut apps, &mut reactor, 1, make_windows(1), Some(wid));

    let wsid = reactor.state.windows.window(wid).unwrap().info.sys_id.unwrap();
    window_server_appeared(&mut reactor, wsid, fullscreen_space, SpaceEventKind::Fullscreen);
    assert!(!has_window_in_layout(&mut reactor, user_space, frame, wid));

    window_server_appeared(&mut reactor, wsid, user_space, SpaceEventKind::User);

    assert!(
        has_window_in_layout(&mut reactor, user_space, frame, wid),
        "managed window should return to layout when native fullscreen exits back to the same space"
    );
}

#[test]
fn fullscreen_tracking_survives_until_ax_window_id_arrives() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let user_space = SpaceId::new(1);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let pid: pid_t = 61;
    let wid = WindowId::new(pid, 1);
    let wsid = WindowServerId::new((pid as u32).saturating_mul(10_000) + 1);
    let frame = CGRect::new(CGPoint::new(50., 50.), CGSize::new(900., 700.));

    reactor.handle_event(space_state_event(vec![screen], vec![Some(user_space)]));

    let (app_tx, mut app_rx) = crate::app::channels::channel();
    reactor.app_manager.apps.insert(pid, AppState {
        info: AppInfo {
            bundle_id: Some("com.test.pending-fullscreen".to_string()),
            localized_name: Some("Pending Fullscreen".to_string()),
        },
        handle: AppThreadHandle::from_sender(app_tx),
    });

    reactor.track_test_window_server_info(wsid, pid, frame);

    window_server_appeared(&mut reactor, wsid, fullscreen_space, SpaceEventKind::Fullscreen);

    assert!(
        reactor
            .state
            .windows
            .pending_native_fullscreen_record_for_window_server_id(wsid)
            .is_some_and(|record| {
                record.pid == pid
                    && record.last_known_user_space == Some(user_space)
                    && record.fullscreen_space == fullscreen_space
            }),
        "fullscreen lifecycle should be retained by wsid until AX tracking binds the window"
    );
    assert!(
        matches!(app_rx.try_recv(), Ok((_, Request::GetVisibleWindows))),
        "fullscreen appearance without AX tracking should still request a visible-window refresh"
    );

    window_server_appeared(&mut reactor, wsid, user_space, SpaceEventKind::User);

    assert!(
        matches!(app_rx.try_recv(), Ok((_, Request::GetVisibleWindows))),
        "fullscreen exit without AX tracking should request a visible-window refresh"
    );

    reactor.discover_test_windows(
        pid,
        vec![(
            wid,
            make_window_info(frame, Some(wsid), "Recovered Window", None),
        )],
        vec![wid],
    );

    assert!(
        reactor
            .state
            .windows
            .pending_native_fullscreen_record_for_window_server_id(wsid)
            .is_none(),
        "binding the AX window id should consume the pending fullscreen record"
    );
    assert!(
        reactor.state.windows.native_fullscreen_record_for_window(wid).is_none(),
        "once the window is back on its user space, the fullscreen lifecycle should retire"
    );
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(user_space)
    );
}

#[test]
fn fullscreen_does_not_suppress_other_same_pid_windows() {
    let (mut reactor, original_wid, original_wsid, user_space, _other_space, frame) =
        reactor_with_window_on_space1();
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let second_wid = WindowId::new(original_wid.pid, 1002);
    let second_wsid = WindowServerId::new(10002);

    window_server_appeared(
        &mut reactor,
        original_wsid,
        fullscreen_space,
        SpaceEventKind::Fullscreen,
    );

    reactor.handle_event(Event::WindowCreated(
        second_wid,
        make_window_info(frame, Some(second_wsid), "Second Window", None),
        Some(crate::windows::domain::info::WindowServerInfo {
            id: second_wsid,
            pid: original_wid.pid,
            layer: 0,
            frame,
            min_frame: frame.size,
            max_frame: frame.size,
        }),
        None,
    ));

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(second_wid),
        Some(user_space)
    );
}

#[test]
fn fullscreen_exit_removes_non_queryable_duplicate_from_layout() {
    let (mut reactor, original_wid, original_wsid, user_space, other_space, frame) =
        reactor_with_window_on_space1();
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let duplicate_wid = WindowId::new(original_wid.pid, 27481);
    let duplicate_wsid = WindowServerId::new(27481);
    let active_workspace = reactor
        .layout_manager
        .layout_engine
        .active_workspace(user_space)
        .expect("active workspace");

    window_server_appeared(
        &mut reactor,
        original_wsid,
        fullscreen_space,
        SpaceEventKind::Fullscreen,
    );

    reactor.add_test_window_with_manageability(
        duplicate_wid,
        duplicate_wsid,
        Some(fullscreen_space),
        frame,
        false,
    );

    window_server_appeared(
        &mut reactor,
        duplicate_wsid,
        fullscreen_space,
        SpaceEventKind::Fullscreen,
    );

    assert!(reactor.assign_test_window_to_workspace(user_space, duplicate_wid, active_workspace));
    reactor.send_layout_event(LayoutEvent::WindowAdded(user_space, duplicate_wid));
    assert!(has_window_in_layout(
        &mut reactor,
        user_space,
        frame,
        duplicate_wid
    ));
    assert!(
        reactor.view().create_window_data(duplicate_wid).is_none(),
        "duplicate is absent from query windows because it is not manageable"
    );

    reactor.mark_test_window_visible_in_space(duplicate_wsid, user_space);
    window_server_appeared(&mut reactor, duplicate_wsid, user_space, SpaceEventKind::User);

    assert!(
        !has_window_in_layout(&mut reactor, user_space, frame, duplicate_wid),
        "fullscreen restore must evict non-queryable duplicate layout ghosts"
    );
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(duplicate_wid),
        None
    );

    reactor.handle_event(space_state_event(vec![frame], vec![Some(other_space)]));
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(duplicate_wid),
        None
    );
    reactor.handle_event(space_state_event(vec![frame], vec![Some(user_space)]));
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(duplicate_wid),
        None
    );
    assert!(
        !has_window_in_layout(&mut reactor, user_space, frame, duplicate_wid),
        "ghost must not reappear when switching back to the original space"
    );
}

#[test]
fn fullscreen_restore_uses_live_rekeyed_window_id() {
    let (mut reactor, old_wid, wsid, user_space, _other_space, frame) =
        reactor_with_window_on_space1();
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let new_wid = WindowId::new(old_wid.pid, 1999);

    window_server_appeared(&mut reactor, wsid, fullscreen_space, SpaceEventKind::Fullscreen);

    rekey_window(&mut reactor, old_wid, new_wid);

    assert!(
        reactor.state.windows.window(old_wid).is_none(),
        "rekey should retire the old AX window id before fullscreen restore"
    );

    window_server_appeared(&mut reactor, wsid, user_space, SpaceEventKind::User);

    assert!(has_window_in_layout(&mut reactor, user_space, frame, new_wid));
    assert!(!has_window_in_layout(&mut reactor, user_space, frame, old_wid));
}

#[test]
fn forwarded_space_state_does_not_clear_existing_fullscreen_tracks_when_snapshot_has_none() {
    let mut reactor = test_reactor();
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let tracked_user_space = SpaceId::new(1);
    let current_space = SpaceId::new(2);
    let fullscreen_space = SpaceId::new(0x400000001);
    let window_id = WindowId::new(42, 1);

    let tracked_workspace = reactor.test_workspace(tracked_user_space, 0);
    assert!(reactor.assign_test_window_to_workspace(
        tracked_user_space,
        window_id,
        tracked_workspace
    ));
    let _ = reactor.state.windows.suspend_window_to_native_fullscreen(
        window_id,
        Some(WindowServerId::new(1)),
        Some(tracked_user_space),
        fullscreen_space,
        NativeFullscreenTransition::Suspended,
    );

    reactor.handle_event(space_state_event_with(
        vec![frame],
        vec![Some(current_space)],
        |state| state.has_seen_display_set = true,
    ));

    assert!(
        reactor
            .state
            .windows
            .native_fullscreen_record_for_window(window_id)
            .is_some_and(|record| record.fullscreen_space == fullscreen_space),
        "empty forwarded fullscreen state must not clear existing fullscreen exit tracking"
    );
}

#[test]
fn fullscreen_space_in_screen_params_does_not_trigger_topology_relayout() {
    let mut reactor = test_reactor();

    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1280., 800.));
    let user_space = SpaceId::new(11);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let display_uuid = "11111111-1111-1111-1111-111111111111".to_string();
    let screens_for = |space: SpaceId| -> Vec<ScreenInfo> {
        vec![ScreenInfo {
            id: rini_core::ids::ScreenId::new(0),
            frame,
            space: Some(space),
            display_uuid: display_uuid.clone(),
            name: None,
        }]
    };

    reactor.handle_event(space_state_event_from_screens(screens_for(user_space)));
    assert_eq!(
        reactor.state.display_memory.affinity.space_for_display(&display_uuid),
        Some(user_space)
    );

    reactor.space_state.fullscreen_spaces.insert(fullscreen_space);
    reactor.handle_event(space_state_event_from_screens(
        screens_for(user_space)
            .into_iter()
            .map(|mut screen| {
                screen.space = None;
                screen
            })
            .collect(),
    ));
    assert_eq!(
        reactor.state.display_memory.affinity.space_for_display(&display_uuid),
        Some(user_space),
        "fullscreen spaces should not replace display->user-space history"
    );

    reactor.handle_event(space_state_event_from_screens(screens_for(user_space)));
    assert_eq!(
        reactor.state.display_memory.affinity.space_for_display(&display_uuid),
        Some(user_space)
    );
}

#[test]
fn fullscreen_transition_preserves_other_display_space() {
    let mut reactor = test_reactor();

    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let left_space_2 = SpaceId::new(12);
    let right_space_1 = SpaceId::new(21);
    let right_fullscreen = SpaceId::new(0x400000000 + right_space_1.get());

    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(left_space_2),
        Some(right_space_1),
    ]));
    reactor.space_state.fullscreen_spaces.insert(right_fullscreen);

    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(left_space_2),
        None,
    ]));

    assert_eq!(
        reactor.raw_spaces_for_current_screens(),
        vec![Some(left_space_2), None],
        "fullscreen transitions on one display must not accept a transient user-space change on another display"
    );
}

#[test]
fn user_space_switch_is_allowed_while_other_display_already_fullscreen() {
    let mut reactor = test_reactor();

    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let left_space_2 = SpaceId::new(12);
    let left_space_1 = SpaceId::new(11);
    let right_space_1 = SpaceId::new(21);
    let right_fullscreen = SpaceId::new(0x400000000 + right_space_1.get());

    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(left_space_2),
        Some(right_space_1),
    ]));
    reactor.space_state.fullscreen_spaces.insert(right_fullscreen);
    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(left_space_2),
        None,
    ]));

    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(left_space_1),
        None,
    ]));

    assert_eq!(
        reactor.raw_spaces_for_current_screens(),
        vec![Some(left_space_1), None],
        "Once another display is already fullscreen, user space switches on this display should still be accepted"
    );
}

#[test]
fn fullscreen_screen_params_preserves_window_layout() {
    // Regression test for #308: waking from sleep while a fullscreen video is
    // active should not wipe workspace assignments.
    let (mut apps, mut reactor) = test_context();

    let user_space = SpaceId::new(1);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));

    // Set up a display with a user space and some windows.
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(user_space)]));
    make_active_app_with_count(&mut apps, &mut reactor, 1, 3, Some(WindowId::new(1, 1)));

    // Rearrange layout so we can detect if it gets reset.
    reactor.handle_test_layout_command(LayoutCommand::MoveNode(Direction::Up));
    apps.simulate_until_quiet(&mut reactor);
    let layout_before = test_layout(&mut reactor, user_space, full_screen);

    // Simulate sleep/wake while fullscreen: ScreenParametersChanged arrives
    // with the fullscreen space id.
    reactor.space_state.fullscreen_spaces.insert(fullscreen_space);
    reactor.handle_event(space_state_event_from_screens(vec![ScreenInfo {
        id: rini_core::ids::ScreenId::new(0),
        frame: full_screen,
        space: None,
        display_uuid: "test-display-0".to_string(),
        name: None,
    }]));
    apps.simulate_until_quiet(&mut reactor);

    // The fullscreen space must not become the active space for the screen.
    assert_eq!(
        reactor.space_state.screens[0].space, None,
        "fullscreen space should be nulled out, not stored as screen space"
    );

    // Return to user space (simulates exiting fullscreen).
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(user_space)]));
    apps.simulate_until_quiet(&mut reactor);

    let layout_after = test_layout(&mut reactor, user_space, full_screen);
    assert_eq!(
        layout_before, layout_after,
        "Window layout on user space must be preserved across fullscreen ScreenParametersChanged"
    );
}

#[test]
fn fullscreen_startup_applies_app_rules_to_hidden_user_space_windows() {
    let (reactor, wid, user_space, _default_workspace, target_workspace) =
        fullscreen_startup_fixture(true, false);

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(user_space)
    );
    assert_eq!(
        reactor.test_workspace_for_window(user_space, wid),
        Some(target_workspace),
        "fullscreen startup should still apply app rules to the hidden user-space window"
    );
}

#[test]
fn fullscreen_startup_discovery_preserves_existing_hidden_assignment_without_app_rules() {
    let (reactor, wid, user_space, default_workspace, secondary_workspace) =
        fullscreen_startup_fixture(false, true);

    assert_ne!(secondary_workspace, default_workspace);
    assert_eq!(
        reactor.test_workspace_for_window(user_space, wid),
        Some(secondary_workspace),
        "fullscreen startup discovery must preserve the existing hidden assignment instead of defaulting it"
    );
}

#[test]
fn unfullscreen_restores_window_tracking() {
    let (mut apps, mut reactor) = test_context();

    let user_space = SpaceId::new(1);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));

    // Set up a display with a user space and some windows.
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(user_space)]));
    make_active_app_with_count(&mut apps, &mut reactor, 1, 1, Some(WindowId::new(1, 1)));

    // Record the window as fullscreened.
    let window_id = WindowId::new(1, 1);
    let _ = reactor.state.windows.suspend_window_to_native_fullscreen(
        window_id,
        Some(WindowServerId::new(1)),
        Some(user_space),
        fullscreen_space,
        NativeFullscreenTransition::Suspended,
    );

    // Transition to fullscreen space.
    reactor.handle_event(space_state_event(vec![full_screen], vec![None]));
    apps.simulate_until_quiet(&mut reactor);

    // Exit fullscreen (return to user space).
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(user_space)]));

    // The reactor should trigger a GetVisibleWindows request.
    let mut saw_get_visible_windows = false;
    for request in apps.requests() {
        if matches!(request, Request::GetVisibleWindows) {
            saw_get_visible_windows = true;
        }
    }
    assert!(
        saw_get_visible_windows,
        "Should send GetVisibleWindows to app on unfullscreen"
    );

    // The fullscreen track should be removed.
    assert!(
        reactor.state.windows.native_fullscreen_record_for_window(window_id).is_none(),
        "Fullscreen track should be removed from space manager"
    );
}

#[test]
fn fullscreen_exit_space_restore_does_not_revive_stale_pre_rekey_window() {
    let (mut reactor, old_wid, wsid, user_space, _other_space, full_screen) =
        reactor_with_window_on_space1();
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let new_wid = WindowId::new(old_wid.pid, 99);

    reactor.send_layout_event(LayoutEvent::WindowAdded(user_space, old_wid));
    assert!(has_window_in_layout(
        &mut reactor,
        user_space,
        full_screen,
        old_wid
    ));

    reactor.space_state.fullscreen_spaces.insert(fullscreen_space);
    let _ = reactor.state.windows.suspend_window_to_native_fullscreen(
        old_wid,
        Some(wsid),
        Some(user_space),
        fullscreen_space,
        NativeFullscreenTransition::Suspended,
    );
    reactor.send_layout_event(LayoutEvent::WindowRemovedPreserveFloating(old_wid));

    rekey_window(&mut reactor, old_wid, new_wid);
    assert!(
        reactor.state.windows.window(old_wid).is_none(),
        "rekey should retire the old AX id before the fullscreen exit snapshot arrives"
    );

    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(user_space)]));

    assert!(
        !has_window_in_layout(&mut reactor, user_space, full_screen, old_wid),
        "fullscreen exit must not recreate a stale layout-only ghost for the old AX window id"
    );
}

#[test]
fn floating_window_toggles_to_fullscreen_within_gaps() {
    let (mut reactor, wid, space1, screen, _floating_frame) = reactor_with_floating_window();
    // Assymetric gaps to prevent swapped left/right or swapped width/height bugs from passing
    reactor.config.settings.layout.gaps.outer = OuterGaps {
        top: 10.,
        left: 20.,
        bottom: 30.,
        right: 40.,
    };
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreenWithinGaps);
    let expected = CGRect::new(
        CGPoint::new(screen.origin.x + 20., screen.origin.y + 10.),
        CGSize::new(screen.size.width - 20. - 40., screen.size.height - 10. - 30.),
    );
    let laid_out = laid_out_frame(&mut reactor, space1, screen, wid).expect("window laid out");
    assert!(
        laid_out.same_as(expected),
        "expected {expected:?}, got {laid_out:?}"
    );
}
