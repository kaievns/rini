use objc2_core_foundation::{CGPoint, CGSize};
use test_log::test;

use super::testing::*;
use super::*;
use crate::actor::app::{AppThreadHandle, Request, pid_t};
use crate::actor::wm_controller::WmEvent;
use crate::common::config::{LayoutMode, OuterGaps, WorkspaceSelector};
use crate::layout_engine::{Direction, LayoutCommand, LayoutEvent};
use crate::model::window_store::NativeFullscreenTransition;
use crate::sys::app::{AppInfo, WindowInfo};
use crate::sys::geometry::SameAs;
use crate::sys::window_server::WindowServerId;

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
        rini_protocol::ContainerNodeType::Container
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
    let windows: Vec<&rini_protocol::ContainerTreeNode> = state
        .container_tree
        .children
        .iter()
        .flat_map(|column| column.children.iter())
        .collect();
    assert!(
        state.container_tree.children.iter().all(|node| node.window_id.is_none()
            && node.role.as_deref() == Some("column")),
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
fn config_reload_propagates_non_keybinding_changes_to_wm_controller() {
    let mut reactor = test_reactor();
    let (wm_tx, mut wm_rx) = actor::channel();
    reactor.communication_manager.wm_sender = Some(wm_tx);

    let mut updated = reactor.config.clone();
    updated.settings.focus_follows_mouse = !updated.settings.focus_follows_mouse;
    updated.settings.mouse_follows_focus = !updated.settings.mouse_follows_focus;
    updated.settings.mouse_hides_on_focus = !updated.settings.mouse_hides_on_focus;

    reactor.handle_event(Event::ConfigUpdated(updated.clone()));

    let (_, event) = wm_rx.try_recv().expect("config update should reach wm controller");
    let WmEvent::ConfigUpdated(actual) = event else {
        panic!("expected config update, got {event:?}");
    };
    assert_eq!(
        actual.settings.focus_follows_mouse,
        updated.settings.focus_follows_mouse
    );
    assert_eq!(
        actual.settings.mouse_follows_focus,
        updated.settings.mouse_follows_focus
    );
    assert_eq!(
        actual.settings.mouse_hides_on_focus,
        updated.settings.mouse_hides_on_focus
    );
}

#[test]
fn it_ignores_stale_resize_events() {
    let (mut apps, mut reactor) = test_context();
    reactor.handle_event(space_state_event(
        vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
        vec![Some(SpaceId::new(1))],
    ));

    reactor.handle_events(apps.make_app(1, make_windows(2)));
    let requests = apps.requests();
    assert!(!requests.is_empty());
    let events_1 = apps.simulate_events_for_requests(requests);

    reactor.handle_events(apps.make_app(2, make_windows(2)));
    assert!(!apps.requests().is_empty());

    for event in dbg!(events_1) {
        reactor.handle_event(event);
    }
    let requests = apps.requests();
    assert!(
        requests.is_empty(),
        "got requests when there should have been none: {requests:?}"
    );
}

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
fn it_manages_windows_on_enabled_spaces() {
    let (mut apps, mut reactor) = test_context();
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(SpaceId::new(1))]));

    reactor.handle_events(apps.make_app(1, make_windows(1)));

    let _events = apps.simulate_events();
    // Tiled onto the space, at the configured column width rather than the whole screen: a
    // lone column no longer expands to fill the viewport, because that made a window's size
    // depend on how many neighbours its workspace held. What this test is actually about is
    // that the window got managed at all, so it asserts placement, not full width.
    let frame = apps.windows.get(&WindowId::new(1, 1)).expect("Window was not resized").frame;
    assert_eq!(frame.origin, full_screen.origin);
    assert_eq!(frame.size.height, full_screen.size.height);
    assert!(
        frame.size.width > 0.0 && frame.size.width <= full_screen.size.width,
        "window must be tiled within the screen, got {frame:?}"
    );
}

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
fn forwarded_space_snapshot_respects_default_disable_policy() {
    let mut reactor = test_reactor();
    reactor.config.settings.default_disable = true;

    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));

    assert!(
        !reactor.is_space_active(space),
        "forwarded raw active spaces must still be filtered by default_disable policy"
    );
}

#[test]
fn forwarded_space_snapshot_respects_one_space_policy() {
    let mut reactor = test_reactor();
    reactor.one_space = true;

    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(space1), Some(space2)],
    ));

    assert!(reactor.is_space_active(space1));
    assert!(
        !reactor.is_space_active(space2),
        "forwarded raw active spaces must not bypass one_space filtering"
    );
}

#[test]
fn forwarded_space_snapshot_respects_toggled_space_activation_policy() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    assert!(reactor.is_space_active(space));

    reactor.handle_event(Event::Command(Command::Reactor(
        ReactorCommand::ToggleSpaceActivated,
    )));
    assert!(!reactor.is_space_active(space));

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));

    assert!(
        !reactor.is_space_active(space),
        "forwarded raw active spaces must not re-enable a space disabled by ToggleSpaceActivated"
    );
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

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(left_space), Some(right_space)],
    ));

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

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(left_space), Some(right_space)],
    ));

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
fn command_space_only_snapshot_does_not_trigger_full_space_reconcile() {
    let (mut apps, mut reactor) = test_context();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);

    reactor.handle_event(space_state_event_with(
        vec![left, right],
        vec![Some(space1), Some(space2)],
        |state| state.has_seen_display_set = true,
    ));

    apps.make_app_and_settle(&mut reactor, 1, make_windows(1));
    assert!(apps.requests().is_empty());

    reactor.handle_event(space_state_event_with(
        vec![left, right],
        vec![Some(space1), Some(space2)],
        |state| {
            state.has_seen_display_set = true;
            state.menu_bar_space = Some(space2);
            state.command_space = Some(space2);
        },
    ));

    assert_eq!(reactor.workspace_command_space(), Some(space2));
    assert!(
        apps.requests().is_empty(),
        "changing only command_space should not trigger visible-window refresh or space reconciliation"
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

#[test]
fn passive_command_space_change_does_not_override_clicked_window_focus() {
    let (mut apps, mut reactor) = test_context();
    let (raise_manager_tx, mut raise_manager_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_manager_tx;

    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let left_space = SpaceId::new(1);
    let right_space = SpaceId::new(2);
    reactor.handle_event(space_state_event_with(
        vec![left, right],
        vec![Some(left_space), Some(right_space)],
        |state| state.has_seen_display_set = true,
    ));

    let mut windows = make_windows(2);
    windows[1].frame.origin = CGPoint::new(1100., 100.);
    reactor.handle_event(Event::ApplicationGloballyActivated(1));
    reactor.handle_events(apps.make_app_with_opts(
        1,
        windows,
        Some(WindowId::new(1, 1)),
        true,
        true,
    ));
    apps.simulate_until_quiet(&mut reactor);

    let old_focus = WindowId::new(1, 1);
    let destination_focus = WindowId::new(1, 2);
    reactor.send_layout_event(LayoutEvent::WindowFocused(right_space, destination_focus));
    reactor.send_layout_event(LayoutEvent::WindowFocused(left_space, old_focus));
    while raise_manager_rx.try_recv().is_ok() {}

    reactor.handle_event(space_state_event_with(
        vec![left, right],
        vec![Some(left_space), Some(right_space)],
        |state| {
            state.has_seen_display_set = true;
            state.menu_bar_space = Some(right_space);
            state.command_space = Some(right_space);
        },
    ));

    assert_eq!(
        reactor.layout_manager.layout_engine.focused_window(),
        Some(old_focus),
        "a passive display snapshot must leave focus ownership to the AX click event"
    );
    assert!(
        raise_manager_rx.try_recv().is_err(),
        "a passive active-display change must not raise the workspace's stale selection"
    );

    reactor.handle_event(Event::ApplicationMainWindowChanged(
        1,
        Some(destination_focus),
        Quiet::No,
    ));
    assert_eq!(
        reactor.layout_manager.layout_engine.focused_window(),
        Some(destination_focus),
        "the subsequent AX focus event should select the window that activated the display"
    );
}

#[test]
fn discovery_does_not_replay_another_apps_global_main_window() {
    let (mut apps, mut reactor) = test_context();
    let space = SpaceId::new(1);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));

    reactor.handle_event(Event::ApplicationGloballyActivated(1));
    reactor.handle_events(apps.make_app_with_opts(
        1,
        make_windows(1),
        Some(WindowId::new(1, 1)),
        true,
        true,
    ));
    reactor.handle_events(apps.make_app_with_opts(2, make_windows(1), None, false, true));
    apps.simulate_until_quiet(&mut reactor);

    let app_two_window = WindowId::new(2, 1);
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, app_two_window));
    let info = reactor
        .state
        .windows
        .window(app_two_window)
        .expect("app two window should be tracked")
        .info
        .clone();

    reactor.discover_test_windows(2, vec![(app_two_window, info)], vec![app_two_window]);

    assert_eq!(reactor.main_window(), Some(WindowId::new(1, 1)));
    assert_eq!(
        reactor.layout_manager.layout_engine.focused_window(),
        Some(app_two_window),
        "app-scoped discovery must not replay another app's global main window"
    );
}

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
fn queries_prefer_authoritative_active_space_over_stale_command_space() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space1)]));
    reactor.handle_test_workspace_command(space1, &LayoutCommand::SwitchToWorkspace(0));
    reactor.handle_test_workspace_command(space2, &LayoutCommand::SwitchToWorkspace(1));

    reactor.handle_event(space_state_event_with(
        vec![screen],
        vec![Some(space2)],
        |state| state.command_space = Some(space1),
    ));

    assert_eq!(
        reactor.query_active_workspace(None),
        reactor.layout_manager.layout_engine.active_workspace(space2),
        "default queries must follow authoritative active space state, not stale command_space"
    );
}

#[test]
fn menu_bar_space_prefers_active_menu_bar_display_space() {
    let mut reactor = test_reactor();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(space1), Some(space2)],
    ));

    assert_eq!(reactor.test_default_query_space(), Some(space1));
    assert_eq!(
        reactor.test_resolve_menu_bar_space_with_preferred(Some(space2)),
        Some(space2),
        "menubar updates should follow the display currently hosting the menu bar"
    );
}

#[test]
fn menu_bar_space_falls_back_when_preferred_space_is_not_visible() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let visible_space = SpaceId::new(1);
    let hidden_space = SpaceId::new(2);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(visible_space)]));

    assert_eq!(
        reactor.test_resolve_menu_bar_space_with_preferred(Some(hidden_space)),
        Some(visible_space),
        "menubar updates should fall back to the normal active context if the preferred menubar space is unavailable"
    );
}

#[test]
fn workspace_queries_are_isolated_per_macos_space() {
    let mut reactor = test_reactor();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(space1), Some(space2)],
    ));

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
fn best_space_prefers_authoritative_window_server_space_over_geometry() {
    let mut reactor = test_reactor();
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    let wid = WindowId::new(1, 1);
    let wsid = WindowServerId::new(11);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(space2)]));
    reactor.insert_test_window(wid, wsid, Some(space1), frame, true);

    assert_eq!(reactor.best_space_for_window_id(wid), Some(space1));
}

#[test]
fn user_space_window_server_events_preserve_hidden_window_state() {
    let mut reactor = test_reactor();
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let wid = WindowId::new(1, 1);
    let wsid = WindowServerId::new(21);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(space1)]));
    reactor.insert_test_window(wid, wsid, Some(space1), frame, true);

    crate::sys::window_server::set_window_ordered_in_override(wsid, Some(true));
    window_server_destroyed(&mut reactor, wsid, space1, SpaceEventKind::User);
    crate::sys::window_server::set_window_ordered_in_override(wsid, None);

    assert!(reactor.state.windows.contains_window(wid));
    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space1));
    assert!(!reactor.state.windows.is_window_visible(wsid));
}

#[test]
fn user_space_window_server_destroyed_removes_window_when_window_server_is_gone() {
    let mut reactor = test_reactor();
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let wid = WindowId::new(1, 1);
    let wsid = WindowServerId::new(22);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(space1)]));
    reactor.insert_test_window(wid, wsid, Some(space1), frame, true);
    reactor.state.windows.mark_window_visible(wsid);

    crate::sys::window_server::set_window_ordered_in_override(wsid, Some(false));
    window_server_destroyed(&mut reactor, wsid, space1, SpaceEventKind::User);
    crate::sys::window_server::set_window_ordered_in_override(wsid, None);

    assert!(!reactor.state.windows.contains_window(wid));
    assert_eq!(reactor.state.windows.tracked_window_id(wsid), None);
    assert_eq!(reactor.assigned_space_for_window_id(wid), None);
}

/// Builds a reactor with `space1` active on a screen and a single tiled window
/// (`wid`/`wsid`) assigned to `space1`. `space2` exists with workspaces so it can
/// be a reassignment target. Returns the pieces the `appeared` tests need.
fn reactor_with_window_on_space1() -> (Reactor, WindowId, WindowServerId, SpaceId, SpaceId, CGRect)
{
    let mut reactor = test_reactor();
    let pid = 1;
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    let wid = WindowId::new(pid, 1);
    let wsid = WindowServerId::new(101);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(space1)]));

    reactor.add_test_app(pid);

    let space1_workspace = reactor.test_workspace(space1, 0);
    let _ = reactor.test_workspace_ids(space2);

    reactor.add_test_window(wid, wsid, Some(space1), frame);

    assert!(reactor.assign_test_window_to_workspace(space1, wid, space1_workspace));
    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space1));

    (reactor, wid, wsid, space1, space2, frame)
}

fn reactor_with_window_moved_to_space2()
-> (Reactor, WindowId, WindowServerId, SpaceId, SpaceId, CGRect) {
    let mut reactor = test_reactor();
    let pid = 1;
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let screen2 = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let moved_frame = CGRect::new(CGPoint::new(1600., 100.), CGSize::new(800., 600.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    let wid = WindowId::new(pid, 1);
    let wsid = WindowServerId::new(111);

    reactor.handle_event(space_state_event(
        vec![screen1, screen2],
        vec![Some(space1), Some(space2)],
    ));

    reactor.add_test_app(pid);

    let space1_workspace = reactor.test_workspace(space1, 0);
    let space2_workspace = reactor.test_workspace(space2, 0);

    reactor.add_test_window(wid, wsid, Some(space2), moved_frame);

    assert!(reactor.assign_test_window_to_workspace(space1, wid, space1_workspace));
    assert!(reactor.assign_test_window_to_workspace(space2, wid, space2_workspace));
    let txid = reactor.transaction_manager.generate_next_txid(wsid);
    reactor.transaction_manager.store_txid(wsid, txid, moved_frame);
    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space2));

    (reactor, wid, wsid, space1, space2, moved_frame)
}

fn reactor_with_window_on_space1_two_displays() -> (
    Reactor,
    WindowId,
    WindowServerId,
    SpaceId,
    SpaceId,
    CGRect,
    CGRect,
) {
    let mut reactor = test_reactor();
    let pid = 1;
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let screen2 = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let initial_frame = CGRect::new(CGPoint::new(100., 100.), CGSize::new(800., 600.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    let wid = WindowId::new(pid, 1);
    let wsid = WindowServerId::new(121);

    reactor.handle_event(space_state_event(
        vec![screen1, screen2],
        vec![Some(space1), Some(space2)],
    ));

    reactor.add_test_app(pid);

    let space1_workspace = reactor.test_workspace(space1, 0);
    let _ = reactor.test_workspace_ids(space2);

    reactor.add_test_window(wid, wsid, Some(space1), initial_frame);

    assert!(reactor.assign_test_window_to_workspace(space1, wid, space1_workspace));

    (reactor, wid, wsid, space1, space2, initial_frame, screen2)
}

fn reactor_with_floating_window() -> (Reactor, WindowId, SpaceId, CGRect, CGRect) {
    let (mut reactor, wid, _wsid, space1, _space2, screen) = reactor_with_window_on_space1();
    reactor.send_layout_event(LayoutEvent::WindowAdded(space1, wid));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space1, wid));
    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    assert!(reactor.layout_manager.layout_engine.is_window_floating(wid));

    let workspace = reactor
        .layout_manager
        .layout_engine
        .active_workspace(space1)
        .expect("workspace");
    let floating_frame = CGRect::new(CGPoint::new(100., 100.), CGSize::new(400., 300.));
    if let Some(w) = reactor.state.windows.window_mut(wid) {
        w.frame_monotonic = floating_frame;
    }
    reactor.layout_manager.layout_engine.store_floating_position(
        space1,
        workspace,
        wid,
        floating_frame,
    );

    (reactor, wid, space1, screen, floating_frame)
}

fn window_server_appeared(
    reactor: &mut Reactor,
    wsid: WindowServerId,
    space: SpaceId,
    kind: SpaceEventKind,
) {
    SpaceEventHandler::handle_window_server_appeared(reactor, wsid, space, kind);
}

fn window_server_destroyed(
    reactor: &mut Reactor,
    wsid: WindowServerId,
    space: SpaceId,
    kind: SpaceEventKind,
) {
    SpaceEventHandler::handle_window_server_destroyed(
        reactor,
        SpaceEventHandler::WindowServerLifecyclePayload {
            window_server_id: wsid,
            space,
            kind,
        },
    )
    .unwrap();
}

#[test]
fn appeared_reassigns_window_without_pending_rini_move() {
    let (mut reactor, wid, wsid, space1, space2, _frame) = reactor_with_window_on_space1();

    // No pending transaction: this is a genuine external space change, so Rini should
    // follow it and reassign the window to the reported space.
    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space1));

    window_server_appeared(&mut reactor, wsid, space2, SpaceEventKind::User);

    assert_eq!(
        reactor.assigned_space_for_window_id(wid),
        Some(space2),
        "window without an in-flight Rini move must follow a genuine external space change"
    );
}

#[test]
fn matching_rini_frame_clears_pending_target() {
    let (mut reactor, wid, wsid, _space1, _space2, frame) = reactor_with_window_on_space1();
    let target_frame = CGRect::new(
        CGPoint::new(frame.origin.x + 40.0, frame.origin.y + 25.0),
        frame.size,
    );
    let txid = reactor.transaction_manager.generate_next_txid(wsid);
    reactor.transaction_manager.store_txid(wsid, txid, target_frame);

    reactor.handle_event(Event::WindowFrameChanged(
        wid,
        target_frame,
        Some(txid),
        Requested(true),
        Some(MouseState::Up),
    ));

    assert_eq!(
        reactor.transaction_manager.get_target_frame(wsid),
        None,
        "a confirmed Rini frame must clear the pending target"
    );
    assert!(
        reactor
            .state
            .windows
            .window(wid)
            .expect("window should still exist")
            .frame_monotonic
            .same_as(target_frame)
    );

    // AX may adjust a requested frame; cache the accepted geometry but keep the target pending.
    let adjusted_target = CGRect::new(CGPoint::new(80.0, 40.0), frame.size);
    let accepted = CGRect::new(CGPoint::new(81.0, 40.0), frame.size);
    let txid = reactor.transaction_manager.generate_next_txid(wsid);
    reactor.transaction_manager.store_txid(wsid, txid, adjusted_target);
    let outcome = reactor
        .dispatch_workflow(Event::WindowFrameChanged(
            wid,
            accepted,
            Some(txid),
            Requested(true),
            Some(MouseState::Up),
        ))
        .unwrap();
    assert!(reactor.state.windows.window(wid).unwrap().frame_monotonic.same_as(accepted));
    assert_eq!(
        reactor.transaction_manager.get_target_frame(wsid),
        Some(adjusted_target)
    );
    assert!(!outcome.arrange.requested && !outcome.refresh_layout_mode);

    // A user drag beginning during the transaction clears it instead of accepting it blindly.
    reactor.handle_event(Event::WindowFrameChanged(
        wid,
        accepted,
        Some(txid),
        Requested(true),
        Some(MouseState::Down),
    ));
    assert_eq!(reactor.transaction_manager.get_target_frame(wsid), None);
}

#[test]
fn frame_acknowledgements_and_unchanged_frames_do_not_invalidate_layout() {
    let (mut reactor, wid, wsid, _space1, _space2, frame) = reactor_with_window_on_space1();
    let target_frame = CGRect::new(
        CGPoint::new(frame.origin.x + 40.0, frame.origin.y + 25.0),
        frame.size,
    );
    let txid = reactor.transaction_manager.generate_next_txid(wsid);
    reactor.transaction_manager.store_txid(wsid, txid, target_frame);

    let acknowledgement = reactor
        .dispatch_workflow(Event::WindowFrameChanged(
            wid,
            target_frame,
            Some(txid),
            Requested(true),
            Some(MouseState::Up),
        ))
        .unwrap();
    assert!(!acknowledgement.arrange.requested);
    assert!(!acknowledgement.refresh_layout_mode);

    let unchanged = reactor
        .dispatch_workflow(Event::WindowFrameChanged(
            wid,
            target_frame,
            None,
            Requested(false),
            Some(MouseState::Up),
        ))
        .unwrap();
    assert!(!unchanged.arrange.requested);
    assert!(!unchanged.refresh_layout_mode);

    let explicitly_requested_frame = CGRect::new(
        CGPoint::new(target_frame.origin.x + 10.0, target_frame.origin.y),
        target_frame.size,
    );
    let requested = reactor
        .dispatch_workflow(Event::WindowFrameChanged(
            wid,
            explicitly_requested_frame,
            None,
            Requested(true),
            Some(MouseState::Up),
        ))
        .unwrap();
    assert!(!requested.arrange.requested);
    assert!(!requested.refresh_layout_mode);
}

#[test]
fn genuine_external_frame_changes_invalidate_layout() {
    let (mut reactor, wid, _wsid, _space1, _space2, frame) = reactor_with_window_on_space1();
    let moved_frame = CGRect::new(
        CGPoint::new(frame.origin.x + 40.0, frame.origin.y + 25.0),
        frame.size,
    );

    let outcome = reactor
        .dispatch_workflow(Event::WindowFrameChanged(
            wid,
            moved_frame,
            None,
            Requested(false),
            Some(MouseState::Up),
        ))
        .unwrap();

    assert!(outcome.arrange.requested);
    assert_eq!(outcome.arrange.passes, 1);
    assert!(outcome.refresh_layout_mode);
}

#[test]
fn stale_and_inactive_frame_events_request_no_arrange_passes() {
    let (mut reactor, wid, wsid, _space1, _space2, frame) = reactor_with_window_on_space1();
    let target_frame = CGRect::new(
        CGPoint::new(frame.origin.x + 40.0, frame.origin.y + 25.0),
        frame.size,
    );
    let txid = reactor.transaction_manager.generate_next_txid(wsid);
    reactor.transaction_manager.store_txid(wsid, txid, target_frame);
    let acknowledgement = reactor
        .dispatch_workflow(Event::WindowFrameChanged(
            wid,
            target_frame,
            Some(txid),
            Requested(true),
            Some(MouseState::Up),
        ))
        .unwrap();
    assert!(!acknowledgement.arrange.requested);

    let duplicate = reactor
        .dispatch_workflow(Event::WindowFrameChanged(
            wid,
            target_frame,
            None,
            Requested(false),
            Some(MouseState::Up),
        ))
        .unwrap();
    assert!(!duplicate.arrange.requested);

    // Stale transaction notification while a newer target is pending.
    let current_txid = reactor.transaction_manager.generate_next_txid(wsid);
    reactor.transaction_manager.store_txid(wsid, current_txid, target_frame);
    let stale = reactor
        .dispatch_workflow(Event::WindowFrameChanged(
            wid,
            CGRect::new(
                CGPoint::new(target_frame.origin.x + 20.0, target_frame.origin.y),
                target_frame.size,
            ),
            Some(current_txid.next()),
            Requested(false),
            Some(MouseState::Up),
        ))
        .unwrap();
    assert!(!stale.arrange.requested);

    // Geometry on an inactive native space.
    reactor.transaction_manager.clear_target_for_window(wsid);
    reactor.set_active_spaces(&[]);
    let inactive = reactor
        .dispatch_workflow(Event::WindowFrameChanged(
            wid,
            CGRect::new(
                CGPoint::new(target_frame.origin.x + 30.0, target_frame.origin.y),
                target_frame.size,
            ),
            None,
            Requested(false),
            Some(MouseState::Up),
        ))
        .unwrap();
    assert!(!inactive.arrange.requested);
}

#[test]
fn external_resize_requests_one_arrange_pass() {
    let (mut reactor, wid, _wsid, _space1, _space2, frame) = reactor_with_window_on_space1();
    let resized = CGRect::new(
        frame.origin,
        CGSize::new(frame.size.width + 80.0, frame.size.height + 40.0),
    );

    let outcome = reactor
        .dispatch_workflow(Event::WindowFrameChanged(
            wid,
            resized,
            None,
            Requested(false),
            Some(MouseState::Up),
        ))
        .unwrap();

    assert!(outcome.arrange.requested);
    assert_eq!(outcome.arrange.passes, 1);
    assert!(outcome.arrange.is_resize);
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
        reactor.assigned_space_for_window_id(wid),
        Some(space1),
        "a parked column's frame is not evidence that it changed display"
    );
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
    let outcome = crate::actor::reactor::events::drag::handle_mouse_up(
        &mut reactor.state,
        &mut reactor.layout_manager,
        &mut reactor.drag_manager,
        crate::actor::reactor::events::drag::MouseUpPayload {
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

    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space2));
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
    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space2));
    assert!(reactor.state.windows.is_window_visible(wsid));

    let _ = reactor.reconcile_windows_with_authoritative_spaces();

    assert_eq!(
        reactor.assigned_space_for_window_id(wid),
        Some(space2),
        "late disappearance from the old display must not drag a moved window back"
    );
}

#[test]
fn stale_user_space_appearance_does_not_restore_old_display_assignment() {
    let (mut reactor, wid, wsid, space1, space2, _) = reactor_with_window_moved_to_space2();

    window_server_appeared(&mut reactor, wsid, space1, SpaceEventKind::User);

    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space2));

    let _ = reactor.reconcile_windows_with_authoritative_spaces();

    assert_eq!(
        reactor.assigned_space_for_window_id(wid),
        Some(space2),
        "late appearance on the old display must not overwrite the newer target assignment"
    );
}

#[test]
fn stale_user_space_appearance_is_ignored_when_server_state_already_matches_pending_target() {
    let (mut reactor, wid, wsid, space1, space2, _frame) = reactor_with_window_moved_to_space2();
    let space1_workspace = reactor.test_workspace(space1, 0);

    assert!(reactor.assign_test_window_to_workspace(space1, wid, space1_workspace));
    reactor.state.windows.set_window_server_space(wsid, Some(space1));
    let txid = reactor.transaction_manager.generate_next_txid(wsid);
    let target_frame = CGRect::new(CGPoint::new(100., 100.), CGSize::new(800., 600.));
    reactor.transaction_manager.store_txid(wsid, txid, target_frame);

    window_server_appeared(&mut reactor, wsid, space2, SpaceEventKind::User);

    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space1));
    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space1));
    assert_eq!(
        reactor.authoritative_space_for_window_id(wid),
        Some(space1),
        "late appearance from the old display should be ignored once Rini has already committed the new server-space target"
    );
}

#[test]
fn stale_user_space_appearance_is_ignored_when_authoritative_window_space_differs() {
    let (mut reactor, wid, wsid, space1, space2, _frame) = reactor_with_window_moved_to_space2();
    crate::sys::window_server::set_window_spaces_override(wsid, Some(vec![space2.get()]));

    window_server_appeared(&mut reactor, wsid, space1, SpaceEventKind::User);

    crate::sys::window_server::set_window_spaces_override(wsid, None);

    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space2));
    assert_eq!(reactor.authoritative_space_for_window_id(wid), Some(space2));
}

#[test]
fn multi_active_visible_window_appearance_keeps_display_assignment_and_visibility() {
    let (mut reactor, wid, wsid, space1, space2, _frame) = reactor_with_window_moved_to_space2();

    window_server_appeared(&mut reactor, wsid, space1, SpaceEventKind::User);

    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space2));
    assert_eq!(reactor.authoritative_space_for_window_id(wid), Some(space2));
    assert!(reactor.state.windows.is_window_visible(wsid));
}

#[test]
fn multi_active_visible_window_disappearance_does_not_reassign_between_display_spaces() {
    let (mut reactor, wid, wsid, space1, space2, _frame) = reactor_with_window_moved_to_space2();

    window_server_destroyed(&mut reactor, wsid, space1, SpaceEventKind::User);

    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space2));
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
    assert_eq!(reactor.hidden_assigned_space_for_window_id(wid), Some(space1));

    crate::sys::window_server::set_window_spaces_override(wsid, Some(vec![space2.get()]));
    window_server_appeared(&mut reactor, wsid, space2, SpaceEventKind::User);
    crate::sys::window_server::set_window_spaces_override(wsid, None);

    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space2));
    assert_eq!(reactor.authoritative_space_for_window_id(wid), Some(space2));
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
        reactor.discovery_space_for_window_id(wid),
        Some(space2),
        "discovery should stay in the authoritative native space instead of hopping to another display's geometry"
    );
    assert_ne!(
        reactor.discovery_space_for_window_id(wid),
        Some(space1),
        "same-index workspaces on other displays must stay isolated"
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

    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space2));
    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
}

#[test]
fn central_space_resolution_prefers_recent_move_target_over_stale_server_space() {
    let (mut reactor, wid, wsid, space1, space2, moved_frame) =
        reactor_with_window_moved_to_space2();

    reactor.state.windows.set_window_server_space(wsid, Some(space1));

    assert_eq!(reactor.authoritative_space_for_window_id(wid), Some(space2));
    assert_eq!(
        reactor.best_space_for_window(&moved_frame, Some(wsid)),
        Some(space2),
        "core space resolution should prefer the recent move target when geometry and assignment agree"
    );
}

#[test]
fn active_space_membership_refresh_does_not_overwrite_recent_move_target() {
    let (mut reactor, wid, wsid, space1, space2, _) = reactor_with_window_moved_to_space2();

    reactor.refresh_active_space_window_membership(vec![(wsid, Some(space1))]);

    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space2));
    assert_eq!(
        reactor.state.windows.window_server_space(wsid),
        Some(space2),
        "active-space reconciliation must not overwrite a recent cross-display move with stale membership"
    );
    assert!(reactor.state.windows.is_window_visible(wsid));
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

    let (app_tx, mut app_rx) = crate::actor::channel();
    reactor.app_manager.apps.insert(
        pid,
        AppState {
            info: AppInfo {
                bundle_id: Some("com.test.pending-fullscreen".to_string()),
                localized_name: Some("Pending Fullscreen".to_string()),
            },
            handle: AppThreadHandle::new_for_test(app_tx),
        },
    );

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
    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(user_space));
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
        Some(crate::sys::window_server::WindowServerInfo {
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
        reactor.assigned_space_for_window_id(second_wid),
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
        reactor.create_window_data(duplicate_wid).is_none(),
        "duplicate is absent from query windows because it is not manageable"
    );

    reactor.mark_test_window_visible_in_space(duplicate_wsid, user_space);
    window_server_appeared(&mut reactor, duplicate_wsid, user_space, SpaceEventKind::User);

    assert!(
        !has_window_in_layout(&mut reactor, user_space, frame, duplicate_wid),
        "fullscreen restore must evict non-queryable duplicate layout ghosts"
    );
    assert_eq!(reactor.assigned_space_for_window_id(duplicate_wid), None);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(other_space)]));
    assert_eq!(reactor.assigned_space_for_window_id(duplicate_wid), None);
    reactor.handle_event(space_state_event(vec![frame], vec![Some(user_space)]));
    assert_eq!(reactor.assigned_space_for_window_id(duplicate_wid), None);
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
fn known_window_server_appearance_restores_layout_membership_without_reassignment() {
    let (mut reactor, wid, wsid, user_space, _other_space, frame) = reactor_with_window_on_space1();

    reactor.send_layout_event(LayoutEvent::WindowAdded(user_space, wid));
    assert!(has_window_in_layout(&mut reactor, user_space, frame, wid));

    reactor.send_layout_event(LayoutEvent::WindowRemovedPreserveFloating(wid));

    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(user_space));
    assert!(
        !has_window_in_layout(&mut reactor, user_space, frame, wid),
        "temporary removal should clear active layout membership before the appearance event"
    );

    window_server_appeared(&mut reactor, wsid, user_space, SpaceEventKind::User);

    assert!(
        has_window_in_layout(&mut reactor, user_space, frame, wid),
        "same-space appearance should heal active layout membership even when workspace assignment already matches"
    );
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
fn it_ignores_windows_on_disabled_spaces() {
    let (mut apps, mut reactor) = test_context();
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(vec![full_screen], vec![None]));

    reactor.handle_events(apps.make_app(1, make_windows(1)));

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
fn handle_layout_response_groups_windows_by_app_and_screen() {
    let (mut apps, mut reactor) = test_context();
    let (raise_manager_tx, mut raise_manager_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_manager_tx;
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(
        vec![screen1, screen2],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
    ));

    reactor.handle_events(apps.make_app(1, make_windows(2)));

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
                WindowId::new(1, 2),
                WindowId::new(2, 1),
                WindowId::new(2, 2),
            ],
            focus_window: None,
            boundary_hit: None,
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
                vec![WindowId::new(1, 1), WindowId::new(1, 2)],
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
fn handle_layout_response_includes_handles_for_raise_and_focus_windows() {
    let (mut apps, mut reactor) = test_context();
    let (raise_manager_tx, mut raise_manager_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_manager_tx;

    reactor.handle_events(apps.make_app(1, make_windows(1)));
    reactor.handle_events(apps.make_app(2, make_windows(1)));

    let _events = apps.simulate_events();
    while raise_manager_rx.try_recv().is_ok() {}
    reactor.handle_layout_response(
        layout::EventResponse {
            changed: true,
            raise_windows: vec![WindowId::new(1, 1)],
            focus_window: Some(WindowId::new(2, 1)),
            boundary_hit: None,
        },
        None,
    );
    let msg = raise_manager_rx.try_recv().expect("Should have sent an event").1;
    match msg {
        raise_manager::Event::RaiseRequest(RaiseRequest { app_handles, .. }) => {
            assert!(app_handles.contains_key(&1));
            assert!(app_handles.contains_key(&2));
        }
        _ => panic!("Unexpected event: {msg:?}"),
    }
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
    assert!(super::animation::AnimationManager::instant_layout(
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
    assert!(super::animation::AnimationManager::workspace_switch_layout(
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
            0.0,
            Default::default(),
            Default::default(),
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
    let (raise_manager_tx, mut raise_manager_rx) = actor::channel();
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
    reactor.handle_test_layout_command(LayoutCommand::SetWorkspaceLayout {
        workspace: None,
        mode: LayoutMode::Scrolling,
    });
    apps.simulate_until_quiet(&mut reactor);
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
fn carbon_activation_is_replayed_when_it_arrives_before_app_registration() {
    let (mut apps, mut reactor) = test_context();
    let pid = 7;

    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    assert!(apps.requests().is_empty());

    reactor.handle_events(apps.make_app_with_opts(
        pid,
        make_windows(1),
        Some(WindowId::new(pid, 1)),
        true,
        true,
    ));

    let requests = apps.requests();
    assert!(
        requests
            .iter()
            .any(|request| matches!(request, Request::ApplicationGloballyActivated(request_pid) if *request_pid == pid)),
        "launching the current Carbon-frontmost app must replay activation on its app thread: {requests:?}"
    );
}

#[test]
fn duplicate_carbon_activation_is_forwarded_to_app_thread_once() {
    let (mut apps, mut reactor) = test_context();
    let pid = 7;

    reactor.handle_events(apps.make_app(pid, make_windows(1)));
    let _ = apps.requests();

    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));

    let activation_count = apps
        .requests()
        .iter()
        .filter(|request| {
            matches!(request, Request::ApplicationGloballyActivated(request_pid) if *request_pid == pid)
        })
        .count();
    assert_eq!(activation_count, 1);
}

#[test]
fn carbon_activation_is_forwarded_during_refresh_quarantine() {
    let (mut apps, mut reactor) = test_context();
    let pid = 7;

    reactor.handle_events(apps.make_app(pid, make_windows(1)));
    let _ = apps.requests();
    reactor.refresh_quarantine_manager.sleeping = true;

    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    assert!(
        apps.requests()
            .iter()
            .any(|request| matches!(request, Request::ApplicationGloballyActivated(request_pid) if *request_pid == pid))
    );
}

#[test]
fn focus_follows_mouse_emits_focus_without_explicit_arrange() {
    let reactor = test_reactor();
    let space = SpaceId::new(1);
    let window = WindowId::new(7, 1);

    let outcome = window_workflow::handle_mouse_moved_over_window(
        &reactor.app_manager,
        window_workflow::MouseMovedPayload {
            window: Some(window),
            should_sync: true,
            is_main: true,
            needs_layout_sync: true,
            active_space: Some(space),
        },
    )
    .expect("mouse focus workflow");

    assert!(!outcome.arrange.requested);
    assert!(matches!(
        outcome.layout_events.as_slice(),
        [LayoutEvent::WindowFocused(event_space, event_window)]
            if *event_space == space && *event_window == window
    ));
}

#[test]
fn resolved_activation_without_main_window_does_not_choose_arbitrary_app_window() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let pid = 2;

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, pid, make_windows(2));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(pid, 1)));
    reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
        workspace: WorkspaceSelector::Index(1),
        follow: false,
        window_id: None,
    });
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    reactor.handle_event(Event::ApplicationMainWindowChanged(pid, None, Quiet::No));
    reactor.handle_event(Event::ApplicationActivated(pid, Quiet::No));

    assert_eq!(
        reactor.layout_manager.layout_engine.active_workspace_idx(space),
        Some(0)
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

    assert_eq!(
        reactor.test_active_workspace_windows(space),
        vec![WindowId::new(1, 2)]
    );
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
fn title_change_reapply_does_not_rebalance_unchanged_layout() {
    let (mut apps, mut reactor) = test_context();
    reactor.config.virtual_workspaces.reapply_app_rules_on_title_change = true;

    let space = SpaceId::new(1);
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(space)]));

    make_active_app_with_count(&mut apps, &mut reactor, 1, 3, Some(WindowId::new(1, 1)));

    assert!(reactor.layout_manager.layout_engine.selected_window(space).is_some());
    reactor.handle_test_layout_command(LayoutCommand::MoveNode(Direction::Up));
    apps.simulate_until_quiet(&mut reactor);

    let modified = test_layout(&mut reactor, space, full_screen);

    reactor.handle_event(Event::WindowTitleChanged(
        WindowId::new(1, 1),
        "Renamed window".to_string(),
    ));

    assert_eq!(test_layout(&mut reactor, space, full_screen), modified);
}

#[test]
fn title_change_reapply_does_not_rebalance_when_window_stays_floating() {
    let (mut apps, mut reactor) = test_context();
    reactor.config.virtual_workspaces.reapply_app_rules_on_title_change = true;

    let space = SpaceId::new(1);
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(space)]));

    make_active_app_with_count(&mut apps, &mut reactor, 1, 3, Some(WindowId::new(1, 1)));

    assert!(reactor.layout_manager.layout_engine.selected_window(space).is_some());
    reactor.handle_test_layout_command(LayoutCommand::MoveNode(Direction::Up));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    apps.simulate_until_quiet(&mut reactor);
    assert!(reactor.layout_manager.layout_engine.is_window_floating(WindowId::new(1, 1)));

    let modified = test_layout(&mut reactor, space, full_screen);

    reactor.handle_event(Event::WindowTitleChanged(
        WindowId::new(1, 1),
        "Renamed floating window".to_string(),
    ));

    assert!(reactor.layout_manager.layout_engine.is_window_floating(WindowId::new(1, 1)));
    assert_eq!(test_layout(&mut reactor, space, full_screen), modified);
}

#[test]
fn menu_open_state_is_cleared_when_owner_deactivates() {
    let mut reactor = test_reactor();
    let (event_tap_tx, mut event_tap_rx) = actor::channel();
    reactor.communication_manager.event_tap_tx = Some(event_tap_tx);

    reactor.handle_event(Event::MenuOpened(1));
    let disable = event_tap_rx.try_recv().expect("menu-open should update event tap").1;
    assert!(matches!(
        disable,
        crate::actor::event_tap::Request::SetFocusFollowsMouseEnabled(false)
    ));
    assert_eq!(reactor.menu_manager.menu_state, MenuState::Open(1));

    reactor.handle_event(Event::ApplicationDeactivated(1));
    let enable = event_tap_rx
        .try_recv()
        .expect("app deactivation should re-enable focus-follows-mouse")
        .1;
    assert!(matches!(
        enable,
        crate::actor::event_tap::Request::SetFocusFollowsMouseEnabled(true)
    ));
    assert_eq!(reactor.menu_manager.menu_state, MenuState::Closed);
}

#[test]
fn stale_menu_open_state_is_cleared_when_other_app_activates() {
    let mut reactor = test_reactor();
    let (event_tap_tx, mut event_tap_rx) = actor::channel();
    reactor.communication_manager.event_tap_tx = Some(event_tap_tx);

    reactor.handle_event(Event::MenuOpened(1));
    let _ = event_tap_rx.try_recv().expect("menu-open should update event tap");
    assert_eq!(reactor.menu_manager.menu_state, MenuState::Open(1));

    reactor.handle_event(Event::ApplicationGloballyActivated(2));
    let enable = event_tap_rx
        .try_recv()
        .expect("activation of another app should clear stale menu state")
        .1;
    assert!(matches!(
        enable,
        crate::actor::event_tap::Request::SetFocusFollowsMouseEnabled(true)
    ));
    assert_eq!(reactor.menu_manager.menu_state, MenuState::Closed);
}

#[test]
fn same_app_focus_change_hides_mouse_and_window_server_confirmation_reasserts_it() {
    let (mut apps, mut reactor) = test_context();
    let (event_tap_tx, mut event_tap_rx) = actor::channel();
    reactor.communication_manager.event_tap_tx = Some(event_tap_tx);

    let space = SpaceId::new(1);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let first = WindowId::new(1, 1);
    let second = WindowId::new(1, 2);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, first));
    while event_tap_rx.try_recv().is_ok() {}

    reactor.send_layout_event(LayoutEvent::WindowFocused(space, second));

    let request = event_tap_rx.try_recv().expect("same-app focus change should hide mouse").1;
    assert!(matches!(request, crate::actor::event_tap::Request::HideOnFocus));

    reactor.handle_event(Event::WindowServerFocusChanged(second, space));

    let request = event_tap_rx
        .try_recv()
        .expect("WindowServer focus confirmation should reassert hidden mouse")
        .1;
    assert!(matches!(
        request,
        crate::actor::event_tap::Request::EnforceHidden
    ));
}

#[test]
fn it_retains_windows_without_server_ids_after_login_visibility_failure() {
    let (mut apps, mut reactor) = test_context();
    let space = SpaceId::new(1);
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(space)]));

    let window = WindowInfo {
        is_standard: true,
        is_root: true,
        is_minimized: false,
        is_resizable: true,
        min_size: None,
        max_size: None,
        title: "NoServerId".to_string(),
        frame: CGRect::new(CGPoint::new(50., 50.), CGSize::new(400., 400.)),
        sys_id: None,
        bundle_id: None,
        path: None,
        ax_role: None,
        ax_subrole: None,
    };

    reactor.handle_events(apps.make_app_with_opts(
        1,
        vec![window],
        Some(WindowId::new(1, 1)),
        true,
        false,
    ));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(space_state_event(vec![full_screen], vec![None]));

    // Simulate a native fullscreen transition: space temporarily becomes a fullscreen
    // space id (reactor suppresses it to None), then returns to the original space.
    let fullscreen_space = SpaceId::new(0x400000000 + space.get());
    reactor.handle_event(space_state_event(
        vec![full_screen],
        vec![Some(fullscreen_space)],
    ));

    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(space)]));

    loop {
        let requests = apps.requests();
        if requests.is_empty() {
            break;
        }

        let mut other_requests = Vec::new();
        for request in requests {
            match request {
                Request::GetVisibleWindows => {
                    reactor.discover_test_windows(1, vec![], vec![]);
                }
                other => other_requests.push(other),
            }
        }

        if !other_requests.is_empty() {
            let events = apps.simulate_events_for_requests(other_requests);
            for event in events {
                reactor.handle_event(event);
            }
        }
    }
}

#[test]
fn animated_layout_handles_windows_without_server_ids() {
    let (mut apps, mut reactor) = test_context();
    let space = SpaceId::new(1);
    reactor.handle_event(space_state_event(
        vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
        vec![Some(space)],
    ));

    let mut window = make_window(1);
    window.sys_id = None;
    window.frame = CGRect::new(CGPoint::new(50., 50.), CGSize::new(400., 400.));

    reactor.handle_events(apps.make_app_with_opts(
        1,
        vec![window],
        Some(WindowId::new(1, 1)),
        true,
        false,
    ));
    apps.requests();

    let target = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    assert!(super::animation::AnimationManager::animate_layout(
        &mut reactor,
        space,
        &[(WindowId::new(1, 1), target)],
        true,
        None,
    ));

    let requests = apps.requests();
    assert!(
        requests.iter().any(|request| matches!(
            request,
            Request::SetWindowFrame(..) | Request::SetBatchWindowFrame(..)
        )),
        "expected layout to still request a frame update without a server id: {requests:?}"
    );
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

    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(space1));
    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space1));

    reactor.reconcile_authoritative_active_window_snapshot(vec![(wsid, Some(space2))], false);

    assert_eq!(
        reactor.state.windows.window_server_space(wsid),
        Some(space2),
        "authoritative active-space membership should update the tracked native space"
    );
    assert_eq!(
        reactor.assigned_space_for_window_id(wid),
        Some(space2),
        "authoritative active-space membership should reassign the window to the new display"
    );
}

#[test]
fn authoritative_active_window_snapshot_removes_missing_window_from_active_layout() {
    let (mut apps, mut reactor) = test_context();
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let pid: pid_t = 42;
    let moved = WindowId::new(pid, 1);
    let retained = WindowId::new(pid, 2);
    let moved_wsid = WindowServerId::new((pid as u32).saturating_mul(10_000) + 1);
    let retained_wsid = WindowServerId::new((pid as u32).saturating_mul(10_000) + 2);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, pid, make_windows(2));

    assert!(has_window_in_layout(&mut reactor, space, frame, moved));
    assert!(has_window_in_layout(&mut reactor, space, frame, retained));
    reactor.mark_test_window_visible_in_space(moved_wsid, space);
    reactor.mark_test_window_visible_in_space(retained_wsid, space);
    reactor
        .reconcile_authoritative_active_window_snapshot(vec![(retained_wsid, Some(space))], false);

    assert!(
        !has_window_in_layout(&mut reactor, space, frame, moved),
        "active-space window missing from the authoritative snapshot must be removed immediately"
    );
    assert!(
        !reactor.state.windows.is_window_visible(moved_wsid),
        "authoritative snapshot reconcile should clear visible state for missing windows"
    );
    assert!(has_window_in_layout(&mut reactor, space, frame, retained));
}

#[test]
fn authoritative_active_window_snapshot_reassigns_missing_window_to_inactive_space() {
    let (mut apps, mut reactor) = test_context();
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let active_space = SpaceId::new(1);
    let inactive_space = SpaceId::new(2);
    let pid: pid_t = 43;
    let moved = WindowId::new(pid, 1);
    let retained = WindowId::new(pid, 2);
    let moved_wsid = WindowServerId::new((pid as u32).saturating_mul(10_000) + 1);
    let retained_wsid = WindowServerId::new((pid as u32).saturating_mul(10_000) + 2);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(active_space)]));
    apps.make_app_and_settle(&mut reactor, pid, make_windows(2));

    reactor.mark_test_window_visible_in_space(moved_wsid, active_space);
    reactor.mark_test_window_visible_in_space(retained_wsid, active_space);
    crate::sys::window_server::set_window_spaces_override(
        moved_wsid,
        Some(vec![inactive_space.get()]),
    );

    reactor.reconcile_authoritative_active_window_snapshot(
        vec![(retained_wsid, Some(active_space))],
        false,
    );

    crate::sys::window_server::set_window_spaces_override(moved_wsid, None);

    assert_eq!(
        reactor.assigned_space_for_window_id(moved),
        Some(inactive_space),
        "missing active-space windows should migrate to their actual inactive native space"
    );
    assert!(
        reactor.test_workspace_for_window(active_space, moved).is_none(),
        "window should no longer belong to the old active native space"
    );
    assert!(
        reactor.test_workspace_for_window(inactive_space, moved).is_some(),
        "window should now belong to the inactive native space that WindowServer reports"
    );
    assert!(
        !has_window_in_layout(&mut reactor, active_space, frame, moved),
        "window moved onto an inactive native space must be removed from the active layout"
    );
    assert!(has_window_in_layout(&mut reactor, active_space, frame, retained));
    assert_eq!(
        reactor.assigned_space_for_window_id(retained),
        Some(active_space),
        "other visible windows on the active space must remain untouched"
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
    crate::sys::window_server::set_window_spaces_override(
        moved_wsid,
        Some(vec![inactive_space.get()]),
    );
    crate::sys::window_server::set_space_window_list_for_space_override(
        active_space.get(),
        Some(vec![retained_wsid.as_u32()]),
    );

    reactor.handle_event(space_state_event_with(
        vec![frame],
        vec![Some(active_space)],
        |state| {
            state.has_seen_display_set = true;
            state.topology_window_delta = Some(crate::actor::spaces::TopologyWindowDelta {
                epoch: 11,
                flags: crate::sys::skylight::DisplayReconfigFlags::MOVED,
                appeared: Vec::new(),
                disappeared: vec![(moved_wsid, active_space)],
            });
        },
    ));

    crate::sys::window_server::set_window_spaces_override(moved_wsid, None);
    crate::sys::window_server::set_space_window_list_for_space_override(active_space.get(), None);

    assert_eq!(reactor.assigned_space_for_window_id(moved), Some(inactive_space));
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

    crate::sys::window_server::set_window_spaces_override(wsid, Some(vec![space2.get()]));
    crate::sys::window_server::set_space_window_list_for_space_override(space1.get(), Some(vec![]));
    crate::sys::window_server::set_space_window_list_for_space_override(
        space2.get(),
        Some(vec![wsid.as_u32()]),
    );

    reactor.handle_event(space_state_event_with(
        vec![screen1, screen2],
        vec![Some(space1), Some(space2)],
        |state| {
            state.has_seen_display_set = true;
            state.topology_window_delta = Some(crate::actor::spaces::TopologyWindowDelta {
                epoch: 12,
                flags: crate::sys::skylight::DisplayReconfigFlags::MOVED,
                appeared: vec![(wsid, space2)],
                disappeared: vec![(wsid, space1)],
            });
        },
    ));

    crate::sys::window_server::set_window_spaces_override(wsid, None);
    crate::sys::window_server::set_space_window_list_for_space_override(space1.get(), None);
    crate::sys::window_server::set_space_window_list_for_space_override(space2.get(), None);

    assert_eq!(
        reactor.assigned_space_for_window_id(wid),
        Some(space2),
        "topology delta should still be processed even when the forwarded screens snapshot is unchanged"
    );
    assert_eq!(reactor.state.windows.window_server_space(wsid), Some(space2));
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
        reactor.hidden_assigned_space_for_window_id(wid),
        Some(space),
        "workspace-hidden status should follow Rini's workspace assignment, not stale corner geometry"
    );
    assert_eq!(
        reactor.geometry_space_for_window(
            &CGRect::new(CGPoint::new(200.0, 200.0), CGSize::new(400.0, 400.0)),
            Some(wsid),
        ),
        Some(space),
        "topology changes can leave hidden windows at stale coordinates; they must still resolve to their assigned space"
    );
}

#[test]
fn display_churn_quarantines_window_frame_and_membership_events() {
    let reactor = test_reactor();
    let space = SpaceId::new(7);
    let wsid = WindowServerId::new(77);
    let _ = crate::sys::display_churn::begin(crate::sys::skylight::DisplayReconfigFlags::ADD);

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

    let _ = crate::sys::display_churn::end();
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
fn lifecycle_events_are_quarantined_during_sleep_and_session_inactivity() {
    let mut reactor = test_reactor();
    let space = SpaceId::new(8);

    reactor.refresh_quarantine_manager.sleeping = true;
    assert!(reactor.should_quarantine_space_lifecycle_event(&Event::SpaceCreated(space)));

    reactor.refresh_quarantine_manager.sleeping = false;
    reactor.refresh_quarantine_manager.session_inactive = true;
    assert!(reactor.should_quarantine_space_lifecycle_event(&Event::SpaceDestroyed(space)));
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
fn fullscreen_space_in_screen_params_does_not_trigger_topology_relayout() {
    let mut reactor = test_reactor();

    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1280., 800.));
    let user_space = SpaceId::new(11);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let display_uuid = "11111111-1111-1111-1111-111111111111".to_string();
    let screens_for = |space: SpaceId| -> Vec<ScreenInfo> {
        vec![ScreenInfo {
            id: crate::sys::screen::ScreenId::new(0),
            frame,
            space: Some(space),
            display_uuid: display_uuid.clone(),
            name: None,
        }]
    };

    reactor.handle_event(space_state_event_from_screens(screens_for(user_space)));
    assert_eq!(
        reactor.layout_manager.layout_engine.last_space_for_display_uuid(&display_uuid),
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
        reactor.layout_manager.layout_engine.last_space_for_display_uuid(&display_uuid),
        Some(user_space),
        "fullscreen spaces should not replace display->user-space history"
    );

    reactor.handle_event(space_state_event_from_screens(screens_for(user_space)));
    assert_eq!(
        reactor.layout_manager.layout_engine.last_space_for_display_uuid(&display_uuid),
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

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(left_space_2), Some(right_space_1)],
    ));
    reactor.space_state.fullscreen_spaces.insert(right_fullscreen);

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(left_space_2), None],
    ));

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

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(left_space_2), Some(right_space_1)],
    ));
    reactor.space_state.fullscreen_spaces.insert(right_fullscreen);
    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(left_space_2), None],
    ));

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(left_space_1), None],
    ));

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
        id: crate::sys::screen::ScreenId::new(0),
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

fn fullscreen_startup_fixture(
    with_app_rule: bool,
    preserve_workspace: bool,
) -> (
    Reactor,
    WindowId,
    SpaceId,
    crate::model::virtual_workspace::VirtualWorkspaceId,
    crate::model::virtual_workspace::VirtualWorkspaceId,
) {
    let mut workspace_cfg = crate::common::config::VirtualWorkspaceSettings {
        default_workspace_count: 2,
        ..crate::common::config::VirtualWorkspaceSettings::default()
    };
    if with_app_rule {
        workspace_cfg.app_rules = vec![crate::common::config::AppWorkspaceRule {
            app_id: Some("com.testapp1".to_string()),
            workspace: Some(crate::common::config::WorkspaceSelector::Index(1)),
            floating: false,
            position: None,
            size: None,
            focus: false,
            manage: true,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
        }];
    }

    let mut reactor = test_reactor_with_workspace_settings(&workspace_cfg);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let pid = 1;
    let wid = WindowId::new(pid, 1);
    let wsid = WindowServerId::new(10_001);
    let user_space = SpaceId::new(1);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());

    reactor.handle_event(fullscreen_startup_space_state(
        screen,
        "test-display-0".to_string(),
        user_space,
        fullscreen_space,
    ));
    reactor.add_test_app_with_info(pid, "com.testapp1", "TestApp1");

    let workspaces = reactor.test_workspace_ids(user_space);
    let default_workspace = workspaces[0];
    let secondary_workspace = workspaces[1];
    if preserve_workspace {
        assert!(reactor.assign_test_window_to_workspace(user_space, wid, secondary_workspace));
    }

    reactor.track_test_window_server_info(wsid, pid, screen);
    reactor.state.windows.set_window_server_space(wsid, Some(user_space));
    reactor.discover_test_windows(
        pid,
        vec![(
            wid,
            make_window_info(screen, Some(wsid), "Window", Some("com.testapp1")),
        )],
        vec![wid],
    );

    (reactor, wid, user_space, default_workspace, secondary_workspace)
}

fn rekey_window(reactor: &mut Reactor, old_wid: WindowId, new_wid: WindowId) {
    let old_info = reactor
        .state
        .windows
        .window(old_wid)
        .expect("old window should exist before rekey")
        .info
        .clone();
    reactor.discover_test_windows(
        old_wid.pid,
        vec![(
            new_wid,
            WindowInfo {
                sys_id: old_info.sys_id,
                ..old_info
            },
        )],
        vec![new_wid],
    );
}

#[test]
fn fullscreen_startup_applies_app_rules_to_hidden_user_space_windows() {
    let (reactor, wid, user_space, _default_workspace, target_workspace) =
        fullscreen_startup_fixture(true, false);

    assert_eq!(reactor.assigned_space_for_window_id(wid), Some(user_space));
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

// Helper: check whether any window owned by `pid` appears in the layout tree for `space`.
fn has_window_in_layout(
    reactor: &mut Reactor,
    space: SpaceId,
    screen: CGRect,
    wid: WindowId,
) -> bool {
    let gaps = reactor.config.settings.layout.gaps.clone();
    reactor
        .layout_manager
        .layout_engine
        .calculate_layout(space, screen, &gaps, 0.0, Default::default(), Default::default())
        .iter()
        .any(|(layout_wid, _)| *layout_wid == wid)
}

fn test_layout(reactor: &mut Reactor, space: SpaceId, screen: CGRect) -> Vec<(WindowId, CGRect)> {
    let gaps = reactor.config.settings.layout.gaps.clone();
    reactor.layout_manager.layout_engine.calculate_layout(
        space,
        screen,
        &gaps,
        0.0,
        crate::common::config::HorizontalPlacement::Top,
        crate::common::config::VerticalPlacement::Right,
    )
}

fn make_active_app(
    apps: &mut Apps,
    reactor: &mut Reactor,
    pid: pid_t,
    windows: Vec<WindowInfo>,
    main_window: Option<WindowId>,
) {
    reactor.handle_events(apps.make_app_with_opts(pid, windows, main_window, true, true));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    apps.simulate_until_quiet(reactor);
}

fn make_active_app_with_count(
    apps: &mut Apps,
    reactor: &mut Reactor,
    pid: pid_t,
    window_count: usize,
    main_window: Option<WindowId>,
) {
    make_active_app(apps, reactor, pid, make_windows(window_count), main_window);
}

fn simulate_login_screen_refresh(apps: &mut Apps, reactor: &mut Reactor, pid: pid_t) {
    for request in apps.requests() {
        match request {
            Request::GetVisibleWindows => reactor.discover_test_windows(pid, vec![], vec![]),
            request => {
                for event in apps.simulate_events_for_requests(vec![request]) {
                    reactor.handle_event(event);
                }
            }
        }
    }
    apps.simulate_until_quiet(reactor);
}

#[test]
fn discovery_minimize_transition_removes_window_from_layout() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));

    assert!(has_window_in_layout(&mut reactor, space, screen, wid));

    reactor.discover_test_windows(
        1,
        vec![(
            wid,
            WindowInfo {
                is_minimized: true,
                ..make_window(1)
            },
        )],
        vec![],
    );

    assert!(
        !has_window_in_layout(&mut reactor, space, screen, wid),
        "minimized window must be removed from layout when discovery reports it minimized"
    );
    assert!(
        reactor.state.windows.window(wid).is_some_and(|window| window.info.is_minimized),
        "reactor state must keep the window marked minimized"
    );
}

#[test]
fn discovery_restore_transition_readds_window_to_layout() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);
    let mut windows = make_windows(1);
    windows[0].is_minimized = true;

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, windows);

    assert!(
        !has_window_in_layout(&mut reactor, space, screen, wid),
        "startup-minimized window must not be inserted into layout"
    );

    reactor.discover_test_windows(1, vec![(wid, make_window(1))], vec![wid]);

    assert!(
        has_window_in_layout(&mut reactor, space, screen, wid),
        "restored window must return to layout when discovery reports it visible again"
    );
    assert!(
        reactor
            .state
            .windows
            .window(wid)
            .is_some_and(|window| !window.info.is_minimized),
        "reactor state must clear the minimized flag after restore"
    );
}

#[test]
fn discovery_manageability_loss_removes_window_from_layout() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));

    assert!(has_window_in_layout(&mut reactor, space, screen, wid));

    reactor.discover_test_windows(
        1,
        vec![(
            wid,
            WindowInfo {
                is_root: false,
                ..make_window(1)
            },
        )],
        vec![wid],
    );

    assert!(
        !has_window_in_layout(&mut reactor, space, screen, wid),
        "window must be removed from layout when discovery marks it unmanageable"
    );
    assert!(
        reactor
            .state
            .windows
            .window(wid)
            .is_some_and(|window| !window.matches_filter(WindowFilter::Manageable)),
        "reactor state must keep the window marked unmanageable"
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
fn wake_gate_waits_for_fresh_space_snapshot_before_refresh() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    assert!(apps.requests().is_empty());

    reactor.handle_event(Event::SystemWillSleep);
    reactor.handle_event(Event::SystemWoke);
    reactor.handle_event(Event::ApplicationGloballyActivated(1));

    let requests = apps.requests();
    assert!(
        requests.iter().all(|request| !matches!(request, Request::GetVisibleWindows)),
        "wake should quarantine visible-window enumeration until a fresh space snapshot: {requests:?}"
    );
    assert!(
        requests.iter().any(
            |request| matches!(request, Request::ApplicationGloballyActivated(pid) if *pid == 1)
        ),
        "Carbon activation should still be reconciled by the app thread: {requests:?}"
    );

    let stale_snapshot = space_state_event(vec![screen], vec![Some(space)]);
    reactor.handle_event(stale_snapshot);
    assert!(
        apps.requests().is_empty(),
        "an older queued WM snapshot must not release the wake quarantine"
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
        "the first fresh post-wake snapshot should flush exactly one deferred visibility refresh"
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
fn current_ax_destruction_after_quarantine_release_removes_window() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    assert!(!reactor.refreshes_blocked());
    assert!(has_window_in_layout(&mut reactor, space, screen, wid));
    let wsid = reactor.test_window_server_id(wid);

    crate::sys::window_server::set_window_ordered_in_override(wsid, Some(false));
    reactor.handle_event(Event::WindowDestroyed(wid));
    crate::sys::window_server::set_window_ordered_in_override(wsid, None);

    assert!(reactor.state.windows.record(wid).is_none());
    assert!(!has_window_in_layout(&mut reactor, space, screen, wid));
}

#[test]
fn ax_destruction_removes_window_on_known_inactive_space_outside_churn() {
    let (mut reactor, wid, wsid, active_space, inactive_space, _frame) =
        reactor_with_window_on_space1();
    let inactive_workspace = reactor.test_workspace(inactive_space, 0);
    assert!(reactor.assign_test_window_to_workspace(inactive_space, wid, inactive_workspace));
    reactor.state.windows.set_window_server_space(wsid, Some(inactive_space));
    reactor.state.windows.mark_window_hidden(wsid);
    assert!(reactor.is_window_on_known_inactive_space(wid));

    crate::sys::window_server::set_window_ordered_in_override(wsid, Some(false));
    reactor.handle_event(Event::WindowDestroyed(wid));
    crate::sys::window_server::set_window_ordered_in_override(wsid, None);

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

    crate::sys::window_server::set_window_ordered_in_override(wsid, Some(false));
    reactor.handle_event(Event::WindowDestroyed(wid));
    crate::sys::window_server::set_window_ordered_in_override(wsid, None);

    assert!(reactor.state.windows.record(wid).is_none());
    assert!(!has_window_in_layout(&mut reactor, space, screen, wid));
}

#[test]
fn repeated_ordered_out_ax_replacement_does_not_accumulate_layout_ghosts() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let pid = 1;
    let middle = WindowId::new(pid, 2);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, pid, make_windows(3));
    let middle_info = reactor.state.windows.window(middle).unwrap().info.clone();
    let wsid = reactor.test_window_server_id(middle);
    assert_eq!(test_layout(&mut reactor, space, screen).len(), 3);

    for _ in 0..2 {
        crate::sys::window_server::set_window_ordered_in_override(wsid, Some(false));
        reactor.handle_event(Event::WindowDestroyed(middle));
        crate::sys::window_server::set_window_ordered_in_override(wsid, None);

        assert!(reactor.state.windows.record(middle).is_none());
        assert_eq!(
            test_layout(&mut reactor, space, screen).len(),
            2,
            "ordered-out AX destruction must remove its slot completely",
        );

        reactor.track_test_window_server_info(wsid, pid, middle_info.frame);
        reactor.mark_test_window_visible_in_space(wsid, space);
        reactor.discover_test_windows(pid, vec![(middle, middle_info.clone())], vec![
            WindowId::new(pid, 1),
            middle,
            WindowId::new(pid, 3),
        ]);
        assert_eq!(
            test_layout(&mut reactor, space, screen).len(),
            3,
            "rediscovery must restore exactly one slot",
        );
    }
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

    crate::sys::window_server::set_window_ordered_in_override(wsid, Some(true));
    reactor.handle_event(Event::WindowDestroyed(wid));
    crate::sys::window_server::set_window_ordered_in_override(wsid, None);

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

#[test]
fn ax_invalidation_during_refresh_quarantine_is_deferred_without_layout_mutation() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    assert!(has_window_in_layout(&mut reactor, space, screen, wid));
    reactor.refresh_quarantine_manager.display_churn_active = true;

    reactor.handle_event(Event::WindowDestroyed(wid));

    assert!(
        reactor.state.windows.window(wid).is_some(),
        "unstable AX invalidation must not discard logical window state",
    );
    assert!(
        has_window_in_layout(&mut reactor, space, screen, wid),
        "unstable AX invalidation must not mutate layout topology",
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

#[test]
fn genuine_close_during_sleep_recovery_does_not_leave_layout_ghost() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let survivor = WindowId::new(1, 1);
    let closed = WindowId::new(1, 2);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(2));
    let closed_wsid = reactor.test_window_server_id(closed);

    reactor.handle_event(Event::SystemWillSleep);
    reactor.handle_event(Event::SessionDidResignActive);
    reactor.handle_event(Event::WindowDestroyed(closed));
    assert!(
        has_window_in_layout(&mut reactor, space, screen, closed),
        "the ambiguous AX edge must be preserved while sleep quarantine is active",
    );

    reactor.handle_event(Event::SystemWoke);
    reactor.handle_event(Event::SessionDidBecomeActive);
    let mut recovered =
        forwarded_space_state(make_screen_snapshots(vec![screen], vec![Some(space)]));
    recovered.releases_lifecycle_refresh_quarantine = true;
    recovered
        .active_window_spaces
        .insert(WindowServerId::new(survivor.idx.get()), space);

    crate::sys::window_server::set_window_ordered_in_override(closed_wsid, Some(false));
    reactor.handle_event(Event::SpaceStateChanged(recovered));
    reactor.discover_test_windows(1, vec![], vec![survivor]);
    crate::sys::window_server::set_window_ordered_in_override(closed_wsid, None);

    assert!(reactor.state.windows.record(closed).is_none());
    assert!(!has_window_in_layout(&mut reactor, space, screen, closed));
    assert!(reactor.state.windows.contains_window(survivor));
    assert!(has_window_in_layout(&mut reactor, space, screen, survivor));
    assert_eq!(
        test_layout(&mut reactor, space, screen).len(),
        1,
        "post-sleep discovery must not retain a stale layout slot for the closed window",
    );
}

#[test]
fn last_window_close_during_sleep_recovery_does_not_leave_layout_ghost() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let closed = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    let closed_wsid = reactor.test_window_server_id(closed);

    reactor.handle_event(Event::SystemWillSleep);
    reactor.handle_event(Event::SessionDidResignActive);
    reactor.handle_event(Event::WindowDestroyed(closed));
    assert!(
        has_window_in_layout(&mut reactor, space, screen, closed),
        "the ambiguous AX edge must be preserved while sleep quarantine is active",
    );

    reactor.handle_event(Event::SystemWoke);
    reactor.handle_event(Event::SessionDidBecomeActive);
    let mut recovered =
        forwarded_space_state(make_screen_snapshots(vec![screen], vec![Some(space)]));
    recovered.releases_lifecycle_refresh_quarantine = true;

    crate::sys::window_server::set_window_ordered_in_override(closed_wsid, Some(false));
    reactor.handle_event(Event::SpaceStateChanged(recovered));
    reactor.discover_test_windows(1, vec![], vec![]);
    crate::sys::window_server::set_window_ordered_in_override(closed_wsid, None);

    assert!(reactor.state.windows.record(closed).is_none());
    assert!(!has_window_in_layout(&mut reactor, space, screen, closed));
    assert!(test_layout(&mut reactor, space, screen).is_empty());
}

#[test]
fn authoritative_destruction_removes_window_server_backed_state() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    let wsid = reactor.test_window_server_id(wid);

    let outcome = window_workflow::handle_window_destroyed(
        &mut reactor.state,
        &reactor.transaction_manager,
        &mut reactor.drag_manager,
        window_workflow::WindowDestroyedPayload { window: wid },
    )
    .expect("authoritative destruction should be handled");
    reactor.apply_event_outcome(outcome);

    assert!(reactor.state.windows.record(wid).is_none());
    assert_eq!(reactor.state.windows.tracked_window_id(wsid), None);
    assert_eq!(reactor.state.windows.workspace_info_for_window(wid), None);
}

#[test]
fn authoritative_active_space_membership_comes_from_space_window_ids_directly() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wsid_a = WindowServerId::new(41);
    let wsid_b = WindowServerId::new(42);

    crate::sys::window_server::set_space_window_list_for_connection_override(Some(vec![
        wsid_a.as_u32(),
        wsid_b.as_u32(),
    ]));

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    let snapshot = reactor.authoritative_active_space_windows();

    crate::sys::window_server::set_space_window_list_for_connection_override(None);

    let ids: Vec<_> = snapshot.into_iter().map(|(wsid, _)| wsid).collect();
    assert_eq!(
        ids,
        vec![wsid_a, wsid_b],
        "active-space membership should be built from the space's own WS ids rather than the lagging global visible-window list"
    );
}

#[test]
fn authoritative_active_space_membership_queries_each_active_space_independently() {
    let mut reactor = test_reactor();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    let wsid_left = WindowServerId::new(41);
    let wsid_right = WindowServerId::new(42);

    crate::sys::window_server::set_space_window_list_for_space_override(
        space1.get(),
        Some(vec![wsid_left.as_u32()]),
    );
    crate::sys::window_server::set_space_window_list_for_space_override(
        space2.get(),
        Some(vec![wsid_right.as_u32()]),
    );
    crate::sys::window_server::set_window_spaces_override(wsid_left, Some(vec![space1.get()]));
    crate::sys::window_server::set_window_spaces_override(wsid_right, Some(vec![space2.get()]));

    reactor.handle_event(space_state_event(
        vec![left, right],
        vec![Some(space1), Some(space2)],
    ));
    let mut snapshot = reactor.authoritative_active_space_windows();

    crate::sys::window_server::set_space_window_list_for_space_override(space1.get(), None);
    crate::sys::window_server::set_space_window_list_for_space_override(space2.get(), None);
    crate::sys::window_server::set_window_spaces_override(wsid_left, None);
    crate::sys::window_server::set_window_spaces_override(wsid_right, None);

    snapshot.sort_unstable_by_key(|(wsid, _)| wsid.as_u32());
    assert_eq!(
        snapshot,
        vec![(wsid_left, Some(space1)), (wsid_right, Some(space2))],
        "multi-display active-space membership should be collected per active space so stale union snapshots do not keep windows visible after topology changes"
    );
}

#[test]
fn empty_active_space_membership_during_wake_race_does_not_blank_known_active_windows() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);
    let wsid = WindowServerId::new(10001);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));

    reactor.mark_test_window_visible_in_space(wsid, space);

    crate::sys::window_server::set_space_window_list_for_connection_override(Some(vec![]));
    reactor.refresh_window_server_snapshot_for_active_spaces();
    crate::sys::window_server::set_space_window_list_for_connection_override(None);

    assert!(
        reactor.state.windows.is_window_visible(wsid),
        "a transient empty active-space WS-id result after wake must not blank windows we already know belong to the active space"
    );
    assert!(
        has_window_in_layout(&mut reactor, space, screen, wid),
        "preserving the visibility basis must also preserve the active workspace layout until discovery catches up"
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

#[test]
fn wsid_rekey_preserves_floating_membership_and_position() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let old_wid = WindowId::new(1, 1);
    let new_wid = WindowId::new(1, 99);
    let stored_position = CGRect::new(CGPoint::new(320., 180.), CGSize::new(240., 200.));

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    make_active_app(&mut apps, &mut reactor, 1, make_windows(1), Some(old_wid));

    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    apps.simulate_until_quiet(&mut reactor);
    assert!(reactor.layout_manager.layout_engine.is_window_floating(old_wid));

    let active_workspace = reactor
        .layout_manager
        .layout_engine
        .active_workspace(space)
        .expect("active workspace");
    reactor.layout_manager.layout_engine.store_floating_position(
        space,
        active_workspace,
        old_wid,
        stored_position,
    );

    rekey_window(&mut reactor, old_wid, new_wid);

    assert!(!reactor.layout_manager.layout_engine.is_window_floating(old_wid));
    assert!(reactor.layout_manager.layout_engine.is_window_floating(new_wid));
    assert_eq!(
        reactor.layout_manager.layout_engine.get_floating_position(
            space,
            active_workspace,
            old_wid
        ),
        None
    );
    assert_eq!(
        reactor.layout_manager.layout_engine.get_floating_position(
            space,
            active_workspace,
            new_wid
        ),
        Some(stored_position)
    );
}

#[test]
fn native_space_resolution_policy_table() {
    let mut cases = Vec::new();

    // A direct observation from the old space is stale while Rini's target is
    // still pending.
    {
        let (reactor, _wid, wsid, space1, space2, _) = reactor_with_window_moved_to_space2();
        cases.push((
            "stale origin",
            reactor.resolve_native_space(wsid, Some(space1)),
            Some(space2),
        ));
    }

    // A direct observation of the target confirms the pending move.
    {
        let (reactor, _wid, wsid, _space1, space2, _) = reactor_with_window_moved_to_space2();
        let resolved = reactor.resolve_native_space(wsid, Some(space2));
        reactor.clear_pending_target_if_confirmed_space(wsid, space2);
        cases.push(("confirmed target", resolved, Some(space2)));
    }

    // With no pending Rini move, a live WindowServer observation is an external move.
    {
        let (reactor, _wid, wsid, _space1, space2, _) = reactor_with_window_on_space1();
        crate::sys::window_server::set_window_spaces_override(wsid, Some(vec![space2.get()]));
        let resolved = reactor.resolve_native_space(wsid, Some(space2));
        crate::sys::window_server::set_window_spaces_override(wsid, None);
        cases.push(("newer external move", resolved, Some(space2)));
    }

    // With only an accepted prior observation, a partial sample keeps it.
    {
        let (reactor, _wid, wsid, space1, _space2, _) = reactor_with_window_on_space1();
        cases.push((
            "partial observation",
            reactor.resolve_native_space(wsid, None),
            Some(space1),
        ));
    }

    // Geometry is used only when no native or prior WindowServer state exists.
    {
        let mut reactor = test_reactor();
        let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
        let space2 = SpaceId::new(2);
        reactor.handle_event(space_state_event(
            vec![left, right],
            vec![Some(SpaceId::new(1)), Some(space2)],
        ));
        let frame = CGRect::new(CGPoint::new(1200., 100.), CGSize::new(400., 400.));
        cases.push((
            "geometry fallback",
            reactor.best_space_for_window(&frame, Some(WindowServerId::new(9999))),
            Some(space2),
        ));
    }

    for (case, resolved, expected) in cases {
        assert_eq!(resolved, expected, "resolver case: {case}");
    }
}

fn laid_out_frame(
    reactor: &mut Reactor,
    space: SpaceId,
    screen: CGRect,
    wid: WindowId,
) -> Option<CGRect> {
    let gaps = reactor.config.settings.layout.gaps.clone();
    reactor
        .layout_manager
        .layout_engine
        .calculate_layout_with_virtual_workspaces(
            &reactor.state.windows,
            space,
            screen,
            &gaps,
            0.0,
            Default::default(),
            Default::default(),
            |q| reactor.state.windows.window(q).map(|w| w.frame_monotonic),
            &[screen],
        )
        .into_iter()
        .find(|(w, _)| *w == wid)
        .map(|(_, f)| f)
}

#[test]
fn floating_window_toggles_to_fullscreen() {
    let (mut reactor, wid, space1, screen, _floating_frame) = reactor_with_floating_window();
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreen);
    let laid_out = laid_out_frame(&mut reactor, space1, screen, wid).expect("window laid out");
    assert!(
        laid_out.same_as(screen),
        "expected fullscreen {screen:?}, got {laid_out:?}"
    );
}

#[test]
fn floating_window_toggle_off_restore_previous_frame() {
    let (mut reactor, wid, space1, screen, floating_frame) = reactor_with_floating_window();
    // Turn on
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreen);
    // Turn off
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreen);
    let laid_out = laid_out_frame(&mut reactor, space1, screen, wid).expect("window laid out");
    assert!(
        laid_out.same_as(floating_frame),
        "expected restore to {floating_frame:?}, got {laid_out:?}"
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

/// Autosave must not reshape live layout state.
///
/// save_current_layout normalizes floating-versus-tiled ownership and rewrites
/// stored floating frames. Those mutations are correct for an explicit save but
/// destructive on every layout change: wiring them into the layout path broke
/// un-fullscreening a floating window, because the frame it should return to had
/// already been overwritten. autosave_current_layout must only refresh fingerprints
/// and write.
#[test]
fn autosave_preserves_floating_restore_frame() {
    let (mut reactor, wid, space1, screen, floating_frame) = reactor_with_floating_window();

    let dir = std::env::temp_dir().join(format!("rini-autosave-test-{}", std::process::id()));
    let path = dir.join("layout.ron");
    let _ = std::fs::remove_dir_all(&dir);

    // Autosave between the two toggles, which is what the live reactor does.
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreen);
    let active = reactor.workspace_command_space();
    // The write itself may legitimately be refused: the persisted-topology validator
    // rejects snapshots taken while a workspace has no layout state, which happens
    // transiently in this harness. That is fine and is why the reactor logs and
    // continues instead of propagating. What must hold is that ATTEMPTING the save
    // does not disturb live state.
    let _ = reactor.layout_manager.layout_engine.autosave_current_layout(
        path.clone(),
        &reactor.state.windows,
        active,
    );
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreen);

    let laid_out = laid_out_frame(&mut reactor, space1, screen, wid).expect("window laid out");
    assert!(
        laid_out.same_as(floating_frame),
        "autosave must not disturb the floating restore frame: expected {floating_frame:?}, got {laid_out:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A display that is unplugged and replugged must get its layout back, even though
/// macOS assigns a brand-new space id each time.
///
/// Two bugs made this fail. prune_display_state deleted the display's UUID -> space
/// mapping the moment it was unplugged, destroying the only durable link between a
/// physical display and its layout; and the spaces actor's own remap path is gated
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
        reactor
            .layout_manager
            .layout_engine
            .last_space_for_display_uuid("test-display-1"),
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
        reactor
            .layout_manager
            .layout_engine
            .last_space_for_display_uuid("test-display-1"),
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

/// Present the windows macOS reports as living on each space, the way a real snapshot
/// does. Without this the reactor sees an empty active-space query, concludes every
/// window has vanished, and drops its workspace assignment — which makes any
/// display-change test measure that instead of what it meant to.
fn set_space_membership(entries: &[(SpaceId, &[u32])]) {
    for (space, wsids) in entries {
        crate::sys::window_server::set_space_window_list_for_space_override(
            space.get(),
            Some(wsids.to_vec()),
        );
    }
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
fn replug_leaves_the_other_display_strip_order_untouched() {
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
        reactor.layout_manager.layout_engine.window_display_home(exile),
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
        reactor.layout_manager.layout_engine.window_display_home(exile),
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

/// A floating window must stay where the user drops it.
///
/// maybe_swap_on_drag ran for floating windows even though they are not in the tiling
/// strip and have nothing to swap with. Finding no target, it fell through to the tail of
/// the function, which clears `skip_layout_for_window` — mid-gesture. The next layout pass
/// then reasserted the window's stored frame underneath the drag.
///
/// Measured on System Settings: the reported old_frame rewound repeatedly inside one drag
/// (695,188 -> 832,167 -> 927,146, then back to 350,212), and the window ended up wherever
/// the tug-of-war left it — about a third of the way back, as reported.
#[test]
fn dragging_a_floating_window_keeps_the_layout_skip_for_the_whole_gesture() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let floating = WindowId::new(1, 1);
    let tiled = WindowId::new(1, 2);
    let start = CGRect::new(CGPoint::new(100., 100.), CGSize::new(200., 200.));

    set_space_membership(&[(space, &[1, 2])]);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.add_test_app(1);
    reactor.add_test_window(tiled, WindowServerId::new(2), Some(space), screen);
    reactor.add_test_window(floating, WindowServerId::new(1), Some(space), start);
    let workspace = reactor.test_workspace(space, 0);
    assert!(reactor.assign_test_window_to_workspace(space, tiled, workspace));
    assert!(reactor.assign_test_window_to_workspace(space, floating, workspace));
    reactor.send_layout_event(LayoutEvent::WindowAdded(space, tiled));
    reactor.send_layout_event(LayoutEvent::WindowAdded(space, floating));

    reactor.send_layout_event(LayoutEvent::WindowFocused(space, floating));
    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    assert!(
        reactor.layout_manager.layout_engine.is_window_floating(floating),
        "test setup must make the window floating"
    );

    // Drag it across the screen, over the tiled window, as a real drag does.
    reactor.ensure_active_drag(floating, &start);
    for x in [300., 500., 700.] {
        let frame = CGRect::new(CGPoint::new(x, 100.), start.size);
        if let Some(state) = reactor.state.windows.window_mut(floating) {
            state.frame_monotonic = frame;
        }
        reactor.maybe_swap_on_drag_for_test(floating, frame);
        assert_eq!(
            reactor.drag_manager.skip_layout_for_window,
            Some(floating),
            "the layout skip must survive the whole gesture; clearing it mid-drag lets the \
             next layout pass write the stored frame back underneath the user"
        );
    }
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
        reactor.layout_manager.layout_engine.window_display_home(doomed),
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
        reactor.layout_manager.layout_engine.window_display_home(doomed),
        None,
        "a closed window must not keep a home; stale entries make a replug look like it \
         has windows to bring back when it does not"
    );
    assert!(
        !reactor
            .layout_manager
            .layout_engine
            .windows_homed_to_display("test-display-1")
            .contains(&doomed),
        "and it must be gone from the display's affinity list"
    );
    assert_eq!(
        reactor.layout_manager.layout_engine.window_display_home(survivor),
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

/// A window owned by a space but absent from its layout tree must be reported.
///
/// That combination is invisible to the strip while remaining cmd-tab reachable, which is
/// what "a second concurrent strip" looks like from the outside.
#[test]
fn diagnostics_report_windows_missing_from_the_layout_tree() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);
    let orphan = WindowId::new(1, 1);

    set_space_membership(&[(space, &[901])]);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.add_test_app(1);
    let workspace = reactor.test_workspace(space, 0);
    reactor.add_test_window(orphan, WindowServerId::new(901), Some(space), screen);
    // Assigned to the workspace, but deliberately never added to the layout tree.
    assert!(reactor.assign_test_window_to_workspace(space, orphan, workspace));

    let diagnostics = reactor.query_diagnostics();
    let dump = diagnostics.spaces.first().expect("one space");

    assert!(
        dump.orphaned_windows.contains(&orphan.into()),
        "a window the space owns but the strip does not contain must be flagged; \
         it is reachable by cmd-tab and unreachable by scrolling"
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
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));

    let builtin_workspaces = reactor.test_workspace_ids(builtin_space);
    let external_workspaces = reactor.test_workspace_ids(external_space);
    assert_eq!(
        builtin_workspaces, external_workspaces,
        "both displays must see one shared workspace list, not a private copy each"
    );

    // Move only the external to workspace 3.
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));
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

/// Switching workspace records which way the switch travels, per display.
///
/// The animation needs this to slide the arriving strip in from the correct edge. Recording it
/// per display matters because displays switch independently: the built-in can be moving down
/// to "comms" while the external stays where it is.
#[test]
fn workspace_switch_records_its_direction_per_display() {
    let mut reactor = test_reactor();
    let builtin = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let external = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let builtin_space = SpaceId::new(1);
    let external_space = SpaceId::new(479);

    set_space_membership(&[(builtin_space, &[]), (external_space, &[])]);
    reactor.handle_event(space_state_event(
        vec![builtin, external],
        vec![Some(builtin_space), Some(external_space)],
    ));

    // Going to a higher ordinal travels DOWN the stack.
    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(2));
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .take_workspace_switch_direction(builtin_space),
        Some(crate::model::reactor::WorkspaceSwitchDirection::Down),
    );
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .take_workspace_switch_direction(external_space),
        None,
        "the other display did not switch, so it has no direction to animate"
    );

    // And back to a lower ordinal travels UP.
    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(0));
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .take_workspace_switch_direction(builtin_space),
        Some(crate::model::reactor::WorkspaceSwitchDirection::Up),
    );

    // Reading is a PEEK, not a consume: a switch runs several arrange passes and every one
    // needs the direction, otherwise the later passes cancel the slide the first one started.
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .take_workspace_switch_direction(builtin_space),
        Some(crate::model::reactor::WorkspaceSwitchDirection::Up),
        "repeated reads within one switch must return the same direction"
    );

    // Switching to the workspace already showing is not movement, and clears the direction so
    // a stale one cannot animate anything later.
    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(0));
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .take_workspace_switch_direction(builtin_space),
        None,
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
    crate::sys::window_server::set_window_spaces_override(wsid, Some(vec![builtin_space.get()]));
    window_server_appeared(&mut reactor, wsid, external_space, SpaceEventKind::User);
    crate::sys::window_server::set_window_spaces_override(wsid, None);

    assert_eq!(
        reactor.assigned_space_for_window_id(parked),
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

/// Strip navigation never walks the floating set, and returns to where the strip was.
///
/// Reported: with Zoom and System Settings floating, ctrl-J/L cycled those two rather than
/// the columns, and leaving them landed on the FIRST column instead of the one that had been
/// selected. Floating windows belong to a workspace but are not strip members; cmd-tab and
/// toggle_focus_floating reach them.
#[test]
fn strip_navigation_skips_floating_windows_and_resumes_where_it_was() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);
    let first = WindowId::new(1, 1);
    let second = WindowId::new(1, 2);
    let floater = WindowId::new(1, 3);

    set_space_membership(&[(space, &[901, 902, 903])]);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.add_test_app(1);
    let workspace = reactor.test_workspace(space, 0);
    for (window, wsid) in [(first, 901u32), (second, 902), (floater, 903)] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), screen);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspace));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
    }

    // Select the SECOND column, then make the third window floating and focus it.
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, second));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, floater));
    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    assert!(
        reactor.layout_manager.layout_engine.is_window_floating(floater),
        "test setup must make the third window floating"
    );

    // Strip navigation from the floating window must land back on the strip's selection,
    // not on the first column and not on another floating window.
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));

    let focused = reactor.layout_manager.layout_engine.focused_window();
    assert_ne!(
        focused,
        Some(floater),
        "strip navigation must leave the floating set rather than cycle within it"
    );
    assert_eq!(
        focused,
        Some(second),
        "and it must resume at the strip's own selection, not the leftmost column"
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
    reactor
        .layout_manager
        .layout_engine
        .set_window_display_home(displaced, external_space);

    // `settled` is already where it belongs.
    reactor.add_test_window(settled, WindowServerId::new(902), Some(builtin_space), builtin);
    assert!(reactor.assign_test_window_to_workspace(builtin_space, settled, workspaces[0]));
    reactor.send_layout_event(LayoutEvent::WindowAdded(builtin_space, settled));
    reactor
        .layout_manager
        .layout_engine
        .set_window_display_home(settled, builtin_space);

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
            crate::actor::raise_manager::Event::RaiseRequest(request) => {
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
        workspace: rini_protocol::WorkspaceSelector::Index(1),
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
            workspace: rini_protocol::WorkspaceSelector::Index(target),
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

/// Strip navigation stops at the edge instead of falling into the floating layer.
///
/// move_focus_internal focused the FIRST floating window whenever the strip ran out of
/// columns. With System Settings floating, walking right stepped off the last column onto
/// Settings and the next keypress came straight back — the two-window bounce that was
/// reported. Reproduced live by stepping focus right repeatedly: the fifth step landed on a
/// floating window and the sixth returned to the previous column.
#[test]
fn strip_navigation_stops_at_the_edge_rather_than_focusing_a_floating_window() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let space = SpaceId::new(1);
    let left = WindowId::new(1, 1);
    let right = WindowId::new(1, 2);
    let floater = WindowId::new(1, 3);

    set_space_membership(&[(space, &[901, 902, 903])]);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    reactor.add_test_app(1);
    let workspace = reactor.test_workspace(space, 0);
    for (window, wsid) in [(left, 901u32), (right, 902), (floater, 903)] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), screen);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspace));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
    }

    reactor.send_layout_event(LayoutEvent::WindowFocused(space, floater));
    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    assert!(reactor.layout_manager.layout_engine.is_window_floating(floater));

    // Sit on the rightmost column and keep walking right.
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, right));
    for _ in 0..3 {
        reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
        assert_ne!(
            reactor.layout_manager.layout_engine.focused_window(),
            Some(floater),
            "walking off the end of the strip must not focus a floating window"
        );
    }
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

    let (animation_tx, mut animation_rx) = actor::channel();
    reactor.communication_manager.workspace_animation_tx = Some(animation_tx);

    reactor.publish_animation_display_for(Some(built_in_space));
    let (_, published) = animation_rx.try_recv().expect("a display should be published");
    let crate::actor::workspace_animation::Event::SetDisplay { id, .. } = published else {
        panic!("expected SetDisplay, got {published:?}");
    };
    assert_eq!(id, 0, "the built-in display is screen 0, and its space is the one animating");

    reactor.publish_animation_display();
    let (_, published) = animation_rx.try_recv().expect("a display should be published");
    let crate::actor::workspace_animation::Event::SetDisplay { id, .. } = published else {
        panic!("expected SetDisplay, got {published:?}");
    };
    assert_eq!(id, 1, "with no space in mind the active display is still the right answer");
}
