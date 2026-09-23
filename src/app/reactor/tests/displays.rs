//! What the reactor does when screens change: arrivals, departures, resolution changes,
//! per-display workspaces, and which display a window calls home.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::{WindowServerId, pid_t};
use rini_geometry::CGRectExt;
use test_log::test;

use super::fixtures::*;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::windows::domain::request::Request;
use crate::workspaces::{Direction, LayoutCommand, LayoutEvent};

#[test]
fn it_clears_screen_state_when_no_displays_are_reported() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));

    reactor.handle_event(space_state_event(vec![screen], vec![Some(SpaceId::new(1))]));
    assert_eq!(1, reactor.space_state.screens.len());

    reactor.handle_event(space_state_event(vec![], vec![]));
    assert!(reactor.space_state.screens.is_empty());
    assert_eq!(reactor.raw_command_space(), None);
    assert_eq!(reactor.space_state.menu_bar_space, None);
    assert!(reactor.space_state.display_space_ids.is_empty());

    reactor.handle_event(space_state_event(vec![], vec![]));
    assert!(reactor.space_state.screens.is_empty());
    assert_eq!(reactor.raw_command_space(), None);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(SpaceId::new(1))]));
    assert_eq!(1, reactor.space_state.screens.len());
}

#[test]
fn layout_commands_follow_active_display_space_across_active_displays() {
    let mut reactor = test_reactor();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let right = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let left_space = SpaceId::new(1);
    let right_space = SpaceId::new(2);
    let source = WindowId::new(1, 1);
    let target_a = WindowId::new(1, 2);
    let target_b = WindowId::new(1, 3);
    let windows = [
        (source, WindowServerId::new(101), left_space, left),
        (target_a, WindowServerId::new(102), right_space, right),
        (target_b, WindowServerId::new(103), right_space, right),
    ];

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(left_space), Some(right_space)],
    ));

    reactor.add_test_app(1);

    reactor.send_layout_event(LayoutEvent::SpaceExposed(left_space, left.size));
    reactor.send_layout_event(LayoutEvent::SpaceExposed(right_space, right.size));

    let left_workspace = reactor.test_workspace(left_space, 0);
    let right_workspace = reactor.test_workspace(right_space, 0);

    for (wid, wsid, space, frame) in windows {
        reactor.add_test_window(wid, wsid, Some(space), frame);
        let workspace = if space == left_space {
            left_workspace
        } else {
            right_workspace
        };
        assert!(reactor.assign_test_window_to_workspace(space, wid, workspace));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, wid));
    }

    reactor.send_layout_event(LayoutEvent::WindowFocused(right_space, target_a));

    assert_eq!(reactor.workspace_command_space(), Some(left_space));
    assert_eq!(reactor.command_context_space(), Some(left_space));
    assert_eq!(
        reactor.layout_manager.layout_engine.focused_window(),
        Some(target_a)
    );

    reactor.handle_test_layout_command(LayoutCommand::NextWindow);

    assert_eq!(
        reactor.layout_manager.layout_engine.focused_window(),
        Some(source),
        "non-workspace layout commands should follow the active display space"
    );
}

#[test]
fn active_display_update_only_changes_command_context() {
    let (mut apps, mut reactor) = test_context();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let left_space = SpaceId::new(1);
    let right_space = SpaceId::new(2);

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(left_space), Some(right_space)],
    ));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(1));
    assert!(apps.requests().is_empty());

    reactor.handle_event(Event::ActiveDisplayChanged {
        menu_bar_space: Some(right_space),
        command_space: Some(right_space),
    });

    assert_eq!(reactor.workspace_command_space(), Some(right_space));
    assert_eq!(reactor.space_state.menu_bar_space, Some(right_space));
    assert!(
        apps.requests().is_empty(),
        "active-display updates must not trigger window discovery"
    );
}

/// A tiled scrolling window that appears on another display's coordinates keeps its own
/// space.
///
/// This test used to assert the opposite, and passed only because the default layout mode
/// was `traditional`. In a scrolling strip, columns scrolled off the edge are deliberately
/// parked outside the display — on a multi-display desktop those coordinates land inside the
/// neighbouring monitor — so inferring ownership from position would hand every parked
/// column to the wrong display. `keep_assigned_for_scrolling` exists precisely to prevent
/// that, and with the tree layouts removed it is now always in force for tiled windows.
#[test]
fn a_tiled_scrolling_window_keeps_its_space_when_its_frame_lands_on_another_display() {
    let (mut reactor, wid, _wsid, space1, _space2, frame, screen2) =
        reactor_with_window_on_space1_two_displays();
    let moved = CGRect::new(
        CGPoint::new(screen2.origin.x + 100.0, frame.origin.y),
        frame.size,
    );

    let _ = reactor
        .dispatch_workflow(Event::WindowFrameChanged(
            wid,
            moved,
            None,
            Requested(false),
            Some(MouseState::Up),
        ))
        .unwrap();

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space1),
        "a parked column's frame is not evidence that it changed display"
    );
}

/// A window whose model frame sits wholly off screen maps to no space, so its next real frame
/// reads as a space change and the frame-changed path adds it to the active workspace. Zoom's
/// 301x45 meeting toolbar (`is_standard: false`) was tiled that way as a column of the workspace
/// just switched to, and the strip scrolled to show it. Only a manageable window may join.
#[test]
fn an_unmanageable_window_is_not_tiled_when_its_frame_comes_back_on_screen() {
    let mut reactor = test_reactor();
    let pid = 1;
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space1 = SpaceId::new(1);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space1)]));
    reactor.add_test_app(pid);
    let _ = reactor.test_workspace(space1, 0);

    let off_screen = CGRect::new(CGPoint::new(2255., 1126.), CGSize::new(301., 45.));
    let on_screen = CGRect::new(CGPoint::new(1100., 32.), CGSize::new(301., 45.));
    let toolbar = WindowId::new(pid, 1);
    reactor.add_test_window_with_manageability(
        toolbar,
        WindowServerId::new(101),
        Some(space1),
        off_screen,
        false,
    );
    let standard = WindowId::new(pid, 2);
    reactor.add_test_window(standard, WindowServerId::new(102), Some(space1), off_screen);

    for wid in [toolbar, standard] {
        reactor.handle_event(Event::WindowFrameChanged(
            wid,
            on_screen,
            None,
            Requested(false),
            Some(MouseState::Up),
        ));
    }

    assert!(
        !has_window_in_layout(&mut reactor, space1, screen, toolbar),
        "an unmanageable window must not be tiled by a frame change"
    );
    assert!(
        has_window_in_layout(&mut reactor, space1, screen, standard),
        "a manageable window coming on screen still joins the layout"
    );
}

#[test]
fn cross_display_drag_clears_source_floating_position() {
    let (mut reactor, wid, _wsid, space1, space2, initial_frame, screen2) =
        reactor_with_window_on_space1_two_displays();
    let source_workspace = reactor
        .layout_manager
        .layout_engine
        .active_workspace(space1)
        .expect("source workspace");
    let target_workspace = reactor
        .layout_manager
        .layout_engine
        .active_workspace(space2)
        .expect("target workspace");

    reactor.send_layout_event(LayoutEvent::WindowAdded(space1, wid));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space1, wid));
    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    assert!(reactor.layout_manager.layout_engine.is_window_floating(wid));
    reactor.layout_manager.layout_engine.store_floating_position(
        space1,
        source_workspace,
        wid,
        initial_frame,
    );

    let moved_frame = CGRect::new(
        CGPoint::new(screen2.origin.x + 120.0, initial_frame.origin.y),
        initial_frame.size,
    );
    reactor.drag_manager.drag_state = DragState::Active {
        session: DragSession {
            window: wid,
            last_frame: moved_frame,
            origin_space: None,
            settled_space: Some(space2),
            layout_dirty: true,
        },
    };

    let (visible_spaces, visible_space_centers) = reactor.visible_spaces_for_layout(true);
    let outcome = crate::app::reactor::events::drag::handle_mouse_up(
        &mut reactor.state,
        &mut reactor.layout_manager,
        &mut reactor.drag_manager,
        crate::app::reactor::events::drag::MouseUpPayload {
            pending_swap: None,
            swap_space: Some(space2),
            final_space: Some(space2),
            visible_spaces,
            visible_space_centers,
        },
    )
    .unwrap();
    assert!(outcome.arrange.requested);
    assert!(matches!(reactor.drag_manager.drag_state, DragState::Inactive));

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2)
    );
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .get_floating_position(space1, source_workspace, wid),
        None,
        "cross-display drags must clear the source workspace's floating position"
    );
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .get_floating_position(space2, target_workspace, wid),
        Some(moved_frame)
    );
}

#[test]
fn stale_user_space_disappearance_does_not_restore_old_display_assignment() {
    let (mut reactor, wid, wsid, space1, space2, _) = reactor_with_window_moved_to_space2();

    window_server_destroyed(&mut reactor, wsid, space1, SpaceEventKind::User);

    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2)
    );
    assert!(reactor.state.windows.is_window_visible(wsid));

    let _ = reactor.reconcile_windows_with_authoritative_spaces();

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2),
        "late disappearance from the old display must not drag a moved window back"
    );
}

#[test]
fn stale_user_space_appearance_does_not_restore_old_display_assignment() {
    let (mut reactor, wid, wsid, space1, space2, _) = reactor_with_window_moved_to_space2();

    window_server_appeared(&mut reactor, wsid, space1, SpaceEventKind::User);

    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2)
    );

    let _ = reactor.reconcile_windows_with_authoritative_spaces();

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2),
        "late appearance on the old display must not overwrite the newer target assignment"
    );
}

#[test]
fn multi_active_visible_window_appearance_keeps_display_assignment_and_visibility() {
    let (mut reactor, wid, wsid, space1, space2, _frame) = reactor_with_window_moved_to_space2();

    window_server_appeared(&mut reactor, wsid, space1, SpaceEventKind::User);

    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2)
    );
    assert_eq!(
        reactor.affinity().authoritative_space_for_window_id(wid),
        Some(space2)
    );
    assert!(reactor.state.windows.is_window_visible(wsid));
}

#[test]
fn multi_active_visible_window_disappearance_does_not_reassign_between_display_spaces() {
    let (mut reactor, wid, wsid, space1, space2, _frame) = reactor_with_window_moved_to_space2();

    window_server_destroyed(&mut reactor, wsid, space1, SpaceEventKind::User);

    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2)
    );
    assert!(reactor.state.windows.is_window_visible(wsid));
}

#[test]
fn hidden_window_can_move_to_another_native_space_without_staying_pinned_to_old_display() {
    let mut reactor = test_reactor_with_workspace_count(2);
    let pid = 1;
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let right = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let frame = CGRect::new(CGPoint::new(100., 100.), CGSize::new(800., 600.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    let wid = WindowId::new(pid, 1);
    let wsid = WindowServerId::new(121);

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(space1), Some(space2)],
    ));

    reactor.add_test_app(pid);

    let workspaces = reactor.test_workspace_ids(space1);
    let hidden_workspace = workspaces[0];
    let visible_workspace = workspaces[1];
    let _ = reactor.test_workspace_ids(space2);

    reactor.add_test_window(wid, wsid, Some(space1), frame);

    assert!(reactor.set_test_active_workspace(space1, visible_workspace));
    assert!(reactor.assign_test_window_to_workspace(space1, wid, hidden_workspace));
    assert_eq!(
        reactor.affinity().hidden_assigned_space_for_window_id(wid),
        Some(space1)
    );

    crate::windows::platform::window_server::set_window_spaces_override(
        wsid,
        Some(vec![space2.get()]),
    );
    window_server_appeared(&mut reactor, wsid, space2, SpaceEventKind::User);
    crate::windows::platform::window_server::set_window_spaces_override(wsid, None);

    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2)
    );
    assert_eq!(
        reactor.affinity().authoritative_space_for_window_id(wid),
        Some(space2)
    );
}

#[test]
fn recent_cross_display_move_ignores_conflicting_geometry_space_change() {
    let (mut reactor, wid, wsid, _space1, space2, _) = reactor_with_window_moved_to_space2();
    let conflicting_frame = CGRect::new(CGPoint::new(100., 100.), CGSize::new(800., 600.));

    reactor.handle_event(Event::WindowFrameChanged(
        wid,
        conflicting_frame,
        None,
        Requested(false),
        Some(MouseState::Up),
    ));

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2)
    );
    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
}

#[test]
fn discovery_preserves_hidden_windows_on_their_original_same_display_space() {
    let mut reactor = test_reactor();
    let pid = 1;
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(space1)]));
    reactor.add_test_app(pid);

    let space1_workspace = reactor.test_workspace(space1, 0);
    let space2_workspace = reactor.test_workspace(space2, 0);

    let windows = [
        (WindowId::new(pid, 1), WindowServerId::new(101), space1),
        (WindowId::new(pid, 2), WindowServerId::new(102), space1),
        (WindowId::new(pid, 3), WindowServerId::new(103), space2),
    ];

    for (wid, wsid, space) in windows {
        reactor.insert_test_window(wid, wsid, Some(space), frame, true);
    }

    assert!(reactor.assign_test_window_to_workspace(
        space1,
        WindowId::new(pid, 1),
        space1_workspace
    ));
    assert!(reactor.assign_test_window_to_workspace(
        space1,
        WindowId::new(pid, 2),
        space1_workspace
    ));
    assert!(reactor.assign_test_window_to_workspace(
        space2,
        WindowId::new(pid, 3),
        space2_workspace
    ));

    reactor.handle_event(space_state_event(vec![frame], vec![Some(space2)]));
    reactor.state.windows.clear_visible_windows();
    reactor.state.windows.mark_window_visible(WindowServerId::new(103));
    reactor.mission_control_manager.pending_mission_control_refresh.insert(pid);

    reactor.on_windows_discovered_with_app_info(pid, vec![], vec![WindowId::new(pid, 3)], None);

    let space1_workspaces = reactor.query_workspaces(Some(space1));
    let space2_workspaces = reactor.query_workspaces(Some(space2));
    let space1_count: usize = space1_workspaces.iter().map(|ws| ws.window_count).sum();
    let space2_count: usize = space2_workspaces.iter().map(|ws| ws.window_count).sum();

    assert_eq!(
        space1_count, 2,
        "inactive native space windows must stay on space1"
    );
    assert_eq!(
        space2_count, 1,
        "only the visible window should belong to space2"
    );
    assert!(reactor.test_workspace_for_window(space1, WindowId::new(pid, 1)).is_some());
    assert!(reactor.test_workspace_for_window(space1, WindowId::new(pid, 2)).is_some());
    assert!(reactor.test_workspace_for_window(space2, WindowId::new(pid, 1)).is_none());
    assert!(reactor.test_workspace_for_window(space2, WindowId::new(pid, 2)).is_none());
}

#[test]
fn forwarded_space_state_is_queued_during_mission_control_and_applied_on_exit() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let old_space = SpaceId::new(1);
    let new_space = SpaceId::new(2);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(old_space)]));
    reactor.handle_event(Event::MissionControlNativeEntered);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(new_space)]));

    assert_eq!(
        reactor
            .pending_space_change_manager
            .pending_space_change
            .as_ref()
            .map(|pending| pending.screens.iter().map(|screen| screen.space).collect::<Vec<_>>()),
        Some(vec![Some(new_space)])
    );

    reactor.handle_event(Event::MissionControlNativeExited);

    assert_eq!(reactor.workspace_command_space(), Some(new_space));
    assert!(reactor.pending_space_change_manager.pending_space_change.is_none());
}

#[test]
fn mission_control_exit_does_not_restore_cached_space_without_authoritative_snapshot() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let stale_space = SpaceId::new(1);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(stale_space)]));
    reactor.handle_event(Event::MissionControlNativeEntered);
    reactor.handle_event(space_state_event(vec![screen], vec![None]));
    reactor.handle_event(Event::MissionControlNativeExited);

    assert_eq!(reactor.workspace_command_space(), None);
    assert_eq!(reactor.space_state.screens[0].space, None);
}

#[test]
fn mission_control_exit_refresh_drops_windows_missing_from_origin_space_snapshot() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let pid: pid_t = 42;
    let moved = WindowId::new(pid, 1);
    let retained = WindowId::new(pid, 2);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, pid, make_windows(2));

    assert!(has_window_in_layout(&mut reactor, space, screen, moved));
    assert!(has_window_in_layout(&mut reactor, space, screen, retained));

    apps.windows.remove(&moved);
    let retained_wsid = WindowServerId::new((pid as u32).saturating_mul(10_000) + 2);
    reactor.refresh_windows_after_mission_control_with_active_windows(vec![(
        retained_wsid,
        Some(space),
    )]);
    apps.simulate_until_quiet(&mut reactor);

    assert!(
        !has_window_in_layout(&mut reactor, space, screen, moved),
        "window moved to another native space during Mission Control should be removed from the origin layout immediately"
    );
    assert!(has_window_in_layout(&mut reactor, space, screen, retained));
}

#[test]
fn mission_control_refresh_known_visible_fallback_does_not_restore_window_moved_to_other_space() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let pid: pid_t = 45;
    let moved = WindowId::new(pid, 1);
    let retained = WindowId::new(pid, 2);
    let retained_wsid = WindowServerId::new((pid as u32).saturating_mul(10_000) + 2);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, pid, make_windows(2));

    reactor.handle_test_workspace_command(space, &LayoutCommand::CreateWorkspace);

    reactor.refresh_windows_after_mission_control_with_active_windows(vec![(
        retained_wsid,
        Some(space),
    )]);
    apps.simulate_until_quiet(&mut reactor);

    assert!(
        !has_window_in_layout(&mut reactor, space, screen, moved),
        "known_visible fallback must not recreate a layout ghost for a window missing from the authoritative active-space snapshot"
    );

    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(1));
    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(0));

    assert!(
        !has_window_in_layout(&mut reactor, space, screen, moved),
        "workspace switching must not re-project a window that Mission Control moved to another native space"
    );
    assert!(has_window_in_layout(&mut reactor, space, screen, retained));
}

#[test]
fn mission_control_enter_clears_active_drag_state() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);
    let frame = CGRect::new(CGPoint::new(50., 50.), CGSize::new(100., 100.));

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.insert_test_window_state(wid, frame, Some(WindowServerId::new(1)), true);
    reactor.ensure_active_drag(wid, &frame);

    assert!(matches!(
        reactor.drag_manager.drag_state,
        DragState::Active { .. }
    ));

    reactor.handle_event(Event::MissionControlNativeEntered);

    assert!(matches!(reactor.drag_manager.drag_state, DragState::Inactive));
    assert!(reactor.drag_manager.skip_layout_for_window.is_none());
}

#[test]
fn it_keeps_discovered_windows_on_their_initial_screen() {
    let (mut apps, mut reactor) = test_context();
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(
        vec![screen1, screen2],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
    ));

    let mut windows = make_windows(2);
    windows[1].frame.origin = CGPoint::new(1100., 100.);
    reactor.handle_events(apps.make_app(1, windows));

    let _events = apps.simulate_events();
    // Asserts WHICH SCREEN each window landed on, not how wide it ended up. A lone column no
    // longer stretches to fill its viewport, so comparing against the full screen rect would
    // be testing the column-width rule rather than the screen-affinity behaviour named here.
    let frame1 = apps.windows.get(&WindowId::new(1, 1)).expect("Window was not resized").frame;
    let frame2 = apps.windows.get(&WindowId::new(1, 2)).expect("Window was not resized").frame;
    assert!(
        screen1.contains(frame1.mid()),
        "window 1 must stay on screen 1: {frame1:?}"
    );
    assert!(
        screen2.contains(frame2.mid()),
        "window 2 must stay on screen 2: {frame2:?}"
    );
}

/// Two columns per screen on purpose. A third would overflow the strip, and a column with nothing on its
/// own display is parked in a corner and dropped from the raise list — which would take one of the groups
/// this is here to check with it. App 2 straddles both screens, which is the case that matters: one app,
/// two groups.
#[test]
fn handle_layout_response_groups_windows_by_app_and_screen() {
    let (mut apps, mut reactor) = test_context();
    let (raise_manager_tx, mut raise_manager_rx) = channels::channel();
    reactor.communication_manager.raise_manager_tx = raise_manager_tx;
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(
        vec![screen1, screen2],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
    ));

    reactor.handle_events(apps.make_app(1, make_windows(1)));

    let mut windows = make_windows(2);
    windows[1].frame.origin = CGPoint::new(1100., 100.);
    reactor.handle_events(apps.make_app(2, windows));

    let _events = apps.simulate_events();
    while raise_manager_rx.try_recv().is_ok() {}

    reactor.handle_layout_response(
        layout::EventResponse {
            changed: true,
            raise_windows: vec![
                WindowId::new(1, 1),
                WindowId::new(2, 1),
                WindowId::new(2, 2),
            ],
            focus_window: None,
            boundary_hit: None,
            edge_hit: None,
        },
        None,
    );
    let msg = raise_manager_rx.try_recv().expect("Should have sent an event").1;
    match msg {
        raise_manager::Event::RaiseRequest(RaiseRequest {
            raise_windows, focus_window, ..
        }) => {
            let raise_windows: HashSet<Vec<WindowId>> = raise_windows.into_iter().collect();
            let expected = [
                vec![WindowId::new(1, 1)],
                vec![WindowId::new(2, 1)],
                vec![WindowId::new(2, 2)],
            ]
            .into_iter()
            .collect();
            assert_eq!(raise_windows, expected);
            assert!(focus_window.is_none());
        }
        _ => panic!("Unexpected event: {msg:?}"),
    }
}

#[test]
fn it_preserves_layout_after_login_screen() {
    // TODO: This would be better tested with a more complete simulation.
    let (mut apps, mut reactor) = test_context();
    let space = SpaceId::new(1);
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(space)]));

    make_active_app_with_count(&mut apps, &mut reactor, 1, 3, Some(WindowId::new(1, 1)));
    let default = test_layout(&mut reactor, space, full_screen);

    assert!(reactor.layout_manager.layout_engine.selected_window(space).is_some());
    // Was MoveNode(Up), a tree operation. In a scrolling strip of single-window columns
    // there is nothing above a window, so that command is a no-op and the layout never
    // changed — the assert_ne below then failed for the wrong reason. Moving a column
    // sideways is the equivalent rearrangement here.
    reactor.handle_test_layout_command(LayoutCommand::MoveNode(Direction::Right));
    apps.simulate_until_quiet(&mut reactor);
    let modified = test_layout(&mut reactor, space, full_screen);
    assert_ne!(default, modified);

    reactor.handle_event(space_state_event(vec![CGRect::ZERO], vec![None]));
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(space)]));
    simulate_login_screen_refresh(&mut apps, &mut reactor, 1);

    assert_eq!(test_layout(&mut reactor, space, full_screen), modified);
}

#[test]
fn display_index_selector_uses_physical_left_to_right_order() {
    let mut reactor = test_reactor();
    let right = CGRect::new(CGPoint::new(200000., 0.), CGSize::new(1000., 1000.));
    let left = CGRect::new(CGPoint::new(100000., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(
        vec![right, left],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
    ));

    let selected = reactor
        .screen_for_selector(&DisplaySelector::Index(0), None)
        .expect("expected display index 0 to resolve");

    assert_eq!(selected.frame, left);
}

#[test]
fn moving_tiled_window_to_display_applies_destination_layout_after_transfer_frame() {
    let (mut apps, mut reactor) = test_context();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
    ));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));

    let moved = WindowId::new(1, 1);
    reactor.handle_event(Event::Command(Command::Reactor(
        ReactorCommand::MoveWindowToDisplay {
            selector: DisplaySelector::Index(1),
            window_id: Some(1),
        },
    )));

    let writes: Vec<CGRect> = apps
        .requests()
        .into_iter()
        .flat_map(|request| match request {
            Request::SetWindowFrame(wid, frame, _, _) if wid == moved => vec![frame],
            Request::SetBatchWindowFrame(frames, _, _) => frames
                .into_iter()
                .filter_map(|(wid, frame)| (wid == moved).then_some(frame))
                .collect(),
            _ => Vec::new(),
        })
        .collect();

    assert!(
        writes.len() >= 2,
        "expected transfer and tiled writes: {writes:?}"
    );
    // The subject here is ORDERING: the destination's own layout pass must have the last word,
    // after the transfer frame. It used to be asserted as "the final frame equals the whole
    // right-hand screen", which only held because a lone column filled its viewport. That rule
    // is gone, so the check is now that the final frame is tiled ON the right-hand display.
    let last = writes.last().copied().expect("at least one write");
    assert!(
        right.contains(last.mid()),
        "the destination layout must supply the final frame, on the destination display: \
         {writes:?}"
    );
    assert!(
        !left.contains(last.mid()),
        "the final frame must not still be on the source display: {writes:?}"
    );
}

#[test]
fn authoritative_active_window_snapshot_reassigns_window_across_active_displays() {
    let (mut reactor, wid, wsid, space1, space2, _initial_frame, _screen2) =
        reactor_with_window_on_space1_two_displays();

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space1)
    );
    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space1));

    reactor.reconcile_authoritative_active_window_snapshot(vec![(wsid, Some(space2))], false);

    assert_eq!(
        reactor.state.windows.window_server_space(wsid),
        Some(space2),
        "authoritative active-space membership should update the tracked native space"
    );
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2),
        "authoritative active-space membership should reassign the window to the new display"
    );
}

#[test]
fn topology_window_delta_reassigns_missing_window_to_inactive_space() {
    let (mut apps, mut reactor) = test_context_with_workspace_count(3);
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let active_space = SpaceId::new(1);
    let inactive_space = SpaceId::new(2);
    let pid: pid_t = 44;
    let moved = WindowId::new(pid, 1);
    let retained = WindowId::new(pid, 2);
    let moved_wsid = WindowServerId::new((pid as u32).saturating_mul(10_000) + 1);
    let retained_wsid = WindowServerId::new((pid as u32).saturating_mul(10_000) + 2);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(active_space)]));
    apps.make_app_and_settle(&mut reactor, pid, make_windows(2));

    let preserved_workspace = reactor.test_workspace(active_space, 2);
    let expected_destination_workspace = reactor.test_workspace(inactive_space, 2);
    reactor.send_layout_event(LayoutEvent::WindowRemovedPreserveFloating(moved));
    assert!(reactor.assign_test_window_to_workspace(active_space, moved, preserved_workspace));
    reactor.handle_test_workspace_command(active_space, &LayoutCommand::SwitchToWorkspace(2));
    reactor.send_layout_event(LayoutEvent::WindowAdded(active_space, moved));
    reactor.handle_test_workspace_command(active_space, &LayoutCommand::SwitchToWorkspace(0));

    reactor.mark_test_window_visible_in_space(moved_wsid, active_space);
    reactor.mark_test_window_visible_in_space(retained_wsid, active_space);
    crate::windows::platform::window_server::set_window_spaces_override(
        moved_wsid,
        Some(vec![inactive_space.get()]),
    );
    crate::windows::platform::window_server::set_space_window_list_for_space_override(
        active_space.get(),
        Some(vec![retained_wsid.as_u32()]),
    );

    reactor.handle_event(space_state_event_with(
        vec![frame],
        vec![Some(active_space)],
        |state| {
            state.has_seen_display_set = true;
            state.topology_window_delta =
                Some(crate::displays::domain::topology::TopologyWindowDelta {
                    epoch: 11,
                    flags: rini_skylight_sys::DisplayReconfigFlags::MOVED,
                    appeared: Vec::new(),
                    disappeared: vec![(moved_wsid, active_space)],
                });
        },
    ));

    crate::windows::platform::window_server::set_window_spaces_override(moved_wsid, None);
    crate::windows::platform::window_server::set_space_window_list_for_space_override(
        active_space.get(),
        None,
    );

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(moved),
        Some(inactive_space)
    );
    assert!(reactor.test_workspace_for_window(active_space, moved).is_none());
    assert_eq!(
        reactor.test_workspace_for_window(inactive_space, moved),
        Some(expected_destination_workspace)
    );
    assert!(!has_window_in_layout(&mut reactor, active_space, frame, moved));
    assert!(has_window_in_layout(&mut reactor, active_space, frame, retained));
}

#[test]
fn topology_window_delta_is_not_ignored_by_command_space_only_short_circuit() {
    let (mut reactor, wid, wsid, space1, space2, _initial_frame, screen2) =
        reactor_with_window_on_space1_two_displays();
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));

    crate::windows::platform::window_server::set_window_spaces_override(
        wsid,
        Some(vec![space2.get()]),
    );
    crate::windows::platform::window_server::set_space_window_list_for_space_override(
        space1.get(),
        Some(vec![]),
    );
    crate::windows::platform::window_server::set_space_window_list_for_space_override(
        space2.get(),
        Some(vec![wsid.as_u32()]),
    );

    reactor.handle_event(space_state_event_with(
        vec![screen1, screen2],
        vec![Some(space1), Some(space2)],
        |state| {
            state.has_seen_display_set = true;
            state.topology_window_delta =
                Some(crate::displays::domain::topology::TopologyWindowDelta {
                    epoch: 12,
                    flags: rini_skylight_sys::DisplayReconfigFlags::MOVED,
                    appeared: vec![(wsid, space2)],
                    disappeared: vec![(wsid, space1)],
                });
        },
    ));

    crate::windows::platform::window_server::set_window_spaces_override(wsid, None);
    crate::windows::platform::window_server::set_space_window_list_for_space_override(
        space1.get(),
        None,
    );
    crate::windows::platform::window_server::set_space_window_list_for_space_override(
        space2.get(),
        None,
    );

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2),
        "topology delta should still be processed even when the forwarded screens snapshot is unchanged"
    );
    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
}

#[test]
fn display_churn_quarantines_window_frame_and_membership_events() {
    let reactor = test_reactor();
    let space = SpaceId::new(7);
    let wsid = WindowServerId::new(77);
    let _ = crate::displays::platform::display_churn::begin(
        rini_skylight_sys::DisplayReconfigFlags::ADD,
    );

    let frame_changed = reactor.should_quarantine_during_display_churn(&Event::WindowFrameChanged(
        WindowId::new(99, 1),
        CGRect::new(CGPoint::new(10., 10.), CGSize::new(500., 400.)),
        None,
        Requested(false),
        Some(MouseState::Up),
    ));
    let appeared = reactor.should_quarantine_during_display_churn(&Event::WindowServerAppeared(
        wsid,
        space,
        SpaceEventKind::User,
    ));
    let destroyed = reactor.should_quarantine_during_display_churn(&Event::WindowServerDestroyed(
        wsid,
        space,
        SpaceEventKind::User,
    ));
    let ax_invalidated = reactor
        .should_quarantine_during_display_churn(&Event::WindowDestroyed(WindowId::new(99, 77)));
    let space_created = reactor.should_quarantine_during_display_churn(&Event::SpaceCreated(space));
    let space_destroyed =
        reactor.should_quarantine_during_display_churn(&Event::SpaceDestroyed(space));

    let _ = crate::displays::platform::display_churn::end();
    assert!(
        frame_changed,
        "WindowFrameChanged should be quarantined during churn"
    );
    assert!(
        appeared,
        "WindowServerAppeared should be quarantined during churn"
    );
    assert!(
        destroyed,
        "WindowServerDestroyed should be quarantined during churn"
    );
    assert!(
        ax_invalidated,
        "AX invalidation must be quarantined during display churn"
    );
    assert!(space_created, "SpaceCreated should be quarantined during churn");
    assert!(
        space_destroyed,
        "SpaceDestroyed should be quarantined during churn"
    );
}

#[test]
fn normal_macos_space_switch_does_not_arm_topology_relayout() {
    let mut reactor = test_reactor();

    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1280., 800.));
    let right = CGRect::new(CGPoint::new(1280., 0.), CGSize::new(1280., 800.));

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(SpaceId::new(11)), Some(SpaceId::new(22))],
    ));
    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(SpaceId::new(111)), Some(SpaceId::new(222))],
    ));
    assert_eq!(
        reactor.raw_spaces_for_current_screens(),
        vec![Some(SpaceId::new(111)), Some(SpaceId::new(222))],
        "Screen state should still advance to the newly active macOS spaces"
    );
    assert!(reactor.is_space_active(SpaceId::new(111)));
    assert!(reactor.is_space_active(SpaceId::new(222)));
}

#[test]
fn display_churn_snapshot_ack_triggers_visible_window_refresh() {
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let (mut apps, mut reactor) = test_context();

    reactor.handle_event(space_state_event(vec![screen], vec![Some(SpaceId::new(1))]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(1));

    reactor.handle_event(Event::DisplayChurnBegin);
    let Event::SpaceStateChanged(mut snapshot) =
        space_state_event(vec![screen], vec![Some(SpaceId::new(1))])
    else {
        unreachable!("space_state_event must produce a space-state event");
    };
    snapshot.releases_display_churn_refresh_quarantine = true;
    reactor.handle_event(Event::SpaceStateChanged(snapshot));

    assert!(
        apps.requests()
            .into_iter()
            .any(|request| matches!(request, Request::GetVisibleWindows)),
        "the snapshot acknowledgement should release churn and request visible windows"
    );
}

#[test]
fn display_churn_end_refresh_is_idempotent_without_topology_change() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));

    assert!(has_window_in_layout(&mut reactor, space, screen, wid));

    reactor.handle_event(Event::DisplayChurnEnd);
    apps.simulate_until_quiet(&mut reactor);

    assert!(
        has_window_in_layout(&mut reactor, space, screen, wid),
        "recovery refresh should preserve existing workspace membership when topology is unchanged"
    );
    assert!(
        apps.requests().is_empty(),
        "idempotent churn-end refresh should not trigger follow-up frame writes when nothing moved"
    );
}

#[test]
fn ax_destruction_removes_window_on_known_inactive_space_outside_churn() {
    let (mut reactor, wid, wsid, active_space, inactive_space, _frame) =
        reactor_with_window_on_space1();
    let inactive_workspace = reactor.test_workspace(inactive_space, 0);
    assert!(reactor.assign_test_window_to_workspace(inactive_space, wid, inactive_workspace));
    reactor.state.windows.set_window_server_space(wsid, Some(inactive_space));
    reactor.state.windows.mark_window_hidden(wsid);
    assert!(reactor.affinity().is_window_on_known_inactive_space(wid));

    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, Some(false));
    reactor.handle_event(Event::WindowDestroyed(wid));
    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, None);

    assert!(reactor.state.windows.record(wid).is_none());
    assert_eq!(reactor.test_workspace_for_window(inactive_space, wid), None);
    assert_eq!(reactor.test_workspace_for_window(active_space, wid), None);
}

#[test]
fn ax_destruction_removes_already_minimized_window_outside_churn() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    let wsid = reactor.test_window_server_id(wid);
    reactor.handle_event(Event::WindowMinimized(wid));
    assert!(reactor.state.windows.window(wid).unwrap().info.is_minimized);

    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, Some(false));
    reactor.handle_event(Event::WindowDestroyed(wid));
    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, None);

    assert!(reactor.state.windows.record(wid).is_none());
    assert!(!has_window_in_layout(&mut reactor, space, screen, wid));
}

#[test]
fn ax_destruction_removes_ordered_in_window_outside_churn() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    let wsid = reactor.test_window_server_id(wid);
    assert!(!reactor.refreshes_blocked());
    assert!(has_window_in_layout(&mut reactor, space, screen, wid));

    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, Some(true));
    reactor.handle_event(Event::WindowDestroyed(wid));
    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, None);

    assert!(reactor.state.windows.record(wid).is_none());
    assert!(!has_window_in_layout(&mut reactor, space, screen, wid));
    assert!(
        apps.requests()
            .iter()
            .all(|request| !matches!(request, Request::GetVisibleWindows)),
        "AX destruction outside churn should not trigger replacement-element polling",
    );
}

#[test]
fn sleep_ax_churn_preserves_modified_layout_through_recovery() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let windows = make_windows(4);
    let window_ids: Vec<_> = (1..=4).map(|idx| WindowId::new(1, idx)).collect();
    let rediscovered = window_ids.iter().copied().zip(windows.iter().cloned()).collect::<Vec<_>>();

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, windows);
    let default_layout = test_layout(&mut reactor, space, screen);
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, window_ids[1]));
    reactor.handle_test_layout_command(LayoutCommand::MoveNode(Direction::Up));
    let modified_layout = test_layout(&mut reactor, space, screen);
    assert_ne!(
        modified_layout, default_layout,
        "test setup must create a non-default layout"
    );

    reactor.handle_event(Event::SystemWillSleep);
    reactor.handle_event(Event::SystemWoke);
    reactor.handle_event(Event::SessionDidResignActive);
    for wid in &window_ids {
        reactor.handle_event(Event::WindowDestroyed(*wid));
    }

    assert_eq!(
        test_layout(&mut reactor, space, screen),
        modified_layout,
        "sleep-time AX destruction must not alter layout topology or weights",
    );

    reactor.handle_event(Event::SessionDidBecomeActive);
    let mut recovered =
        forwarded_space_state(make_screen_snapshots(vec![screen], vec![Some(space)]));
    recovered.releases_lifecycle_refresh_quarantine = true;
    for wid in &window_ids {
        recovered.active_window_spaces.insert(WindowServerId::new(wid.idx.get()), space);
    }
    reactor.handle_event(Event::SpaceStateChanged(recovered));
    reactor.discover_test_windows(1, rediscovered, window_ids.clone());

    assert_eq!(
        test_layout(&mut reactor, space, screen),
        modified_layout,
        "authoritative recovery and AX rediscovery must update existing nodes in place",
    );
}

#[test]
fn clamshell_sleep_preserves_nested_layout_across_display_replacement() {
    let (mut apps, mut reactor) = test_context();
    let external_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(3440., 1409.));
    let internal_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1083.));
    let space = SpaceId::new(1);
    let windows = make_windows(4);
    let window_ids: Vec<_> = (1..=4).map(|idx| WindowId::new(1, idx)).collect();
    let rediscovered = window_ids.iter().copied().zip(windows.iter().cloned()).collect::<Vec<_>>();

    apps.make_app_and_settle_on_screen(&mut reactor, external_screen, space, 1, windows);
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, window_ids[1]));
    reactor.handle_test_layout_command(LayoutCommand::MoveNode(Direction::Up));

    let topology_before = reactor
        .query_layout_state(Some(space.get()), None)
        .expect("external-display layout state")
        .container_tree;
    assert!(
        topology_before.children.iter().any(|child| !child.children.is_empty()),
        "test setup must reproduce the nested split/stack topology from the clamshell capture",
    );

    reactor.handle_event(Event::DisplayChurnBegin);
    reactor.handle_event(Event::SystemWillSleep);
    reactor.handle_event(Event::SessionDidResignActive);
    for wid in &window_ids {
        reactor.handle_event(Event::WindowDestroyed(*wid));
    }

    assert_eq!(
        reactor
            .query_layout_state(Some(space.get()), None)
            .expect("quarantined layout state")
            .container_tree,
        topology_before,
        "sleep-time AX destruction must not flatten the nested layout",
    );

    reactor.handle_event(Event::SystemWoke);
    reactor.handle_event(Event::SessionDidBecomeActive);
    let mut screens = make_screen_snapshots(vec![internal_screen], vec![Some(space)]);
    screens[0].display_uuid = "internal-display".to_string();
    let mut recovered = forwarded_space_state(screens);
    recovered.display_set_changed = true;
    recovered.topology_changed = true;
    recovered.allow_space_remap = true;
    recovered.should_force_refresh_layout = true;
    recovered.releases_lifecycle_refresh_quarantine = true;
    recovered.releases_display_churn_refresh_quarantine = true;
    recovered.resized_spaces.push((space, internal_screen.size));
    for wid in &window_ids {
        recovered.active_window_spaces.insert(WindowServerId::new(wid.idx.get()), space);
    }
    reactor.handle_event(Event::SpaceStateChanged(recovered));
    reactor.discover_test_windows(1, rediscovered, window_ids.clone());

    assert_eq!(
        reactor
            .query_layout_state(Some(space.get()), None)
            .expect("internal-display layout state")
            .container_tree,
        topology_before,
        "clamshell recovery must preserve container nesting, order, selection, and weights",
    );
    assert_eq!(
        test_layout(&mut reactor, space, internal_screen).len(),
        window_ids.len(),
        "every rediscovered window must occupy exactly one layout slot",
    );
}

/// A display that is unplugged and replugged must get its layout back, even though
/// macOS assigns a brand-new space id each time.
///
/// Two bugs made this fail. prune_display_state deleted the display's UUID -> space
/// mapping the moment it was unplugged, destroying the only durable link between a
/// physical display and its layout; and the spaces actor's own remap path is held
/// behind should_force_refresh_layout, which never became true across a real
/// unplug/replug cycle (every snapshot reported allow_space_remap: false).
///
/// Observed on hardware: the same monitor came back as space 479, then 484, then 487,
/// and windows arranged on it stayed on the built-in display.
#[test]
fn reconnected_display_regains_its_layout_under_the_new_space_id() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let external = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let builtin_space = SpaceId::new(1);
    let external_space = SpaceId::new(479);
    // macOS mints a new id on reconnect.
    let replugged_space = SpaceId::new(484);

    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));

    let pid = 1;
    reactor.add_test_app(pid);
    let wid = WindowId::new(pid, 1);
    let wsid = WindowServerId::new(931);
    let external_workspace = reactor.test_workspace(external_space, 0);
    reactor.add_test_window(wid, wsid, Some(external_space), external);
    assert!(reactor.assign_test_window_to_workspace(external_space, wid, external_workspace));

    // The engine must know which display owns that space.
    assert_eq!(
        reactor.state.display_memory.affinity.space_for_display("test-display-1"),
        Some(external_space),
        "the external display's space must be recorded before unplugging"
    );

    // Unplug: only the built-in remains.
    reactor.handle_event(space_state_event_with(
        vec![builtin],
        vec![Some(builtin_space)],
        |state| state.display_set_changed = true,
    ));

    // The mapping must SURVIVE the unplug — this is what prune_display_state destroyed.
    assert_eq!(
        reactor.state.display_memory.affinity.space_for_display("test-display-1"),
        Some(external_space),
        "unplugging must not forget which space belonged to the display"
    );

    // Replug with a DIFFERENT space id, as macOS actually does.
    reactor.handle_event(space_state_event_with(
        vec![builtin, external],
        vec![Some(builtin_space), Some(replugged_space)],
        |state| state.display_set_changed = true,
    ));

    // The mapping is overwritten by update_space_display regardless, so asserting on
    // it proves nothing. What matters is whether the WINDOW came back with the display.
    let landed = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .workspace_for_window(&reactor.state.windows, replugged_space, wid);
    assert!(
        landed.is_some(),
        "the window arranged on the external display must be reachable under its new \
         space id after reconnect; instead the display came back empty"
    );
}

/// Replugging a display must bring back the windows that were ON it, not whichever
/// windows happen to occupy the slots its old space snapshot recorded.
///
/// The previous fix remapped the display's whole SPACE from its old id onto its new one.
/// That replays a snapshot taken before the unplug, so once the user carried on working on
/// the remaining display it moved back the wrong windows entirely — reported on hardware
/// as Excel and Slack returning to the external instead of the two terminals that had
/// been there.
#[test]
fn replug_returns_the_windows_that_were_on_that_display() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let external = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let builtin_space = SpaceId::new(1);
    let external_space = SpaceId::new(479);
    let replugged_space = SpaceId::new(552);

    // Two windows on the external (the terminals), one on the built-in (Slack).
    let terminal_a = WindowId::new(1, 1);
    let terminal_b = WindowId::new(1, 2);
    let slack = WindowId::new(1, 3);
    set_space_membership(&[(builtin_space, &[903]), (external_space, &[901, 902])]);

    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
    reactor.add_test_app(1);
    let external_workspace = reactor.test_workspace(external_space, 0);
    let builtin_workspace = reactor.test_workspace(builtin_space, 0);
    reactor.add_test_window(
        terminal_a,
        WindowServerId::new(901),
        Some(external_space),
        external,
    );
    reactor.add_test_window(
        terminal_b,
        WindowServerId::new(902),
        Some(external_space),
        external,
    );
    reactor.add_test_window(slack, WindowServerId::new(903), Some(builtin_space), builtin);
    assert!(reactor.assign_test_window_to_workspace(
        external_space,
        terminal_a,
        external_workspace
    ));
    assert!(reactor.assign_test_window_to_workspace(
        external_space,
        terminal_b,
        external_workspace
    ));
    assert!(reactor.assign_test_window_to_workspace(builtin_space, slack, builtin_workspace));

    // Unplug. macOS evacuates the external's windows onto the built-in.
    set_space_membership(&[(builtin_space, &[901, 902, 903]), (external_space, &[])]);
    reactor.handle_event(space_state_event_with(
        vec![builtin],
        vec![Some(builtin_space)],
        |state| state.display_set_changed = true,
    ));

    // Replug under a new space id. macOS still reports all three on the built-in.
    set_space_membership(&[(builtin_space, &[901, 902, 903]), (replugged_space, &[])]);
    reactor.handle_event(space_state_event_with(
        vec![builtin, external],
        vec![Some(builtin_space), Some(replugged_space)],
        |state| state.display_set_changed = true,
    ));

    let space_of = |reactor: &Reactor, window: WindowId| {
        reactor
            .state
            .windows
            .workspace_info_for_window(window)
            .map(|assignment| assignment.space)
    };
    assert_eq!(
        space_of(&reactor, terminal_a),
        Some(replugged_space),
        "a window that was on the external must return to it"
    );
    assert_eq!(
        space_of(&reactor, terminal_b),
        Some(replugged_space),
        "both windows that were on the external must return to it"
    );
    assert_eq!(
        space_of(&reactor, slack),
        Some(builtin_space),
        "a window that was never on the external must NOT be dragged onto it"
    );
}

/// A replug must not disturb the display that stayed attached.
///
/// remap_space deletes the workspaces already sitting on the target space id, which drops
/// the WindowStore assignment of every window macOS had put there. Those were then
/// re-assigned from scratch in discovery order, which is what reshuffled the built-in's
/// column order on every dock/undock cycle.
#[test]
fn replug_leaves_the_other_display_group_order_untouched() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let external = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let builtin_space = SpaceId::new(1);
    let external_space = SpaceId::new(479);
    let replugged_space = SpaceId::new(552);

    let strip: Vec<WindowId> = (1..=3).map(|idx| WindowId::new(1, idx)).collect();
    let resident = WindowId::new(1, 9);
    set_space_membership(&[(builtin_space, &[801, 802, 803]), (external_space, &[909])]);

    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
    reactor.add_test_app(1);
    let builtin_workspace = reactor.test_workspace(builtin_space, 0);
    let external_workspace = reactor.test_workspace(external_space, 0);
    reactor.add_test_window(
        resident,
        WindowServerId::new(909),
        Some(external_space),
        external,
    );
    assert!(reactor.assign_test_window_to_workspace(external_space, resident, external_workspace));
    for (offset, window) in strip.iter().enumerate() {
        reactor.add_test_window(
            *window,
            WindowServerId::new(801 + offset as u32),
            Some(builtin_space),
            builtin,
        );
        assert!(reactor.assign_test_window_to_workspace(builtin_space, *window, builtin_workspace));
    }
    let order_before = reactor.test_workspace_windows(builtin_space, builtin_workspace);
    assert_eq!(
        order_before, strip,
        "test setup must establish a known strip order"
    );

    set_space_membership(&[
        (builtin_space, &[801, 802, 803, 909]),
        (external_space, &[]),
    ]);
    reactor.handle_event(space_state_event_with(
        vec![builtin],
        vec![Some(builtin_space)],
        |state| state.display_set_changed = true,
    ));
    set_space_membership(&[
        (builtin_space, &[801, 802, 803, 909]),
        (replugged_space, &[]),
    ]);
    reactor.handle_event(space_state_event_with(
        vec![builtin, external],
        vec![Some(builtin_space), Some(replugged_space)],
        |state| state.display_set_changed = true,
    ));

    assert_eq!(
        reactor.test_workspace_windows(builtin_space, builtin_workspace),
        order_before,
        "a replug must not reorder or drop the windows on the display that stayed attached"
    );
}

/// A window parked on the built-in only because its own display was unplugged must NOT be
/// re-homed to the built-in. That would overwrite the record the replug depends on.
#[test]
fn evacuated_windows_keep_their_home_while_their_display_is_detached() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let external = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let builtin_space = SpaceId::new(1);
    let external_space = SpaceId::new(479);
    let exile = WindowId::new(1, 1);

    set_space_membership(&[(builtin_space, &[]), (external_space, &[901])]);
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
    reactor.add_test_app(1);
    let external_workspace = reactor.test_workspace(external_space, 0);
    reactor.add_test_window(exile, WindowServerId::new(901), Some(external_space), external);
    assert!(reactor.assign_test_window_to_workspace(external_space, exile, external_workspace));
    reactor.send_layout_event(LayoutEvent::WindowAdded(external_space, exile));
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
    assert_eq!(
        reactor.state.display_memory.affinity.window_home(exile),
        Some("test-display-1")
    );

    // Unplug, then let the built-in-only topology settle repeatedly, as it does in practice.
    set_space_membership(&[(builtin_space, &[901]), (external_space, &[])]);
    reactor.handle_event(space_state_event_with(
        vec![builtin],
        vec![Some(builtin_space)],
        |state| state.display_set_changed = true,
    ));
    reactor.handle_event(space_state_event(vec![builtin], vec![Some(builtin_space)]));
    reactor.handle_event(space_state_event(vec![builtin], vec![Some(builtin_space)]));

    assert_eq!(
        reactor.state.display_memory.affinity.window_home(exile),
        Some("test-display-1"),
        "an evacuated window must keep its own display's home, otherwise the replug has \
         nothing left to bring it back with"
    );
}

/// Windows kept side by side must come back side by side.
///
/// Repatriation used to run in WindowId order, which is unrelated to strip position, so two
/// terminals the user had adjacent came back as terminal, Chrome, terminal, editor.
#[test]
fn replug_rebuilds_strip_adjacency() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let external = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let builtin_space = SpaceId::new(1);
    let external_space = SpaceId::new(479);
    let replugged_space = SpaceId::new(552);

    // On the external: chrome, then the two terminals adjacent, then the editor. The
    // terminals have the HIGHEST ids, so id order would place them last, not in the middle.
    let chrome = WindowId::new(1, 1);
    let terminal_a = WindowId::new(1, 8);
    let terminal_b = WindowId::new(1, 9);
    let editor = WindowId::new(1, 2);
    let ids = [
        (chrome, 901u32),
        (terminal_a, 908),
        (terminal_b, 909),
        (editor, 902),
    ];

    set_space_membership(&[
        (builtin_space, &[]),
        (external_space, &[901, 902, 908, 909]),
    ]);
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
    reactor.add_test_app(1);
    let external_workspace = reactor.test_workspace(external_space, 0);
    for (window, server_id) in ids {
        reactor.add_test_window(
            window,
            WindowServerId::new(server_id),
            Some(external_space),
            external,
        );
        assert!(reactor.assign_test_window_to_workspace(
            external_space,
            window,
            external_workspace
        ));
    }
    // Establish the visual order chrome, terminal_a, terminal_b, editor.
    for window in [chrome, terminal_a, terminal_b, editor] {
        reactor.send_layout_event(LayoutEvent::WindowAdded(external_space, window));
    }
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
    let strip_before = reactor
        .layout_manager
        .layout_engine
        .ordered_windows_in_active_workspace(external_space);
    assert_eq!(
        strip_before,
        vec![chrome, terminal_a, terminal_b, editor],
        "test setup must establish a known strip order on the external"
    );

    set_space_membership(&[
        (builtin_space, &[901, 902, 908, 909]),
        (external_space, &[]),
    ]);
    reactor.handle_event(space_state_event_with(
        vec![builtin],
        vec![Some(builtin_space)],
        |state| state.display_set_changed = true,
    ));
    set_space_membership(&[
        (builtin_space, &[901, 902, 908, 909]),
        (replugged_space, &[]),
    ]);
    reactor.handle_event(space_state_event_with(
        vec![builtin, external],
        vec![Some(builtin_space), Some(replugged_space)],
        |state| state.display_set_changed = true,
    ));

    let strip_after = reactor
        .layout_manager
        .layout_engine
        .ordered_windows_in_active_workspace(replugged_space);
    assert_eq!(
        strip_after, strip_before,
        "the strip must come back in the order it was left, keeping adjacent windows adjacent"
    );
}

/// A closed window must not keep its display affinity.
///
/// Affinity was only cleared on the WindowRemoved path, but every display change removes
/// windows with WindowRemovedPreserveFloating, which does not clear it. A window closed
/// while its display was unplugged therefore kept its home forever.
///
/// Measured on hardware: the external's affinity list held three long-closed windows (two
/// Ghostty, one Chrome) while all fourteen live windows were homed to the built-in.
/// Repatriation logged `homed=[3 windows] to_move=[]` and the external came back empty on
/// every replug.
#[test]
fn closed_windows_do_not_keep_their_display_affinity() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let external = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let builtin_space = SpaceId::new(1);
    let external_space = SpaceId::new(479);
    let doomed = WindowId::new(1, 1);
    let survivor = WindowId::new(1, 2);

    set_space_membership(&[(builtin_space, &[]), (external_space, &[901, 902])]);
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
    reactor.add_test_app(1);
    let external_workspace = reactor.test_workspace(external_space, 0);
    for (window, wsid) in [(doomed, 901u32), (survivor, 902)] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(external_space), external);
        assert!(reactor.assign_test_window_to_workspace(
            external_space,
            window,
            external_workspace
        ));
        reactor.send_layout_event(LayoutEvent::WindowAdded(external_space, window));
    }
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
    assert_eq!(
        reactor.state.display_memory.affinity.window_home(doomed),
        Some("test-display-1"),
        "test setup must home both windows to the external"
    );

    // The window is closed. This is the removal flavour the display-change path uses, and
    // the one that used to leave affinity behind.
    reactor.send_layout_event(LayoutEvent::WindowRemovedPreserveFloating(doomed));
    reactor.state.windows.remove_window(doomed);

    // Any settled topology is enough to notice.
    set_space_membership(&[(builtin_space, &[]), (external_space, &[902])]);
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));

    assert_eq!(
        reactor.state.display_memory.affinity.window_home(doomed),
        None,
        "a closed window must not keep a home; stale entries make a replug look like it \
         has windows to bring back when it does not"
    );
    assert!(
        !reactor
            .state
            .display_memory
            .affinity
            .windows_homed_to("test-display-1")
            .contains(&doomed),
        "and it must be gone from the display's affinity list"
    );
    assert_eq!(
        reactor.state.display_memory.affinity.window_home(survivor),
        Some("test-display-1"),
        "the window that is still open keeps its home"
    );
}

/// The diagnostics dump must report every space, not just the queried one.
///
/// This is the tool-level defect that made three diagnoses wrong in a row:
/// `query windows` with no space falls back to ONE space's active workspace, so windows
/// on the other display appear to be missing. They were present the whole time.
#[test]
fn diagnostics_report_every_display_not_just_the_default_space() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let external = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let builtin_space = SpaceId::new(1);
    let external_space = SpaceId::new(479);
    let on_builtin = WindowId::new(1, 1);
    let on_external = WindowId::new(1, 2);

    set_space_membership(&[(builtin_space, &[901]), (external_space, &[902])]);
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
    reactor.add_test_app(1);
    for (window, wsid, space, frame) in [
        (on_builtin, 901u32, builtin_space, builtin),
        (on_external, 902, external_space, external),
    ] {
        let workspace = reactor.test_workspace(space, 0);
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), frame);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspace));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
    }

    let diagnostics = reactor.query_diagnostics();

    assert_eq!(diagnostics.spaces.len(), 2, "both displays must be reported");
    let spaces: Vec<u64> = diagnostics.spaces.iter().map(|space| space.space_id).collect();
    assert!(spaces.contains(&builtin_space.get()) && spaces.contains(&external_space.get()));

    let external_dump = diagnostics
        .spaces
        .iter()
        .find(|space| space.space_id == external_space.get())
        .expect("external space present");
    assert!(
        external_dump
            .windows
            .iter()
            .any(|window| window.window_id == on_external.into()),
        "a window on the non-default display must appear in its own space's dump, \
         which is exactly what `query windows` hid"
    );
    assert_eq!(
        external_dump.display_uuid.as_deref(),
        Some("test-display-1"),
        "each space must carry the display it belongs to"
    );
    assert!(
        external_dump.orphaned_windows.is_empty(),
        "a window in the layout tree must not be reported as orphaned"
    );
}

/// A window rini parked off-screen must not be treated as having changed display.
///
/// Windows belonging to a workspace their display is not showing are moved off-screen, and
/// macOS refuses to keep a window entirely outside every display — so those coordinates land
/// inside the NEIGHBOURING display. WindowServer then announces the window there.
///
/// Believing that announcement created a feedback loop: park off the built-in, get claimed by
/// the external, park off the external on the next switch, and so on until every window had
/// walked onto one display. Measured on hardware as all 17 windows collapsing onto the
/// external, with both displays stuck on the same workspace.
///
/// The old per-display workspace model hid this: a window reassigned across displays landed in
/// a different workspace OBJECT, which broke the cycle by accident.
#[test]
fn a_parked_window_is_not_claimed_by_the_display_it_is_parked_over() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let external = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let builtin_space = SpaceId::new(1);
    let external_space = SpaceId::new(479);
    let parked = WindowId::new(1, 1);
    let wsid = WindowServerId::new(901);

    set_space_membership(&[(builtin_space, &[901]), (external_space, &[])]);
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
    reactor.add_test_app(1);

    // The window belongs to workspace 1 while the built-in shows workspace 0, so rini parks
    // it — and the parked frame sits over the external.
    let workspaces = reactor.test_workspace_ids(builtin_space);
    assert!(reactor.set_test_active_workspace(builtin_space, workspaces[0]));
    reactor.add_test_window(parked, wsid, Some(builtin_space), builtin);
    assert!(reactor.assign_test_window_to_workspace(builtin_space, parked, workspaces[1]));

    // WindowServer announces it on the external, as it does for a parked window whose frame
    // overlaps that display. Its space MEMBERSHIP still says built-in, which is what
    // distinguishes this from a real move.
    crate::windows::platform::window_server::set_window_spaces_override(
        wsid,
        Some(vec![builtin_space.get()]),
    );
    window_server_appeared(&mut reactor, wsid, external_space, SpaceEventKind::User);
    crate::windows::platform::window_server::set_window_spaces_override(wsid, None);

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(parked),
        Some(builtin_space),
        "a parked window's position is not evidence of a display change"
    );
    assert_eq!(
        reactor
            .state
            .windows
            .workspace_info_for_window(parked)
            .map(|assignment| assignment.workspace_id),
        Some(workspaces[1]),
        "and it stays in the workspace it belongs to"
    );
}

/// Redistribute moves windows back to their home display without touching their workspace.
///
/// Recovery for a layout where windows have piled onto one display. Workspace membership is
/// the window's identity, so a recovery command must not guess at it — only the display is
/// corrected.
#[test]
fn redistribute_returns_windows_to_their_home_display_only() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let external = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let builtin_space = SpaceId::new(1);
    let external_space = SpaceId::new(479);
    let displaced = WindowId::new(1, 1);
    let settled = WindowId::new(1, 2);

    set_space_membership(&[(builtin_space, &[901, 902]), (external_space, &[])]);
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
    reactor.add_test_app(1);
    let workspaces = reactor.test_workspace_ids(builtin_space);

    // `displaced` belongs on the external but sits on the built-in, in workspace 2.
    reactor.add_test_window(displaced, WindowServerId::new(901), Some(builtin_space), builtin);
    assert!(reactor.assign_test_window_to_workspace(builtin_space, displaced, workspaces[2]));
    reactor.send_layout_event(LayoutEvent::WindowAdded(builtin_space, displaced));
    reactor.layout_manager.layout_engine.set_window_display_home(
        &mut reactor.state.display_memory,
        displaced,
        external_space,
    );

    // `settled` is already where it belongs.
    reactor.add_test_window(settled, WindowServerId::new(902), Some(builtin_space), builtin);
    assert!(reactor.assign_test_window_to_workspace(builtin_space, settled, workspaces[0]));
    reactor.send_layout_event(LayoutEvent::WindowAdded(builtin_space, settled));
    reactor.layout_manager.layout_engine.set_window_display_home(
        &mut reactor.state.display_memory,
        settled,
        builtin_space,
    );

    reactor.handle_event(Event::Command(Command::Reactor(
        ReactorCommand::RedistributeWindows,
    )));

    let displaced_now = reactor
        .state
        .windows
        .workspace_info_for_window(displaced)
        .expect("displaced window keeps an assignment");
    assert_eq!(
        displaced_now.space, external_space,
        "it must move to the display its affinity records"
    );
    assert_eq!(
        displaced_now.workspace_id, workspaces[2],
        "and keep the workspace it was in: redistribute corrects the display, nothing else"
    );

    let settled_now = reactor
        .state
        .windows
        .workspace_info_for_window(settled)
        .expect("settled window keeps an assignment");
    assert_eq!(
        settled_now.space, builtin_space,
        "a window already on its home display is left alone"
    );
    assert_eq!(settled_now.workspace_id, workspaces[0]);
}

/// The overlay is one window, so it has to be on the display whose windows are about to move.
///
/// Measured on a two-display setup: cmd-tabbing between two windows of the BUILT-IN display animated the
/// EXTERNAL screen, because the cursor was over there and the overlay followed the active display. Same
/// switch, wrong screen: the external showed the built-in's windows sliding, and the built-in snapped.
#[test]
fn the_animation_overlay_follows_the_space_being_animated_not_the_active_display() {
    let (_apps, mut reactor) = test_context();
    let built_in = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let external = CGRect::new(CGPoint::new(-670., -1692.), CGSize::new(3008., 1692.));
    let built_in_space = SpaceId::new(1);
    let external_space = SpaceId::new(519);

    reactor.handle_event(space_state_event(
        vec![built_in, external],
        vec![Some(built_in_space), Some(external_space)],
    ));
    // The cursor is on the external display, which is what used to decide this.
    reactor.handle_event(Event::ActiveDisplayChanged {
        menu_bar_space: Some(external_space),
        command_space: Some(external_space),
    });

    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.workspace_animation_tx = Some(animation_tx);

    reactor.publish_animation_display_for(Some(built_in_space));
    let (_, published) = animation_rx.try_recv().expect("a display should be published");
    let crate::animation::platform::engine::Event::SetDisplay { id, .. } = published else {
        panic!("expected SetDisplay, got {published:?}");
    };
    assert_eq!(
        id, 0,
        "the built-in display is screen 0, and its space is the one animating"
    );

    reactor.publish_animation_display();
    let (_, published) = animation_rx.try_recv().expect("a display should be published");
    let crate::animation::platform::engine::Event::SetDisplay { id, .. } = published else {
        panic!("expected SetDisplay, got {published:?}");
    };
    assert_eq!(
        id, 1,
        "with no space in mind the active display is still the right answer"
    );
}
