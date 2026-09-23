//! Workspaces: creating, switching, moving windows between them, app rules, and queries.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::WindowServerId;
use rini_geometry::SameAs;
use test_log::test;

use super::fixtures::*;
use crate::app::config::WorkspaceSelector;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
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

    // Windows hang off COLUMNS, one level down.
    //
    // This test was written when the default layout was Traditional, whose container tree
    // puts windows directly under the root, so it asserted on `children[N].window_id`. The
    // tree-based layouts are gone and scrolling is the only mode left: its top-level children
    // are columns (`window_id: None`, `role: "column"`) and the windows sit inside them. The
    // old assertions compared `selected_window` against a column's absent id and read
    // `None`, which is the structure being correct rather than a defect.
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
        reactor.space_state.active_spaces,
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
        reactor.layout_manager.layout_engine.active_workspace(right_space),
        Some(right_workspace)
    );

    reactor.handle_test_layout_command(LayoutCommand::NextWorkspace(None));

    assert_eq!(
        reactor.layout_manager.layout_engine.active_workspace(left_space),
        Some(next_left_workspace),
        "workspace commands should follow the active display space"
    );
    assert_eq!(
        reactor.layout_manager.layout_engine.active_workspace(right_space),
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
    let (mut reactor, wid, wsid, space1, space2, _moved_frame) =
        reactor_with_window_moved_to_space2();
    let conflicting_frame = CGRect::new(CGPoint::new(100., 100.), CGSize::new(800., 600.));

    reactor
        .state
        .windows
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
    assert!(crate::app::reactor::animation::AnimationManager::instant_layout(
        &mut reactor,
        space,
        &[(wid, target)],
        None,
    ));

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
    assert!(
        crate::app::reactor::animation::AnimationManager::workspace_switch_layout(
            &mut reactor,
            space,
            &[(wid, target)],
            None,
        )
    );

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

    if let Some(window) = reactor.state.windows.window_mut(wid) {
        window.frame_monotonic = CGRect::new(CGPoint::new(200.0, 200.0), CGSize::new(400.0, 400.0));
    }

    let gaps = reactor.config.settings.layout.gaps.clone();
    let hidden_target = reactor
        .layout_manager
        .layout_engine
        .calculate_layout_with_virtual_workspaces(
            &reactor.state.windows,
            space,
            screen,
            &gaps,
            |query_wid| {
                reactor.state.windows.window(query_wid).map(|window| window.frame_monotonic)
            },
            &[screen],
        )
        .into_iter()
        .find(|(layout_wid, _)| *layout_wid == wid)
        .map(|(_, frame)| frame)
        .expect("inactive-workspace window should still be laid out to a hidden position");

    let txid = reactor.transaction_manager.generate_next_txid(wsid);
    reactor.transaction_manager.store_txid(wsid, txid, hidden_target);

    assert!(!reactor.update_layout_or_warn(false, true, None));
    assert!(
        apps.requests().is_empty(),
        "a stale pending target suppresses the hide write before topology invalidation"
    );

    reactor.handle_event(space_state_event_with(
        vec![screen],
        vec![Some(space)],
        |state| {
            state.has_seen_display_set = true;
            state.display_set_changed = true;
            state.topology_changed = true;
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
    let (raise_manager_tx, mut raise_manager_rx) = channels::channel();
    reactor.communication_manager.raise_manager_tx = raise_manager_tx;

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
        reactor.layout_manager.layout_engine.is_window_in_active_workspace(
            &reactor.state.windows,
            space,
            same_app_visible
        ),
        "another window from the activated app should remain visible on the current workspace"
    );
    reactor.handle_event(Event::ApplicationGloballyActivated(activated.pid));
    assert_eq!(reactor.main_window(), Some(activated));
    assert_eq!(
        reactor.layout_manager.layout_engine.active_workspace_idx(space),
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
    assert_eq!(
        reactor.layout_manager.layout_engine.focused_window(),
        Some(activated)
    );
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

    reactor.handle_test_layout_command(LayoutCommand::CreateWorkspace);
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

    if let Some(window) = reactor.state.windows.window_mut(wid) {
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

    reactor.handle_event(Event::DisplayChurnEnd);
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(
        reactor.test_workspace_for_window(space, wid),
        Some(secondary_workspace),
        "visibility refresh must preserve an existing non-default assignment when no app rule matches"
    );
    assert_eq!(
        reactor.layout_manager.layout_engine.active_workspace(space),
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
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_info_for_window_any(&reactor.state.windows, old_wid),
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

    set_space_membership(&[(builtin_space, &[]), (external_space, &[])]);
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
        reactor.layout_manager.layout_engine.active_workspace(external_space),
        Some(external_workspaces[3]),
        "the external follows the switch"
    );
    assert_eq!(
        reactor.layout_manager.layout_engine.active_workspace(builtin_space),
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

    set_space_membership(&[(old_space, &[901])]);
    reactor.handle_event(space_state_event(vec![builtin], vec![Some(old_space)]));
    reactor.add_test_app(1);
    let workspace = reactor.test_workspace(old_space, 2);
    assert!(reactor.set_test_active_workspace(old_space, workspace));
    reactor.add_test_window(window, WindowServerId::new(901), Some(old_space), builtin);
    assert!(reactor.assign_test_window_to_workspace(old_space, window, workspace));
    reactor.send_layout_event(LayoutEvent::WindowAdded(old_space, window));

    reactor.layout_manager.layout_engine.remap_space(
        &mut reactor.state.windows,
        &mut reactor.state.display_memory,
        old_space,
        new_space,
    );

    let landed = reactor
        .state
        .windows
        .workspace_info_for_window(window)
        .expect("the window keeps an assignment across the id change");
    assert_eq!(landed.space, new_space);
    assert_eq!(
        landed.workspace_id, workspace,
        "and it is the SAME workspace: a new space id is not a new workspace"
    );
    assert_eq!(
        reactor.layout_manager.layout_engine.active_workspace(new_space),
        Some(workspace),
        "the display carries on showing what it was showing"
    );
}

/// Moving a window to another workspace keeps the width the user gave it.
///
/// Adding a window to a workspace creates a FRESH column at the default ratio, so a window
/// sized to a third or two thirds snapped back on every move. Reported as the size resetting
/// to 50%. Asserts the laid-out width, which is what is actually visible.
/// Cycling an app's windows must reach the ones on OTHER workspaces.
///
/// macOS's cmd-` only offers windows on the visible workspace, so three Ghostty windows
/// split across two workspaces cycled between the two that shared one: "i have three
/// ghostty windows between different displays/workspaces and i can only swap between the
/// two on the same workspace". rini knows where all of them are, so CycleAppWindows rotates
/// through every one and switches the display's workspace to follow.
#[test]
fn cycling_app_windows_reaches_every_workspace() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);
    let first = WindowId::new(1, 1);
    let second = WindowId::new(1, 2);
    let elsewhere = WindowId::new(1, 3);

    set_space_membership(&[(space, &[901, 902, 903])]);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.add_test_app(1);
    let workspaces = reactor.test_workspace_ids(space);
    // Two windows share workspace 0; the third sits on workspace 1, which is the one macOS
    // could never reach.
    for (window, wsid, workspace) in [
        (first, 901u32, workspaces[0]),
        (second, 902, workspaces[0]),
        (elsewhere, 903, workspaces[1]),
    ] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), screen);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspace));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
    }
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, first));
    reactor.set_test_focus(first);

    // Rotate through the whole set. Focus is normally applied by the raise manager, which
    // does not run under test, so read the requested target out of the outcome and apply it
    // by hand — otherwise every iteration rotates from the same starting point.
    let mut visited = vec![first];
    for _ in 0..3 {
        let outcome = reactor.probe_cycle_app_windows(false);
        let target = outcome.raise_requests.iter().find_map(|request| match request {
            crate::windows::domain::raise::Event::RaiseRequest(request) => {
                request.focus_window.map(|(window, _)| window)
            }
            _ => None,
        });
        let Some(target) = target else { break };
        visited.push(target);
        reactor.set_test_focus(target);
        reactor.send_layout_event(LayoutEvent::WindowFocused(space, target));
    }

    assert!(
        visited.contains(&elsewhere),
        "the window on another workspace must be reachable; visited {visited:?}"
    );
    assert!(
        visited.contains(&second),
        "the window sharing the workspace must still be reachable; visited {visited:?}"
    );
    // And reaching it must have brought the display along, or focus would sit on a window
    // parked off-screen and the keystroke would look like it did nothing.
    assert_eq!(
        reactor.test_workspace_for_window(space, *visited.last().expect("non-empty")),
        reactor.test_active_workspace(space),
        "the display must be showing the workspace of the window focus landed on"
    );
}

#[test]
fn moving_a_window_between_workspaces_keeps_its_column_width() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);
    let sized = WindowId::new(1, 1);
    let neighbour = WindowId::new(1, 2);

    set_space_membership(&[(space, &[901, 902])]);
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

    set_space_membership(&[(space, &[901, 902])]);
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
        assert!(reactor.query_workspace_layouts(Some(never_seen), None).is_empty());
        assert!(
            reactor
                .layout_manager
                .layout_engine
                .virtual_workspace_manager()
                .existing_workspaces(never_seen)
                .is_empty()
        );
    }
}
