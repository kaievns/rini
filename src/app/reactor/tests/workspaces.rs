//! Workspaces: creating, switching, moving windows between them, app rules, and queries.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::WindowServerId;
use rini_geometry::SameAs;
use rini_geometry::space::SurfacePoint;
use rini_ipc::protocol::{SwitchScope, WorkspaceSelector};
use test_log::test;

use super::fixtures::*;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::displays::domain::topology::{DisplaySet, Topology};
use crate::windows::domain::raise::{self as raise_manager, RaiseRequest};
use crate::windows::domain::request::Request;
use crate::workspaces::{LayoutCommand, LayoutEvent};

#[test]
fn layout_query_exposes_active_and_inactive_workspace_container_trees() {
    let mut reactor = test_reactor();
    let space = SpaceId::new(1);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.send_layout_event(LayoutEvent::SpaceExposed(space, screen.size));
    reactor.send_layout_event(LayoutEvent::WindowAdded(space, WindowId::new(42, 1)));
    reactor.send_layout_event(LayoutEvent::WindowAdded(space, WindowId::new(42, 2)));

    let state = reactor.query_layout_state(None, None).expect("layout state");
    assert_eq!(state.space_id, space.get());
    assert!(state.is_active_workspace);
    assert_eq!(
        state.container_tree.node_type,
        rini_ipc::protocol::ContainerNodeType::Container
    );
    assert_eq!(state.container_tree.children.len(), 2);

    // Windows hang off COLUMNS, one level down: the top-level children are columns
    // (`window_id: None`, `role: "column"`) and the windows sit inside them.
    let windows: Vec<&rini_ipc::protocol::ContainerTreeNode> = state
        .container_tree
        .children
        .iter()
        .flat_map(|column| column.children.iter())
        .collect();
    assert!(
        state.container_tree.children.iter().all(|node| node.window_id.is_none()),
        "scrolling exposes columns at the top level: {:#?}",
        state.container_tree
    );
    assert_eq!(windows.iter().filter(|node| node.window_id.is_some()).count(), 2);
    assert_eq!(
        state.selected_window,
        windows.iter().find(|node| node.is_selected).and_then(|node| node.window_id),
        "the selected window must be reachable through its column"
    );

    let original_workspace = state.workspace_id;
    reactor.handle_test_layout_command(LayoutCommand::NextWorkspace(Some(false)));
    let inactive = reactor
        .query_layout_state(Some(space.get()), Some(original_workspace))
        .expect("inactive workspace layout state");
    assert!(!inactive.is_active_workspace);
    assert_eq!(inactive.workspace_id, original_workspace);
    assert!(reactor.query_layout_state(Some(space.get()), Some(usize::MAX)).is_none());
}

#[test]
fn workspace_command_space_follows_forwarded_space_snapshot() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let old_space = SpaceId::new(1);
    let new_space = SpaceId::new(2);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(old_space)]));
    make_active_app_with_count(&mut apps, &mut reactor, 1, 1, Some(WindowId::new(1, 1)));

    assert_eq!(reactor.workspace_command_space(), Some(old_space));

    reactor.handle_event(space_state_event(vec![screen], vec![Some(new_space)]));

    assert_eq!(
        reactor.workspace_command_space(),
        Some(new_space),
        "workspace commands must follow the forwarded active screen space, not stale main-window space",
    );
}

#[test]
fn forwarded_active_spaces_filter_active_workspace_context() {
    let mut reactor = test_reactor();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let inactive_space = SpaceId::new(1);
    let active_space = SpaceId::new(2);

    reactor.handle_event(space_state_event_with(
        vec![left, right],
        vec![Some(inactive_space), Some(active_space)],
        |state| {
            state.active_spaces = [active_space].into_iter().collect();
            state.menu_bar_space = Some(active_space);
            state.command_space = Some(active_space);
        },
    ));

    assert!(!reactor.is_space_active(inactive_space));
    assert!(reactor.is_space_active(active_space));
    assert_eq!(
        reactor.space_state().active_spaces,
        [active_space].into_iter().collect(),
        "the stored forwarded state should reflect the authority's active-space set",
    );
}

#[test]
fn workspace_commands_follow_active_display_space_across_active_displays() {
    let mut reactor = test_reactor();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let right = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let left_space = SpaceId::new(1);
    let right_space = SpaceId::new(2);
    let source = WindowId::new(1, 1);
    let target = WindowId::new(1, 2);
    let windows = [
        (source, WindowServerId::new(201), left_space, left),
        (target, WindowServerId::new(202), right_space, right),
    ];

    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(left_space),
        Some(right_space),
    ]));

    reactor.add_test_app(1);

    reactor.send_layout_event(LayoutEvent::SpaceExposed(left_space, left.size));
    reactor.send_layout_event(LayoutEvent::SpaceExposed(right_space, right.size));

    let left_workspaces = reactor.test_workspace_ids(left_space);
    let right_workspaces = reactor.test_workspace_ids(right_space);
    let left_workspace = left_workspaces[0];
    let next_left_workspace = left_workspaces[1];
    let right_workspace = right_workspaces[0];

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

    reactor.send_layout_event(LayoutEvent::WindowFocused(right_space, target));

    assert_eq!(reactor.workspace_command_space(), Some(left_space));
    assert_eq!(reactor.command_context_space(), Some(left_space));
    assert_eq!(
        reactor.layout().active_workspace(right_space),
        Some(right_workspace)
    );

    reactor.handle_test_layout_command(LayoutCommand::NextWorkspace(None));

    assert_eq!(
        reactor.layout().active_workspace(left_space),
        Some(next_left_workspace),
        "workspace commands should follow the active display space"
    );
    assert_eq!(
        reactor.layout().active_workspace(right_space),
        Some(right_workspace),
        "workspace commands should not switch the focused window's display when it is not active"
    );
}

#[test]
fn workspace_switch_arrange_is_scoped_to_its_command_space() {
    let mut reactor = test_reactor();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let right = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let left_space = SpaceId::new(1);
    let right_space = SpaceId::new(2);

    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(left_space),
        Some(right_space),
    ]));

    let switch = reactor.dispatch_test_layout_command(LayoutCommand::NextWorkspace(None));
    assert_eq!(switch.arrange.space_scope, Some(left_space));

    let ordinary = reactor.dispatch_test_layout_command(LayoutCommand::NextWindow);
    assert_eq!(ordinary.arrange.space_scope, None);
}

#[test]
fn no_op_workspace_switch_does_not_request_arrangement() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));

    let already_active = reactor.dispatch_test_layout_command(LayoutCommand::SwitchToWorkspace(0));
    assert!(!already_active.arrange.requested);
    assert!(already_active.layout_responses.is_empty());

    let missing =
        reactor.dispatch_test_layout_command(LayoutCommand::SwitchToWorkspace(usize::MAX));
    assert!(!missing.arrange.requested);
    assert!(missing.layout_responses.is_empty());
}

#[test]
fn workspace_queries_are_isolated_per_macos_space() {
    let mut reactor = test_reactor();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);

    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(space1),
        Some(space2),
    ]));

    reactor.handle_test_workspace_command(space1, &LayoutCommand::SwitchToWorkspace(0));
    reactor.handle_test_workspace_command(space2, &LayoutCommand::SwitchToWorkspace(1));

    let space1_workspaces = reactor.query_workspaces(Some(space1));
    let space2_workspaces = reactor.query_workspaces(Some(space2));

    assert_eq!(space1_workspaces.iter().filter(|ws| ws.is_active).count(), 1);
    assert_eq!(space2_workspaces.iter().filter(|ws| ws.is_active).count(), 1);
    assert_ne!(
        space1_workspaces.iter().position(|ws| ws.is_active),
        space2_workspaces.iter().position(|ws| ws.is_active),
        "each macOS space must retain its own active virtual workspace state",
    );

    reactor.handle_event(space_state_event(vec![left], vec![Some(space2)]));

    let default_workspaces = reactor.query_workspaces(None);
    assert_eq!(
        default_workspaces.iter().position(|ws| ws.is_active),
        space2_workspaces.iter().position(|ws| ws.is_active),
        "default workspace queries must reflect the currently active macOS space",
    );
}

#[test]
fn discovery_prefers_authoritative_space_over_geometry_when_displays_overlap_workspaces() {
    let OneWindow {
        mut reactor,
        window: wid,
        server_id: wsid,
        space1,
        space2,
        ..
    } = reactor_with_window_moved_to_space2();
    let conflicting_frame = CGRect::new(CGPoint::new(100., 100.), CGSize::new(800., 600.));

    reactor
        .windows_mut()
        .window_mut(wid)
        .expect("window should exist")
        .frame_monotonic = conflicting_frame;
    reactor.track_test_window_server_info(wsid, wid.pid, conflicting_frame);

    assert_eq!(
        reactor.affinity().discovery_space_for_window_id(wid),
        Some(space2),
        "discovery should stay in the authoritative native space instead of hopping to another display's geometry"
    );
    assert_ne!(
        reactor.affinity().discovery_space_for_window_id(wid),
        Some(space1),
        "same-index workspaces on other displays must stay isolated"
    );
}

#[test]
fn workspace_switch_batches_all_window_positions_with_eui_enabled() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(2));
    let _ = apps.requests();

    reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
        workspace: WorkspaceSelector::Index(1),
        follow: false,
        window_id: Some(2),
    });
    apps.simulate_until_quiet(&mut reactor);
    let _ = apps.requests();

    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(1));

    let requests = apps.requests();
    assert!(
        requests.iter().any(|req| {
            matches!(
                req,
                Request::SetWorkspaceSwitchPositions(positions, _, true)
                    if positions.iter().any(|(wid, _)| *wid == WindowId::new(1, 1))
            )
        }),
        "expected a position-only workspace-switch batch with eui enabled: {requests:?}"
    );
}

#[test]
fn non_workspace_instant_layout_keeps_full_frame_batch() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    let _ = apps.requests();

    let target = CGRect::new(CGPoint::new(25., 30.), CGSize::new(700., 650.));
    assert!(reactor.instant_layout(space, &[(wid, target)], None,));

    let requests = apps.requests();
    assert!(
        requests.iter().any(|request| matches!(
            request,
            Request::SetBatchWindowFrame(frames, _, true)
                if frames.as_slice() == [(wid, target)]
        )),
        "ordinary instant layouts must retain full-frame writes: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|request| !matches!(request, Request::SetWorkspaceSwitchPositions(..))),
        "the workspace-switch-only request escaped into an ordinary instant layout: {requests:?}"
    );
}

#[test]
fn workspace_switch_layout_falls_back_to_full_frames_for_size_changes() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    let _ = apps.requests();

    let target = CGRect::new(CGPoint::new(25., 30.), CGSize::new(700., 650.));
    assert!(reactor.workspace_switch_layout(space, None, &[(wid, target)], None,));

    let requests = apps.requests();
    assert!(
        requests.iter().any(|request| matches!(
            request,
            Request::SetBatchWindowFrame(frames, _, true)
                if frames.as_slice() == [(wid, target)]
        )),
        "workspace layouts with size changes must retain full-frame writes: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|request| !matches!(request, Request::SetWorkspaceSwitchPositions(..))),
        "a size-changing workspace layout must not use position-only writes: {requests:?}"
    );
}

#[test]
fn topology_change_clears_stale_pending_hide_target_before_next_workspace_layout() {
    let (mut apps, mut reactor) = test_context_with_workspace_count(2);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    let _ = apps.requests();

    let wsid = reactor.test_window_server_id(wid);
    let workspaces = reactor.test_workspace_ids(space);
    let hidden_workspace = workspaces[0];
    let active_workspace = workspaces[1];

    assert!(reactor.set_test_active_workspace(space, active_workspace));
    assert!(reactor.assign_test_window_to_workspace(space, wid, hidden_workspace));

    if let Some(window) = reactor.windows_mut().window_mut(wid) {
        window.frame_monotonic = CGRect::new(CGPoint::new(200.0, 200.0), CGSize::new(400.0, 400.0));
    }

    let hidden_target = reactor
        .test_layout_with_virtual_workspaces(space, screen)
        .into_iter()
        .find(|(layout_wid, _)| *layout_wid == wid)
        .map(|(_, frame)| frame)
        .expect("inactive-workspace window should still be laid out to a hidden position");

    reactor.pend_test_frame(wsid, hidden_target);

    assert!(!reactor.update_layout(false, true, None));
    assert!(
        apps.requests().is_empty(),
        "a stale pending target suppresses the hide write before topology invalidation"
    );

    reactor.handle_event(space_state_event_with(
        vec![screen],
        vec![Some(space)],
        |state| {
            state.has_seen_display_set = true;
            state.topology = Topology::Changed(DisplaySet::Changed);
        },
    ));
    let requests = apps.requests();
    assert!(
        requests.iter().any(|req| {
            matches!(req,
                Request::SetWindowFrame(req_wid, frame, _, true)
                    if *req_wid == wid && frame.same_as(hidden_target)
            ) || matches!(req,
                Request::SetBatchWindowFrame(frames, _, true)
                    if frames.iter().any(|(req_wid, frame)| *req_wid == wid && frame.same_as(hidden_target))
            )
        }),
        "topology invalidation must resend the hidden-window frame write instead of treating the stale target as still pending: {requests:?}"
    );
}

#[test]
fn auto_workspace_switch_follows_activated_window_when_same_app_is_visible_elsewhere() {
    let (mut apps, mut reactor) = test_context();
    let mut raise_manager_rx = reactor.connect_test_raises();

    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let stale_focus = WindowId::new(1, 1);
    let activated = WindowId::new(2, 1);
    let same_app_visible = WindowId::new(2, 2);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.handle_events(apps.make_app(1, make_windows(1)));
    apps.make_app_and_settle(&mut reactor, 2, make_windows(2));

    reactor.send_layout_event(LayoutEvent::WindowFocused(space, stale_focus));
    reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
        workspace: WorkspaceSelector::Index(1),
        follow: false,
        window_id: None,
    });
    apps.simulate_until_quiet(&mut reactor);

    reactor.send_layout_event(LayoutEvent::WindowFocused(space, activated));
    reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
        workspace: WorkspaceSelector::Index(1),
        follow: false,
        window_id: None,
    });
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(1));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, stale_focus));
    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(0));
    apps.simulate_until_quiet(&mut reactor);
    while raise_manager_rx.try_recv().is_ok() {}

    assert!(
        reactor
            .layout()
            .is_window_in_active_workspace(reactor.windows(), space, same_app_visible),
        "another window from the activated app should remain visible on the current workspace"
    );
    reactor.handle_event(Event::ApplicationGloballyActivated(activated.pid));
    assert_eq!(reactor.main_window(), Some(activated));
    assert_eq!(
        reactor.layout().active_workspace_idx(space),
        Some(0),
        "Carbon activation must wait for the app thread to resolve its AX focus"
    );
    let activation_requests = apps.requests();
    assert!(
        activation_requests
            .iter()
            .all(|request| !matches!(request, Request::GetVisibleWindows)),
        "Carbon activation should not enumerate every AX window: {activation_requests:?}"
    );
    assert!(
        activation_requests
            .iter()
            .any(|request| matches!(request, Request::ApplicationGloballyActivated(pid) if *pid == activated.pid)),
        "Carbon activation should be reconciled on the app thread: {activation_requests:?}"
    );
    assert!(raise_manager_rx.try_recv().is_err());

    // This is the resolved event emitted by the app thread after it refreshes
    // the current main window and applies quiet-activation bookkeeping.
    reactor.handle_event(Event::ApplicationActivated(activated.pid, Quiet::No));

    let requests = apps.requests();
    assert!(
        requests.iter().any(|request| match request {
            Request::SetWindowFrame(wid, _, _, _) => *wid == activated,
            Request::SetBatchWindowFrame(frames, _, _) => {
                frames.iter().any(|(wid, _)| *wid == activated)
            }
            Request::SetWorkspaceSwitchPositions(positions, _, _) => {
                positions.iter().any(|(wid, _)| *wid == activated)
            }
            _ => false,
        }),
        "auto workspace switch should arrange the activated window immediately: {requests:?}"
    );

    let msg = raise_manager_rx.try_recv().expect("Should have sent an event").1;
    match msg {
        raise_manager::Event::RaiseRequest(RaiseRequest { focus_window, focus_quiet, .. }) => {
            assert_eq!(focus_window.map(|(wid, _)| wid), Some(activated));
            assert_eq!(focus_quiet, Quiet::Yes);
        }
        _ => panic!("Unexpected event: {msg:?}"),
    }
}

#[test]
fn dock_activation_reveals_window_in_active_scrolling_workspace() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(600., 600.));
    let space = SpaceId::new(1);
    let pid = 2;
    let activated = WindowId::new(pid, 3);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, pid, make_windows(3));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(pid, 1)));
    apps.simulate_until_quiet(&mut reactor);
    let _ = apps.requests();

    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    let _ = apps.requests();
    reactor.handle_event(Event::ApplicationMainWindowChanged(
        pid,
        Some(activated),
        Quiet::No,
    ));

    let outcome = reactor
        .dispatch_workflow(Event::ApplicationActivated(pid, Quiet::No))
        .expect("resolved Dock activation");
    assert!(!outcome.arrange.requested);
    assert!(outcome.layout_events.is_empty());
    assert_eq!(outcome.focused_window, Some(activated));

    reactor.apply_event_outcome(outcome);
    assert_eq!(reactor.layout().focused_window(), Some(activated));
    assert!(
        !apps.requests().is_empty(),
        "revealing the activated scrolling window should write the adjusted strip layout"
    );
}

#[test]
fn windows_discovered_does_not_reintroduce_inactive_workspace_window() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(2));

    reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
        workspace: WorkspaceSelector::Index(1),
        follow: false,
        window_id: Some(2),
    });
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(1));
    apps.simulate_until_quiet(&mut reactor);

    reactor.discover_test_windows(1, vec![], vec![WindowId::new(1, 1), WindowId::new(1, 2)]);

    assert_eq!(reactor.test_active_workspace_windows(space), vec![
        WindowId::new(1, 2)
    ]);
}

#[test]
fn workspace_query_uses_authoritative_assignment_after_move() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));

    reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
        workspace: WorkspaceSelector::Index(1),
        follow: false,
        window_id: Some(wid.idx.get()),
    });
    apps.simulate_until_quiet(&mut reactor);

    let workspaces = reactor.test_workspace_ids(space);
    let ws1 = workspaces[0];
    let ws2 = workspaces[1];

    assert_eq!(reactor.test_workspace_for_window(space, wid), Some(ws2));

    let queried = reactor.query_workspaces(Some(space));
    assert_eq!(queried[0].window_count, 0);
    assert_eq!(queried[1].window_count, 1);
    assert_eq!(queried[1].windows[0].id, wid);
    assert_eq!(
        reactor.test_workspace_windows(space, ws1),
        Vec::<WindowId>::new()
    );
    assert_eq!(reactor.test_workspace_windows(space, ws2), vec![wid]);
}

#[test]
fn login_screen_refresh_preserves_manual_workspace_assignment() {
    let (mut apps, mut reactor) = test_context();
    let space = SpaceId::new(1);
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let wid1 = WindowId::new(1, 1);
    let wid2 = WindowId::new(1, 2);

    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(space)]));
    make_active_app(&mut apps, &mut reactor, 1, make_windows(2), Some(wid1));

    reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
        workspace: WorkspaceSelector::Index(1),
        follow: false,
        window_id: Some(2),
    });
    apps.simulate_until_quiet(&mut reactor);
    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(1));
    apps.simulate_until_quiet(&mut reactor);

    let workspace_before = reactor
        .test_workspace_for_window(space, wid2)
        .expect("window should be assigned to workspace 2 before login refresh");
    let other_workspace_before = reactor
        .test_workspace_for_window(space, wid1)
        .expect("window should remain assigned to original workspace before login refresh");
    assert_ne!(workspace_before, other_workspace_before);
    assert_eq!(
        reactor.test_active_workspace_windows(space),
        vec![wid2],
        "switched workspace should show only the moved window before login refresh"
    );

    reactor.handle_event(space_state_event(vec![CGRect::ZERO], vec![None]));
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(space)]));
    simulate_login_screen_refresh(&mut apps, &mut reactor, 1);

    assert_eq!(
        reactor.test_workspace_for_window(space, wid2),
        Some(workspace_before),
        "login refresh must preserve the moved window's workspace assignment"
    );
    assert_eq!(
        reactor.test_workspace_for_window(space, wid1),
        Some(other_workspace_before),
        "login refresh must preserve other windows' original workspace assignments"
    );
    assert_eq!(
        reactor.test_active_workspace_windows(space),
        vec![wid2],
        "active workspace contents must survive login refresh"
    );
}

#[test]
fn non_active_workspace_windows_remain_hidden_even_if_frame_no_longer_matches_corner_geometry() {
    let (mut apps, mut reactor) = test_context_with_workspace_count(2);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));

    let wsid = reactor.test_window_server_id(wid);
    let workspaces = reactor.test_workspace_ids(space);
    let inactive_workspace = workspaces[0];
    let active_workspace = workspaces[1];

    assert!(reactor.set_test_active_workspace(space, active_workspace));
    assert!(reactor.assign_test_window_to_workspace(space, wid, inactive_workspace));

    if let Some(window) = reactor.windows_mut().window_mut(wid) {
        window.frame_monotonic = CGRect::new(CGPoint::new(200.0, 200.0), CGSize::new(400.0, 400.0));
    }

    assert_eq!(
        reactor.affinity().hidden_assigned_space_for_window_id(wid),
        Some(space),
        "workspace-hidden status should follow Rini's workspace assignment, not stale corner geometry"
    );
    assert_eq!(
        reactor.affinity().geometry_space_for_window(
            &CGRect::new(CGPoint::new(200.0, 200.0), CGSize::new(400.0, 400.0)),
            Some(wsid),
        ),
        Some(space),
        "topology changes can leave hidden windows at stale coordinates; they must still resolve to their assigned space"
    );
}

#[test]
fn display_churn_end_refresh_preserves_non_default_workspace_without_app_rules() {
    let (mut apps, mut reactor) = test_context_with_workspace_count(2);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));

    let workspaces = reactor.test_workspace_ids(space);
    let default_workspace = workspaces[0];
    let secondary_workspace = workspaces[1];

    assert!(reactor.assign_test_window_to_workspace(space, wid, secondary_workspace));
    assert!(reactor.set_test_active_workspace(space, secondary_workspace));
    reactor.discover_test_windows(1, vec![], vec![wid]);

    assert_eq!(
        reactor.test_workspace_for_window(space, wid),
        Some(secondary_workspace)
    );
    assert_ne!(secondary_workspace, default_workspace);
    assert!(has_window_in_layout(&mut reactor, space, screen, wid));

    reactor.handle_event(Event::DisplayChurnBegin);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(
        reactor.test_workspace_for_window(space, wid),
        Some(secondary_workspace),
        "visibility refresh must preserve an existing non-default assignment when no app rule matches"
    );
    assert_eq!(
        reactor.layout().active_workspace(space),
        Some(secondary_workspace),
        "refresh must not switch the active workspace back to default"
    );
    assert!(
        has_window_in_layout(&mut reactor, space, screen, wid),
        "window should remain in the visible layout of its non-default workspace after refresh"
    );
}

#[test]
fn partial_post_wake_snapshot_preserves_manual_workspace_assignment() {
    let (mut apps, mut reactor) = test_context_with_workspace_count(2);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let kept = WindowId::new(1, 1);
    let omitted = WindowId::new(1, 2);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(2));

    let secondary_workspace = reactor.test_workspace(space, 1);
    assert!(reactor.assign_test_window_to_workspace(space, omitted, secondary_workspace));

    reactor.handle_event(Event::SystemWillSleep);
    reactor.handle_event(Event::SystemWoke);

    let mut fresh_state =
        forwarded_space_state(make_screen_snapshots(vec![screen], vec![Some(space)]));
    fresh_state.releases_lifecycle_refresh_quarantine = true;
    fresh_state
        .active_window_spaces
        .insert(WindowServerId::new(kept.idx.get()), space);
    reactor.handle_event(Event::SpaceStateChanged(fresh_state));

    assert_eq!(
        reactor.test_workspace_for_window(space, omitted),
        Some(secondary_workspace),
        "a partial recovery snapshot must not erase a manual workspace assignment"
    );

    reactor.discover_test_windows(1, vec![], vec![kept, omitted]);

    assert_eq!(
        reactor.test_workspace_for_window(space, omitted),
        Some(secondary_workspace),
        "post-wake discovery without an app rule must retain the manual workspace"
    );
}

#[test]
fn wsid_rekey_preserves_non_default_workspace_without_app_rules() {
    let (mut apps, mut reactor) = test_context_with_workspace_count(2);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let old_wid = WindowId::new(1, 1);
    let new_wid = WindowId::new(1, 99);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));

    let workspaces = reactor.test_workspace_ids(space);
    let secondary_workspace = workspaces[1];

    assert!(reactor.assign_test_window_to_workspace(space, old_wid, secondary_workspace));
    assert!(reactor.set_test_active_workspace(space, secondary_workspace));

    rekey_window(&mut reactor, old_wid, new_wid);

    assert_eq!(
        reactor.test_workspace_for_window(space, new_wid),
        Some(secondary_workspace),
        "AX id churn for the same WindowServer window must preserve its workspace assignment"
    );
    assert_eq!(
        reactor
            .layout()
            .virtual_workspace_manager()
            .workspace_info_for_window_any(reactor.windows(), old_wid),
        None,
        "old AX window id should relinquish its assignment after rekey"
    );
}

/// Each display switches workspaces independently, and a workspace is the SAME workspace on
/// every display.
///
/// This is the core of the restructure. Previously each display got its own set of workspace
/// objects, so "coding" on the built-in and "coding" on the external were unrelated; moving a
/// window between displays had to guess a target by ordinal, and an unplug scattered windows
/// into whichever workspace shared an index.
#[test]
fn displays_share_workspaces_but_switch_between_them_independently() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let external = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let builtin_space = SpaceId::new(1);
    let external_space = SpaceId::new(479);

    set_space_membership(&reactor, &[(builtin_space, &[]), (external_space, &[])]);
    reactor.handle_event(space_state_event(vec![builtin, external], vec![
        Some(builtin_space),
        Some(external_space),
    ]));

    let builtin_workspaces = reactor.test_workspace_ids(builtin_space);
    let external_workspaces = reactor.test_workspace_ids(external_space);
    assert_eq!(
        builtin_workspaces, external_workspaces,
        "both displays must see one shared workspace list, not a private copy each"
    );

    // Move only the external to workspace 3.
    reactor.handle_event(space_state_event(vec![builtin, external], vec![
        Some(builtin_space),
        Some(external_space),
    ]));
    assert!(reactor.set_test_active_workspace(external_space, external_workspaces[3]));

    assert_eq!(
        reactor.layout().active_workspace(external_space),
        Some(external_workspaces[3]),
        "the external follows the switch"
    );
    assert_eq!(
        reactor.layout().active_workspace(builtin_space),
        Some(builtin_workspaces[0]),
        "and the built-in is unaffected: displays switch independently"
    );
}

/// A window keeps its workspace when its display's native space id changes.
///
/// macOS mints a new space id on every reconnect. Windows macOS had already moved to the
/// incoming id used to be silently dropped, because the remap replaced the target's window
/// set instead of merging into it.
#[test]
fn reconnect_under_a_new_space_id_keeps_every_windows_workspace() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let old_space = SpaceId::new(479);
    let new_space = SpaceId::new(552);
    let window = WindowId::new(1, 1);

    set_space_membership(&reactor, &[(old_space, &[901])]);
    reactor.handle_event(space_state_event(vec![builtin], vec![Some(old_space)]));
    reactor.add_test_app(1);
    let workspace = reactor.test_workspace(old_space, 2);
    assert!(reactor.set_test_active_workspace(old_space, workspace));
    reactor.add_test_window(window, WindowServerId::new(901), Some(old_space), builtin);
    assert!(reactor.assign_test_window_to_workspace(old_space, window, workspace));
    reactor.send_layout_event(LayoutEvent::WindowAdded(old_space, window));

    reactor.remap_test_space(old_space, new_space);

    let landed = reactor
        .windows()
        .workspace_info_for_window(window)
        .expect("the window keeps an assignment across the id change");
    assert_eq!(landed.space, new_space);
    assert_eq!(
        landed.workspace_id, workspace,
        "and it is the SAME workspace: a new space id is not a new workspace"
    );
    assert_eq!(
        reactor.layout().active_workspace(new_space),
        Some(workspace),
        "the display carries on showing what it was showing"
    );
}

/// What a commit or a tap asked to focus.
fn focus_target(outcome: &EventOutcome) -> Option<WindowId> {
    outcome.raise_requests.iter().find_map(|request| match request {
        crate::windows::domain::raise::Event::RaiseRequest(request) => {
            request.focus_window.map(|(window, _)| window)
        }
        _ => None,
    })
}

/// Focus `window` the way a real focus change arrives: the layout hears it, the window server
/// reports it, and it becomes the window a switch treats as current.
fn focus_reported(reactor: &mut Reactor, space: SpaceId, window: WindowId) {
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, window));
    reactor.handle_event(Event::WindowServerFocusChanged(window, space));
    reactor.set_test_focus(window);
}

/// One display holding `windows`, each on the workspace at its index, focused in `focus` order so the
/// last one is current.
fn switch_desk(windows: &[(WindowId, usize)], focus: &[WindowId]) -> (Reactor, SpaceId) {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);
    let wsids: Vec<u32> = (0..windows.len() as u32).map(|at| 901 + at).collect();
    set_space_membership(&reactor, &[(space, &wsids)]);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    let mut pids: Vec<rini_core::ids::pid_t> =
        windows.iter().map(|(window, _)| window.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    for pid in pids {
        reactor.add_test_app(pid);
    }
    let workspaces = reactor.test_workspace_ids(space);
    for ((window, at), wsid) in windows.iter().zip(&wsids) {
        reactor.add_test_window(*window, WindowServerId::new(*wsid), Some(space), screen);
        assert!(reactor.assign_test_window_to_workspace(space, *window, workspaces[*at]));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, *window));
    }
    for window in focus {
        focus_reported(&mut reactor, space, *window);
    }
    (reactor, space)
}

/// The every-window switcher steps between APPLICATIONS, most recently used first, each one a stack of
/// its windows across every workspace.
///
/// Both halves are asserted: the flat list spans two apps and two workspaces in the order they were
/// focused rather than the order they were added, and the stacks group it by application.
#[test]
fn the_every_window_switch_offers_one_stack_per_application_in_focus_order() {
    let ghostty = WindowId::new(1, 1);
    let other_ghostty = WindowId::new(1, 2);
    let slack = WindowId::new(2, 1);
    let (mut reactor, _) = switch_desk(&[(ghostty, 0), (other_ghostty, 1), (slack, 0)], &[
        other_ghostty,
        slack,
        ghostty,
    ]);

    let list = reactor.probe_switch_candidates(SwitchScope::Everything);
    assert_eq!(
        list.iter().map(|c| c.window).collect::<Vec<_>>(),
        vec![ghostty, slack, other_ghostty],
        "every window, both apps, both workspaces, most recently focused first"
    );

    let stacks = reactor.probe_switch_entries(SwitchScope::Everything);
    assert_eq!(stacks.len(), 2, "one entry per application");
    assert_eq!(stacks[0].front().window, ghostty);
    assert_eq!(
        stacks[0].behind().iter().map(|c| c.window).collect::<Vec<_>>(),
        vec![other_ghostty],
        "the window on the other workspace is in its application's stack"
    );
    assert_eq!(stacks[1].front().window, slack);
    assert_eq!(stacks[1].count(), 1);

    assert_eq!(
        focus_target(&reactor.probe_switch_window(false, SwitchScope::Everything)),
        Some(slack),
        "a single step goes to the previous application, not to Ghostty's other window"
    );
}

/// A held every-window switch moves a stack at a time: two steps past an application with three
/// windows land on the third application, not on the first one's third window.
#[test]
fn a_held_every_window_switch_steps_between_applications_not_windows() {
    use crate::input::domain::switch_session::Signal;

    let (a1, a2, a3) = (WindowId::new(1, 1), WindowId::new(1, 2), WindowId::new(1, 3));
    let (b, c) = (WindowId::new(2, 1), WindowId::new(3, 1));
    let (mut reactor, _) =
        switch_desk(&[(a1, 0), (a2, 0), (a3, 0), (b, 0), (c, 0)], &[c, a2, a3, b, a1]);

    reactor.dispatch_test_switch(Signal::Open {
        backward: false,
        scope: SwitchScope::Everything,
    });
    reactor.dispatch_test_switch(Signal::Step(1));
    let committed = reactor.dispatch_test_switch(Signal::Commit);

    assert_eq!(focus_target(&committed), Some(c));
}

/// Choosing a stack focuses its application's most recent window, wherever it is, and the display
/// follows to that window's workspace.
#[test]
fn a_stack_commits_its_applications_most_recent_window_wherever_it_is() {
    let older = WindowId::new(1, 1);
    let recent = WindowId::new(1, 2);
    let current = WindowId::new(2, 1);
    let current_other = WindowId::new(2, 2);
    let (mut reactor, space) =
        switch_desk(&[(older, 0), (recent, 1), (current, 0), (current_other, 0)], &[
            older,
            recent,
            current_other,
            current,
        ]);
    let workspaces = reactor.test_workspace_ids(space);
    assert!(reactor.set_test_active_workspace(space, workspaces[0]));

    let outcome = reactor.probe_switch_window(false, SwitchScope::Everything);

    assert_eq!(
        focus_target(&outcome),
        Some(recent),
        "not the current application's other window, and not the older window beside us"
    );
    assert_eq!(
        reactor.test_active_workspace(space),
        Some(workspaces[1]),
        "the display shows the workspace of the window focus landed on"
    );
}

/// Minimising takes a window off its workspace, so it is not offered: its application's stack goes
/// without it, and an application whose windows are all minimised has no stack.
#[test]
fn a_minimised_window_is_not_offered() {
    let current = WindowId::new(1, 1);
    let visible = WindowId::new(2, 1);
    let minimised = WindowId::new(2, 2);
    let (hidden, other_hidden) = (WindowId::new(3, 1), WindowId::new(3, 2));
    let (mut reactor, _) = switch_desk(
        &[
            (current, 0),
            (visible, 0),
            (minimised, 0),
            (hidden, 0),
            (other_hidden, 1),
        ],
        &[hidden, other_hidden, visible, minimised, current],
    );
    for window in [minimised, hidden, other_hidden] {
        reactor.handle_event(Event::WindowMinimized(window));
    }

    assert_eq!(
        reactor
            .probe_switch_candidates(SwitchScope::Everything)
            .iter()
            .map(|c| c.window)
            .collect::<Vec<_>>(),
        vec![current, visible]
    );
    let stacks = reactor.probe_switch_entries(SwitchScope::Everything);
    assert_eq!(
        stacks.len(),
        2,
        "no stack for the application with every window minimised"
    );
    assert_eq!(stacks[1].front().window, visible);
    assert_eq!(
        stacks[1].detail(),
        "1 window",
        "the minimised window is not counted"
    );
    assert_eq!(
        focus_target(&reactor.probe_switch_window(false, SwitchScope::Everything)),
        Some(visible)
    );
}

/// A stack is made of the windows the switch offers, so an application with none of them gets no
/// stack: neither one with no window at all, which the Dock's switcher lists, nor one whose only window
/// rini has not placed on a workspace.
#[test]
fn an_application_with_no_offered_window_gets_no_stack() {
    let current = WindowId::new(1, 1);
    let other = WindowId::new(2, 1);
    let unplaced = WindowId::new(4, 1);
    let (mut reactor, space) = switch_desk(&[(current, 0), (other, 0)], &[other, current]);
    reactor.add_test_app(3);
    reactor.add_test_app(4);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    reactor.add_test_window(unplaced, WindowServerId::new(950), Some(space), screen);
    assert!(reactor.windows().workspace_info_for_window(unplaced).is_none());

    let stacks = reactor.probe_switch_entries(SwitchScope::Everything);
    assert_eq!(
        stacks.iter().map(|entry| entry.front().window.pid).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

/// A tap steps from the stack holding the focused window, whichever of its windows that is. With
/// recency stale, as when focus moved without a report, the focused window can sit behind its stack's
/// top one, and stepping from the top would mistake the stack for some other.
#[test]
fn a_tap_steps_from_the_stack_holding_the_focused_window() {
    let b = WindowId::new(2, 1);
    let (a_top, a_focused) = (WindowId::new(1, 1), WindowId::new(1, 2));
    let (mut reactor, _) =
        switch_desk(&[(a_top, 0), (a_focused, 0), (b, 0)], &[a_focused, a_top, b]);
    reactor.set_test_focus(a_focused);
    let stacks = reactor.probe_switch_entries(SwitchScope::Everything);
    assert_eq!(stacks[0].front().window, b);
    assert_eq!(stacks[1].front().window, a_top);

    assert_eq!(
        focus_target(&reactor.probe_switch_window(false, SwitchScope::Everything)),
        Some(b),
        "the step leaves the focused window's stack and wraps to b, rather than landing on a_top"
    );
}

/// The popup is sent one row per application: its top window, at most two behind it, and the count.
#[test]
fn the_popup_draws_one_stack_per_application() {
    use crate::input::domain::switch_session::Signal;
    use crate::switcher::platform::actor;

    let ghostty: Vec<WindowId> = (1..=4).map(|idx| WindowId::new(1, idx)).collect();
    let slack = WindowId::new(2, 1);
    let mut windows: Vec<(WindowId, usize)> = ghostty.iter().map(|window| (*window, 0)).collect();
    windows.push((slack, 0));
    let (mut reactor, _) = switch_desk(&windows, &[
        ghostty[3], ghostty[2], slack, ghostty[1], ghostty[0],
    ]);
    let mut popup = reactor.connect_test_switcher();

    reactor.dispatch_test_switch(Signal::Open {
        backward: false,
        scope: SwitchScope::Everything,
    });

    let mut drawn = None;
    while let Ok((_, event)) = popup.try_recv() {
        if let actor::Event::Show { rows, selected, .. } = event {
            drawn = Some((rows, selected));
        }
    }
    let (rows, selected) = drawn.expect("the open draws the switch");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].window, ghostty[0]);
    assert_eq!(
        rows[0].behind,
        vec![ghostty[1], ghostty[2]],
        "two behind, nearest first"
    );
    assert_eq!(rows[0].detail, "4 windows");
    assert_eq!(rows[1].window, slack);
    assert!(rows[1].behind.is_empty());
    assert_eq!(rows[1].detail, "1 window");
    assert_eq!(selected, 1, "the previous application");
}

/// With one application's windows open the every-window switch has one entry, so a hold draws nothing
/// and a tap stays where it is.
#[test]
fn a_switch_with_one_entry_opens_no_popup() {
    use crate::input::domain::switch_session::Signal;

    let (first, second) = (WindowId::new(1, 1), WindowId::new(1, 2));
    let (mut reactor, _) = switch_desk(&[(first, 0), (second, 1)], &[second, first]);
    let mut popup = reactor.connect_test_switcher();

    reactor.dispatch_test_switch(Signal::Open {
        backward: false,
        scope: SwitchScope::Everything,
    });
    let committed = reactor.dispatch_test_switch(Signal::Commit);

    let mut shown = false;
    while let Ok((_, event)) = popup.try_recv() {
        shown |= matches!(event, crate::switcher::platform::actor::Event::Show { .. });
    }
    assert!(!shown);
    assert_eq!(focus_target(&committed), None);
    assert_eq!(
        focus_target(&reactor.probe_switch_window(false, SwitchScope::Everything)),
        None
    );
}

/// The pictures read and asked for are the cards drawn: each stack's top window and the two behind it,
/// never a window the stack stands for but does not draw.
#[test]
fn a_switch_reads_and_warms_only_the_windows_it_draws() {
    use crate::input::domain::switch_session::Signal;

    let ghostty: Vec<WindowId> = (1..=4).map(|idx| WindowId::new(1, idx)).collect();
    let slack = WindowId::new(2, 1);
    let mut windows: Vec<(WindowId, usize)> = ghostty.iter().map(|window| (*window, 0)).collect();
    windows.push((slack, 0));
    let (mut reactor, _) = switch_desk(&windows, &[
        ghostty[3], ghostty[2], slack, ghostty[1], ghostty[0],
    ]);
    let mut popup = reactor.connect_test_switcher();
    let mut pictures = reactor.connect_test_pictures();

    reactor.dispatch_test_switch(Signal::Open {
        backward: false,
        scope: SwitchScope::Everything,
    });

    let mut read = Vec::new();
    while let Ok((_, event)) = popup.try_recv() {
        if let crate::switcher::platform::actor::Event::Show { rows, .. } = event {
            read.extend(crate::switcher::platform::panel::drawn_windows(&rows));
        }
    }
    let warmed = asked_at(
        &pictures_asked(&mut pictures),
        crate::pictures::domain::schedule::Priority::Warm,
    );
    let drawn = vec![ghostty[0], ghostty[1], ghostty[2], slack];
    assert_eq!(read, drawn);
    assert_eq!(warmed, drawn);
}

/// The workspace switcher is unchanged: one entry per window, by recency, so a tap reaches the other
/// window of the application beside you, which the every-window switch steps over.
#[test]
fn a_workspace_switch_stays_one_entry_per_window() {
    let editor = WindowId::new(1, 1);
    let other_editor = WindowId::new(1, 2);
    let elsewhere = WindowId::new(1, 3);
    let slack = WindowId::new(2, 1);
    let (mut reactor, _) =
        switch_desk(&[(editor, 0), (other_editor, 0), (elsewhere, 1), (slack, 0)], &[
            slack,
            other_editor,
            editor,
        ]);

    let entries = reactor.probe_switch_entries(SwitchScope::Workspace);
    assert_eq!(
        entries.iter().map(|entry| entry.front().window).collect::<Vec<_>>(),
        vec![editor, other_editor, slack]
    );
    assert!(entries.iter().all(|entry| entry.count() == 1));
    assert_eq!(
        focus_target(&reactor.probe_switch_window(false, SwitchScope::Workspace)),
        Some(other_editor)
    );
    assert_eq!(
        focus_target(&reactor.probe_switch_window(false, SwitchScope::Everything)),
        Some(slack)
    );
}

/// The workspace switcher follows the WORKSPACE, not the display: its windows on the other display
/// are offered, and another workspace's windows on this display are not.
#[test]
fn a_workspace_switch_offers_that_workspace_on_every_display() {
    let mut reactor = test_reactor();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let right = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1920., 1080.));
    let (left_space, right_space) = (SpaceId::new(1), SpaceId::new(2));
    let here = WindowId::new(1, 1);
    let there = WindowId::new(2, 1);
    let other_workspace = WindowId::new(1, 2);

    set_space_membership(&reactor, &[(left_space, &[901, 903]), (right_space, &[902])]);
    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(left_space),
        Some(right_space),
    ]));
    reactor.add_test_app(1);
    reactor.add_test_app(2);
    let workspaces = reactor.test_workspace_ids(left_space);
    for (window, wsid, space, screen, workspace) in [
        (here, 901u32, left_space, left, workspaces[0]),
        (there, 902, right_space, right, workspaces[0]),
        (other_workspace, 903, left_space, left, workspaces[1]),
    ] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), screen);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspace));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
    }
    reactor.send_layout_event(LayoutEvent::WindowFocused(left_space, here));
    reactor.handle_event(Event::WindowServerFocusChanged(here, left_space));
    reactor.set_test_focus(here);

    let mut offered: Vec<WindowId> = reactor
        .probe_switch_candidates(SwitchScope::Workspace)
        .iter()
        .map(|c| c.window)
        .collect();
    offered.sort();
    assert_eq!(
        offered,
        vec![here, there],
        "both displays' share of the focused workspace, and nothing from the other workspace"
    );
}

/// The reported case: the laptop showing ws2 and the external showing ws1, focus on a ws2 window on the
/// laptop. The workspace switch is a hard filter on ws2: its windows on BOTH displays, including one
/// parked on the external, and none of ws1's — whether ws1 is showing on the external or parked on the
/// laptop.
#[test]
fn a_workspace_switch_is_a_hard_filter_whatever_the_other_display_shows() {
    let mut reactor = test_reactor();
    let laptop = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let external = CGRect::new(CGPoint::new(-670., -1692.), CGSize::new(3440., 1440.));
    let (laptop_space, external_space) = (SpaceId::new(1), SpaceId::new(449));
    let editor = WindowId::new(1, 1); // ws2, laptop, focused
    let chrome = WindowId::new(2, 1); // ws1, laptop, parked
    let slack = WindowId::new(3, 1); // ws1, external, showing
    let terminal = WindowId::new(4, 1); // ws2, external, parked

    set_space_membership(&reactor, &[
        (laptop_space, &[901, 902]),
        (external_space, &[903, 904]),
    ]);
    reactor.handle_event(space_state_event(vec![laptop, external], vec![
        Some(laptop_space),
        Some(external_space),
    ]));
    for pid in [1, 2, 3, 4] {
        reactor.add_test_app(pid);
    }
    let ws = reactor.test_workspace_ids(laptop_space);
    let (ws1, ws2) = (ws[0], ws[1]);
    for (window, wsid, space, screen, workspace) in [
        (editor, 901u32, laptop_space, laptop, ws2),
        (chrome, 902, laptop_space, laptop, ws1),
        (slack, 903, external_space, external, ws1),
        (terminal, 904, external_space, external, ws2),
    ] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), screen);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspace));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
    }
    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(1));
    for window in [slack, chrome, terminal, editor] {
        let space = if window == slack || window == terminal {
            external_space
        } else {
            laptop_space
        };
        reactor.handle_event(Event::WindowServerFocusChanged(window, space));
    }
    reactor.set_test_focus(editor);
    assert_eq!(
        reactor.test_workspace_for_window(laptop_space, editor),
        Some(ws2)
    );

    let mut offered: Vec<WindowId> = reactor
        .probe_switch_candidates(SwitchScope::Workspace)
        .iter()
        .map(|c| c.window)
        .collect();
    offered.sort();
    assert_eq!(
        offered,
        vec![editor, terminal],
        "ws2 on both displays, and nothing of ws1"
    );
}

/// The application switcher offers the focused application's windows and nothing else, wherever
/// they are, and a held switch over it commits within them.
#[test]
fn an_app_switch_stays_inside_the_focused_application() {
    use crate::input::domain::switch_session::Signal;

    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);
    let meeting = WindowId::new(1, 1);
    let controls = WindowId::new(1, 2);
    let slack = WindowId::new(2, 1);

    set_space_membership(&reactor, &[(space, &[901, 902, 903])]);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.add_test_app(1);
    reactor.add_test_app(2);
    let workspace = reactor.test_workspace(space, 0);
    for (window, wsid) in [(meeting, 901u32), (controls, 902), (slack, 903)] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), screen);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspace));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
    }
    // Slack last-but-one, so it is where an every-window switch would go.
    for window in [controls, slack, meeting] {
        reactor.send_layout_event(LayoutEvent::WindowFocused(space, window));
        reactor.handle_event(Event::WindowServerFocusChanged(window, space));
    }
    reactor.set_test_focus(meeting);

    assert_eq!(
        reactor
            .probe_switch_candidates(SwitchScope::App)
            .iter()
            .map(|c| c.window)
            .collect::<Vec<_>>(),
        vec![meeting, controls],
        "the current window first, then the rest of its application"
    );

    reactor.dispatch_test_switch(Signal::Open {
        backward: false,
        scope: SwitchScope::App,
    });
    let committed = reactor.dispatch_test_switch(Signal::Commit);
    assert_eq!(
        focus_target(&committed),
        Some(controls),
        "not Slack: another application is outside the scope"
    );
}

/// With nothing focused there is no application to switch within, so the switch does nothing rather
/// than guessing one.
#[test]
fn an_app_switch_with_nothing_focused_offers_nothing() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);
    set_space_membership(&reactor, &[(space, &[901])]);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.add_test_app(1);
    let window = WindowId::new(1, 1);
    reactor.add_test_window(window, WindowServerId::new(901), Some(space), screen);
    let workspace = reactor.test_workspace(space, 0);
    assert!(reactor.assign_test_window_to_workspace(space, window, workspace));

    assert!(reactor.probe_switch_candidates(SwitchScope::App).is_empty());
    let outcome = reactor.probe_switch_window(false, SwitchScope::App);
    assert!(outcome.raise_requests.is_empty());
}

/// A held session steps a selection and focuses ONCE, on the release.
///
/// Focusing as the selection moved would raise every window it passed over — a burst of Accessibility
/// work and a visible flicker through windows the user never asked to see. The signals arrive from the
/// input thread; nothing but the commit touches a window.
#[test]
fn a_held_switch_focuses_only_on_the_commit() {
    use crate::input::domain::switch_session::Signal;

    let first = WindowId::new(1, 1);
    let second = WindowId::new(2, 1);
    let third = WindowId::new(3, 1);
    // Focus order: third (oldest), second, first (current).
    let (mut reactor, _) =
        switch_desk(&[(first, 0), (second, 0), (third, 0)], &[third, second, first]);

    // Open selects the second entry, and one step moves to the third. Neither touches a window.
    let opened = reactor.dispatch_test_switch(Signal::Open {
        backward: false,
        scope: SwitchScope::Everything,
    });
    assert_eq!(focus_target(&opened), None, "opening a switch focuses nothing");
    let stepped = reactor.dispatch_test_switch(Signal::Step(1));
    assert_eq!(focus_target(&stepped), None, "stepping focuses nothing");

    let committed = reactor.dispatch_test_switch(Signal::Commit);
    assert_eq!(
        focus_target(&committed),
        Some(third),
        "the release focuses the selection, once: second entry plus one step"
    );
}

/// A click on a row commits THAT row's window, whatever the keyboard had selected, and only while a
/// switch is open. A window the switch does not offer commits nothing rather than something nearby.
#[test]
fn a_click_on_a_row_commits_that_window() {
    use crate::input::domain::switch_session::Signal;

    let first = WindowId::new(1, 1);
    let second = WindowId::new(2, 1);
    let third = WindowId::new(3, 1);
    let (mut reactor, _) =
        switch_desk(&[(first, 0), (second, 0), (third, 0)], &[third, second, first]);

    assert_eq!(
        focus_target(&reactor.dispatch_test_pick(third)),
        None,
        "no switch open: a click has nothing to commit"
    );

    reactor.dispatch_test_switch(Signal::Open {
        backward: false,
        scope: SwitchScope::Everything,
    });
    assert_eq!(
        focus_target(&reactor.dispatch_test_pick(WindowId::new(9, 9))),
        None,
        "a window the switch does not offer commits nothing"
    );
    assert_eq!(
        focus_target(&reactor.dispatch_test_pick(third)),
        Some(third),
        "the keyboard had the second entry selected; the click wins"
    );
    assert_eq!(
        focus_target(&reactor.dispatch_test_switch(Signal::Commit)),
        None,
        "and the modifier's release afterwards has nothing left to commit"
    );
}

/// The panel hands over the window a row is drawn for, which for a stack is its top window. A window
/// drawn behind is no row of its own.
#[test]
fn a_click_on_a_stack_commits_its_top_window() {
    use crate::input::domain::switch_session::Signal;

    let (a1, a2) = (WindowId::new(1, 1), WindowId::new(1, 2));
    let (b1, b2) = (WindowId::new(2, 1), WindowId::new(2, 2));
    let (mut reactor, _) = switch_desk(&[(a1, 0), (a2, 0), (b1, 0), (b2, 0)], &[b2, a2, b1, a1]);

    reactor.dispatch_test_switch(Signal::Open {
        backward: false,
        scope: SwitchScope::Everything,
    });
    assert_eq!(
        focus_target(&reactor.dispatch_test_pick(b2)),
        None,
        "b2 is drawn behind b1, and no row is drawn for it"
    );
    assert_eq!(focus_target(&reactor.dispatch_test_pick(b1)), Some(b1));
}

/// Escape leaves everything where it was.
#[test]
fn a_cancelled_switch_focuses_nothing() {
    use crate::input::domain::switch_session::Signal;

    let first = WindowId::new(1, 1);
    let second = WindowId::new(2, 1);
    let (mut reactor, _) = switch_desk(&[(first, 0), (second, 0)], &[second, first]);

    reactor.dispatch_test_switch(Signal::Open {
        backward: false,
        scope: SwitchScope::Everything,
    });
    assert!(reactor.switch_is_open(), "two applications: the switch opened");
    reactor.dispatch_test_switch(Signal::Cancel);
    let after = reactor.dispatch_test_switch(Signal::Commit);

    assert!(
        after.raise_requests.is_empty(),
        "a commit after a cancel has nothing to commit"
    );
}

/// Three Ghostty windows, two sharing a workspace and one on another: "i have three ghostty windows
/// between different displays/workspaces and i can only swap between the two on the same workspace".
fn three_ghosttys() -> (Reactor, SpaceId, [WindowId; 3]) {
    let first = WindowId::new(1, 1);
    let second = WindowId::new(1, 2);
    let elsewhere = WindowId::new(1, 3);
    let (mut reactor, space) = switch_desk(&[(first, 0), (second, 0), (elsewhere, 1)], &[
        elsewhere, second, first,
    ]);
    let workspaces = reactor.test_workspace_ids(space);
    assert!(reactor.set_test_active_workspace(space, workspaces[0]));
    (reactor, space, [first, second, elsewhere])
}

/// cmd-` goes by recency like the other switchers: quick taps toggle between the two most recent
/// windows rather than moving on through the rest.
#[test]
fn an_app_switch_quick_tap_toggles_the_two_most_recent() {
    let (mut reactor, space, [first, second, _]) = three_ghosttys();

    let tapped = focus_target(&reactor.probe_switch_window(false, SwitchScope::App));
    assert_eq!(tapped, Some(second));
    focus_reported(&mut reactor, space, second);

    assert_eq!(
        focus_target(&reactor.probe_switch_window(false, SwitchScope::App)),
        Some(first),
        "back to the first, not on to the third"
    );
}

/// The window on another workspace is offered, two steps into a held switch, and reaching it brings
/// the display along — or focus would sit on a window parked off-screen and the key would look dead.
#[test]
fn an_app_switch_reaches_a_window_on_another_workspace_with_a_hold() {
    use crate::input::domain::switch_session::Signal;

    let (mut reactor, space, [_, second, elsewhere]) = three_ghosttys();
    assert_eq!(
        focus_target(&reactor.probe_switch_window(false, SwitchScope::App)),
        Some(second),
        "a quick tap goes to the previous window"
    );

    reactor.dispatch_test_switch(Signal::Open {
        backward: false,
        scope: SwitchScope::App,
    });
    reactor.dispatch_test_switch(Signal::Step(1));
    let committed = reactor.dispatch_test_switch(Signal::Commit);

    assert_eq!(focus_target(&committed), Some(elsewhere));
    assert_eq!(
        reactor.test_workspace_for_window(space, elsewhere),
        reactor.test_active_workspace(space),
        "the display must be showing the workspace of the window focus landed on"
    );
}

/// Moving a window to another workspace keeps the width the user gave it.
///
/// Adding a window to a workspace creates a FRESH column at the default ratio, so a window
/// sized to a third or two thirds snapped back on every move. Reported as the size resetting
/// to 50%. Asserts the laid-out width, which is what is actually visible.
#[test]
fn moving_a_window_between_workspaces_keeps_its_column_width() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);
    let sized = WindowId::new(1, 1);
    let neighbour = WindowId::new(1, 2);

    set_space_membership(&reactor, &[(space, &[901, 902])]);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.add_test_app(1);
    let workspaces = reactor.test_workspace_ids(space);
    for (window, wsid) in [(sized, 901u32), (neighbour, 902)] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), screen);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspaces[0]));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
    }

    // Two equal columns to start with. Cycle the focused one to a different preset width.
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, sized));
    let default_width = laid_out_frame(&mut reactor, space, screen, sized)
        .expect("window is laid out")
        .size
        .width;
    reactor.handle_test_layout_command(LayoutCommand::CyclePresetColumnWidth);
    let resized_width = laid_out_frame(&mut reactor, space, screen, sized)
        .expect("window is laid out")
        .size
        .width;
    assert_ne!(
        resized_width.round(),
        default_width.round(),
        "test setup must actually change the column width"
    );

    reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
        workspace: rini_ipc::protocol::WorkspaceSelector::Index(1),
        follow: true,
        window_id: None,
    });

    let moved_width = laid_out_frame(&mut reactor, space, screen, sized)
        .expect("window is laid out after the move")
        .size
        .width;
    assert_eq!(
        moved_width.round(),
        resized_width.round(),
        "the window must arrive with the width it had, not the workspace default"
    );
}

/// A window's width must not change because its destination workspace is emptier.
///
/// The exact reported repro: a full-size window moved from workspace 1 to 2 to 3 on ONE
/// display went half-size on 2 (which held other windows) and full again on 3 (which was
/// empty). Two causes, both fixed:
///   - a lone column used to be rendered at the full viewport width, so "alone" and
///     "deliberately full width" were indistinguishable, and
///   - the full-width MODE was dropped by `remove_window` on the way out of a tree, with
///     nothing recording that the user had asked for it.
///
/// Width is now remembered per DISPLAY, so it survives any number of workspace hops. The
/// window here is alone on workspace 3 and shares workspace 2, which is what made the old
/// behaviour flip back and forth.
#[test]
fn a_full_width_window_stays_full_width_across_workspaces() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);
    let sized = WindowId::new(1, 1);
    let neighbour = WindowId::new(1, 2);

    set_space_membership(&reactor, &[(space, &[901, 902])]);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.add_test_app(1);
    let workspaces = reactor.test_workspace_ids(space);
    // The neighbour lives on workspace 2, so that workspace is POPULATED while workspace 3
    // stays empty — the asymmetry that produced the bug.
    for (window, wsid, workspace) in [
        (sized, 901u32, workspaces[0]),
        (neighbour, 902, workspaces[1]),
    ] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), screen);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspace));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
    }

    reactor.send_layout_event(LayoutEvent::WindowFocused(space, sized));
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreenWithinGaps);
    let full_width = laid_out_frame(&mut reactor, space, screen, sized)
        .expect("window is laid out")
        .size
        .width;

    for target in [1usize, 2] {
        reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
            workspace: rini_ipc::protocol::WorkspaceSelector::Index(target),
            follow: true,
            window_id: None,
        });
        let width = laid_out_frame(&mut reactor, space, screen, sized)
            .unwrap_or_else(|| panic!("window is laid out on workspace {target}"))
            .size
            .width;
        assert_eq!(
            width.round(),
            full_width.round(),
            "workspace {target} changed the width of a full-size window"
        );
    }
}

/// Every workspace has its own strip, so the offsets of two of them are not comparable. Keyed by space
/// alone, the first pass after a switch read the destination's offset as a movement of the difference
/// between the two and panned sideways after the vertical slide had already landed.
#[test]
fn a_workspace_switch_is_not_a_strip_movement() {
    let (mut apps, mut reactor) = test_context_with_workspace_count(2);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    // Enough columns that the strip is longer than the display and therefore scrolled.
    apps.make_app_and_settle(&mut reactor, 1, make_windows(6));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(1, 6)));

    let first = reactor.take_strip_movement(space);
    assert_eq!(
        first,
        Some(CGPoint::new(0., 0.)),
        "the first look at a strip reports no movement"
    );
    let settled = reactor.take_strip_movement(space);
    assert_eq!(
        settled,
        Some(CGPoint::new(0., 0.)),
        "and a strip that has not moved reports none"
    );

    reactor.handle_test_layout_command(LayoutCommand::NextWorkspace(None));

    assert_eq!(
        reactor.take_strip_movement(space),
        Some(CGPoint::new(0., 0.)),
        "arriving on another workspace is a vertical switch, not a horizontal pan"
    );
}

/// `reactor` just started on one display, with two windows on the workspace it shows, laid out and
/// settled with the flight engine listening from here on: what the first switch after start meets.
fn started(
    mut reactor: Reactor,
) -> (
    Apps,
    Reactor,
    crate::app::channels::Receiver<crate::transitions::platform::engine::Event>,
) {
    let mut apps = Apps::new();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    reactor.handle_event(space_state_event(vec![screen], vec![Some(SpaceId::new(1))]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    apps.requests();
    reactor.settings_mut().animate = true;
    let animation_rx = reactor.connect_test_transitions();
    (apps, reactor, animation_rx)
}

/// A reactor started on a layout saved showing the workspace at `showing` of four, restored the way a
/// launch restores it.
fn restored_showing(showing: usize) -> Reactor {
    let mut settings = crate::app::config::VirtualWorkspaceSettings::default();
    settings.workspace_names = crate::workspaces::settings::numbered_names(4);
    let layout = crate::app::config::LayoutSettings::default();
    let space = SpaceId::new(1);
    let mut memory = DisplayMemory::default();
    let mut saved = LayoutEngine::new(&settings, &layout);
    let _ = saved.handle_event(
        &mut crate::workspaces::domain::window_store::WindowStore::default(),
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1728., 1117.)),
    );
    let workspace = saved.virtual_workspace_manager_mut().list_workspaces(space)[showing].0;
    saved.virtual_workspace_manager_mut().set_active_workspace(space, workspace);
    let path = std::env::temp_dir().join(format!(
        "rini-first-switch-test-{}-{showing}.ron",
        std::process::id()
    ));
    saved.save(&memory, path.clone()).unwrap();
    let (mut restored, _) =
        LayoutEngine::load_for_startup_restore(path.clone(), |_, _| false, |_| false).unwrap();
    let _ = std::fs::remove_file(path);
    restored.finish_loading(&settings, &layout);
    Reactor::new_for_test(restored)
}

/// Whether any of `requests` writes a window's frame.
fn writes_a_frame(requests: &[Request]) -> bool {
    requests.iter().any(|request| {
        matches!(
            request,
            Request::SetWindowFrame(..)
                | Request::SetBatchWindowFrame(..)
                | Request::SetWorkspaceSwitchPositions(..)
        )
    })
}

/// The first switch after rini starts slides like every other, where it cut: the origin of a switch
/// was written only by a switch, so the first one on a space had none. "The vertical workspace
/// switch" in `specs/animation.md`.
#[test]
fn the_first_switch_after_start_slides() {
    let (mut apps, mut reactor, mut rx) = started(test_reactor_with_workspace_count(4));
    let pitch = reactor.space_state().screens[0].frame.size.height;

    reactor.handle_test_layout_command(LayoutCommand::NextWorkspace(None));

    let slides = strip_movements(&mut rx);
    assert_eq!(slides.len(), 1, "one slide down the stack");
    assert_eq!(
        (slides[0].from_offset, slides[0].to_offset),
        (SurfacePoint::origin(), SurfacePoint::new(0., pitch)),
        "from the first workspace to the second"
    );
    let requests = apps.requests();
    assert!(
        !writes_a_frame(&requests),
        "the slide places the windows, not the switch: {requests:?}"
    );
}

/// A switch reads Low Power Mode from the reactor's reads, so in it the switch cuts rather than
/// slides.
#[test]
fn in_low_power_mode_a_workspace_switch_does_not_slide() {
    let (_apps, mut reactor, mut rx) = started(test_reactor_with_workspace_count(4));

    reactor.reads_mut().low_power = || true;
    reactor.handle_test_layout_command(LayoutCommand::NextWorkspace(None));
    assert!(strip_movements(&mut rx).is_empty(), "no slide in Low Power Mode");

    reactor.reads_mut().low_power = || false;
    reactor.handle_test_layout_command(LayoutCommand::PrevWorkspace(None));
    assert_eq!(
        strip_movements(&mut rx).len(),
        1,
        "the switch back slides out of it"
    );
}

/// A switch claims the destination strip's layout with its origin, so the next pan starts a parked
/// window where the switch drew it, not where an older pass on that workspace last had it.
#[test]
fn a_switch_claims_the_destination_strip_s_layout() {
    let (mut apps, mut reactor, _rx) = started(test_reactor_with_workspace_count(4));
    let space = SpaceId::new(1);
    reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
        workspace: rini_ipc::protocol::WorkspaceSelector::Index(1),
        follow: false,
        window_id: Some(2),
    });
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_test_layout_command(LayoutCommand::NextWorkspace(None));

    let (claimed, now) = reactor.take_strip_layout(space).expect("a strip");
    let claimed = claimed.expect("the switch drew the destination");
    assert!(
        claimed.iter().any(|(window, _)| *window == WindowId::new(1, 2)),
        "{claimed:?}"
    );
    assert_eq!(claimed, now, "as the strip has it now");
}

/// A jump across the stack draws every workspace between the two, each one pitch below the one
/// above it, and lands on the destination's row.
#[test]
fn a_jump_across_the_stack_draws_each_workspace_one_row_below_the_last() {
    let (mut apps, mut reactor, mut rx) = started(test_reactor_with_workspace_count(4));
    let pitch = reactor.space_state().screens[0].frame.size.height;
    reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
        workspace: rini_ipc::protocol::WorkspaceSelector::Index(2),
        follow: false,
        window_id: Some(2),
    });
    apps.simulate_until_quiet(&mut reactor);
    strip_movements(&mut rx);

    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(2));

    let slides = strip_movements(&mut rx);
    assert_eq!(slides.len(), 1, "one slide down the stack");
    let row = |window: WindowId| {
        let drawn = slides[0].windows.iter().find(|drawn| drawn.window == window).unwrap();
        (drawn.frame.origin.y / pitch).floor()
    };
    assert_eq!((row(WindowId::new(1, 1)), row(WindowId::new(1, 2))), (0., 2.));
    assert_eq!(
        slides[0].to_offset,
        SurfacePoint::new(0., 2. * pitch),
        "two rows down"
    );
}

/// A switch reads the display's bounds once for its stacked rows and the out-of-view check, so
/// both measure the same display even if it is reconfigured mid-switch, and once more where it
/// publishes the display its flight is drawn on.
#[test]
fn a_switch_reads_the_display_bounds_once_for_its_rows() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static READS: AtomicUsize = AtomicUsize::new(0);
    let (mut apps, mut reactor, mut rx) = started(test_reactor_with_workspace_count(4));
    reactor.reads_mut().display_bounds = |_| {
        READS.fetch_add(1, Ordering::SeqCst);
        CGRect::ZERO
    };
    reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
        workspace: rini_ipc::protocol::WorkspaceSelector::Index(2),
        follow: false,
        window_id: Some(2),
    });
    apps.simulate_until_quiet(&mut reactor);
    strip_movements(&mut rx);
    READS.store(0, Ordering::SeqCst);

    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(2));

    assert_eq!(strip_movements(&mut rx).len(), 1, "one slide down the stack");
    assert_eq!(
        READS.load(Ordering::SeqCst),
        2,
        "the rows', then the flight's display"
    );
}

/// A restored layout shows the workspace it was saved on, and the first switch slides from that one.
#[test]
fn a_restored_layout_s_first_switch_slides_from_the_workspace_it_opened_on() {
    let (mut apps, mut reactor, mut rx) = started(restored_showing(2));
    let space = SpaceId::new(1);
    assert_eq!(
        reactor.test_active_workspace(space),
        Some(reactor.test_workspace(space, 2))
    );
    let pitch = reactor.space_state().screens[0].frame.size.height;

    reactor.handle_test_layout_command(LayoutCommand::PrevWorkspace(None));

    let slides = strip_movements(&mut rx);
    assert_eq!(slides.len(), 1, "one slide up the stack");
    assert_eq!(
        (slides[0].from_offset, slides[0].to_offset),
        (SurfacePoint::new(0., pitch), SurfacePoint::origin()),
        "from the third workspace to the second"
    );
    let requests = apps.requests();
    assert!(
        !writes_a_frame(&requests),
        "the slide places the windows, not the switch: {requests:?}"
    );
}

mod query_view {
    use test_log::test;

    use super::*;

    /// A query is a read. Asking about a space the engine has never seen must not create its
    /// default workspaces as a side effect, which `list_workspaces` used to do.
    #[test]
    fn querying_an_unknown_space_creates_nothing() {
        let reactor = test_reactor();
        let never_seen = SpaceId::new(4242);
        assert!(reactor.query_workspaces(Some(never_seen)).is_empty());
        assert!(reactor.layout().virtual_workspace_manager().existing_workspaces().is_empty());
    }
}
