//! Fixtures for the reactor's tests: the reactors, apps and windows the cases are built on.
//!
//! They were defined among the tests themselves, at lines 779 through 5103 of one file, so "how do
//! I get a reactor with a floating window" meant scrolling for it.
use objc2_core_foundation::{CGPoint, CGSize};

use super::*;
use crate::windows::domain::request::Request;
use rini_core::ids::pid_t;
use crate::workspaces::{LayoutCommand, LayoutEvent};
use crate::windows::domain::info::WindowInfo;
use rini_core::ids::WindowServerId;

/// Builds a reactor with `space1` active on a screen and a single tiled window
/// (`wid`/`wsid`) assigned to `space1`. `space2` exists with workspaces so it can
/// be a reassignment target. Returns the pieces the `appeared` tests need.
pub fn reactor_with_window_on_space1() -> (Reactor, WindowId, WindowServerId, SpaceId, SpaceId, CGRect)
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
    assert_eq!(reactor.affinity().assigned_space_for_window_id(wid), Some(space1));

    (reactor, wid, wsid, space1, space2, frame)
}

pub fn reactor_with_window_moved_to_space2()
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
    assert_eq!(reactor.affinity().assigned_space_for_window_id(wid), Some(space2));

    (reactor, wid, wsid, space1, space2, moved_frame)
}

pub fn reactor_with_window_on_space1_two_displays() -> (
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

pub fn reactor_with_floating_window() -> (Reactor, WindowId, SpaceId, CGRect, CGRect) {
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

pub fn window_server_appeared(
    reactor: &mut Reactor,
    wsid: WindowServerId,
    space: SpaceId,
    kind: SpaceEventKind,
) {
    SpaceEventHandler::handle_window_server_appeared(reactor, wsid, space, kind);
}

pub fn window_server_destroyed(
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

pub fn fullscreen_startup_fixture(
    with_app_rule: bool,
    preserve_workspace: bool,
) -> (
    Reactor,
    WindowId,
    SpaceId,
    crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId,
    crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId,
) {
    let mut workspace_cfg = crate::app::config::VirtualWorkspaceSettings {
        default_workspace_count: 2,
        ..crate::app::config::VirtualWorkspaceSettings::default()
    };
    if with_app_rule {
        workspace_cfg.app_rules = vec![crate::app::config::AppWorkspaceRule {
            app_id: Some("com.testapp1".to_string()),
            workspace: Some(crate::app::config::WorkspaceSelector::Index(1)),
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
            modal: None,
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

pub fn rekey_window(reactor: &mut Reactor, old_wid: WindowId, new_wid: WindowId) {
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

// Helper: check whether any window owned by `pid` appears in the layout tree for `space`.
pub fn has_window_in_layout(
    reactor: &mut Reactor,
    space: SpaceId,
    screen: CGRect,
    wid: WindowId,
) -> bool {
    let gaps = reactor.config.settings.layout.gaps.clone();
    reactor
        .layout_manager
        .layout_engine
        .calculate_layout(space, screen, &gaps)
        .iter()
        .any(|(layout_wid, _)| *layout_wid == wid)
}

pub fn test_layout(reactor: &mut Reactor, space: SpaceId, screen: CGRect) -> Vec<(WindowId, CGRect)> {
    let gaps = reactor.config.settings.layout.gaps.clone();
    reactor.layout_manager.layout_engine.calculate_layout(
        space,
        screen,
        &gaps,
    )
}

pub fn make_active_app(
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

pub fn make_active_app_with_count(
    apps: &mut Apps,
    reactor: &mut Reactor,
    pid: pid_t,
    window_count: usize,
    main_window: Option<WindowId>,
) {
    make_active_app(apps, reactor, pid, make_windows(window_count), main_window);
}

pub fn simulate_login_screen_refresh(apps: &mut Apps, reactor: &mut Reactor, pid: pid_t) {
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

pub fn laid_out_frame(
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
            |q| reactor.state.windows.window(q).map(|w| w.frame_monotonic),
            &[screen],
        )
        .into_iter()
        .find(|(w, _)| *w == wid)
        .map(|(_, f)| f)
}

/// Present the windows macOS reports as living on each space, the way a real snapshot
/// does. Without this the reactor sees an empty active-space query, concludes every
/// window has vanished, and drops its workspace assignment — which makes any
/// display-change test measure that instead of what it meant to.
pub fn set_space_membership(entries: &[(SpaceId, &[u32])]) {
    for (space, wsids) in entries {
        crate::windows::platform::window_server::set_space_window_list_for_space_override(
            space.get(),
            Some(wsids.to_vec()),
        );
    }
}
