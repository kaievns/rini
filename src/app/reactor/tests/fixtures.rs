//! Fixtures for the reactor's tests: the reactors, apps and windows the cases are built on.
//!
//! A fixture hands back a struct that names its parts, so a case takes the ones it needs by name.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::{WindowServerId, pid_t};
use rini_geometry::space::SurfacePoint;

use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::windows::domain::info::WindowInfo;
use crate::windows::domain::request::Request;
use crate::workspaces::{LayoutCommand, LayoutEvent};

/// A reactor with one tiled window and the spaces it is about.
pub struct OneWindow {
    pub reactor: Reactor,
    pub window: WindowId,
    pub server_id: WindowServerId,
    pub space1: SpaceId,
    pub space2: SpaceId,
    pub frame: CGRect,
}

/// [`OneWindow`] on two displays, `screen2` the second display's frame.
pub struct OneWindowTwoDisplays {
    pub reactor: Reactor,
    pub window: WindowId,
    pub server_id: WindowServerId,
    pub space1: SpaceId,
    pub space2: SpaceId,
    pub frame: CGRect,
    pub screen2: CGRect,
}

/// A reactor with one floating window on `space`, remembered at `floating_frame`.
pub struct Floating {
    pub reactor: Reactor,
    pub window: WindowId,
    pub space: SpaceId,
    pub screen: CGRect,
    pub floating_frame: CGRect,
}

/// A reactor started while its one display showed a fullscreen space over `user_space`.
pub struct FullscreenStartup {
    pub reactor: Reactor,
    pub window: WindowId,
    pub user_space: SpaceId,
    pub default_workspace: crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId,
    pub secondary_workspace: crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId,
}

/// `space1` active on a 1440x900 screen with one tiled window assigned to it at `frame`, the
/// whole screen. `space2` exists with workspaces so it can be a reassignment target.
pub fn reactor_with_window_on_space1() -> OneWindow {
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
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space1)
    );

    OneWindow {
        reactor,
        window: wid,
        server_id: wsid,
        space1,
        space2,
        frame,
    }
}

/// Two displays, the window assigned to both spaces and moved onto `space2` at `frame`, its write
/// there still pending.
pub fn reactor_with_window_moved_to_space2() -> OneWindow {
    let mut reactor = test_reactor();
    let pid = 1;
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let screen2 = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let moved_frame = CGRect::new(CGPoint::new(1600., 100.), CGSize::new(800., 600.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    let wid = WindowId::new(pid, 1);
    let wsid = WindowServerId::new(111);

    reactor.handle_event(space_state_event(vec![screen1, screen2], vec![
        Some(space1),
        Some(space2),
    ]));

    reactor.add_test_app(pid);

    let space1_workspace = reactor.test_workspace(space1, 0);
    let space2_workspace = reactor.test_workspace(space2, 0);

    reactor.add_test_window(wid, wsid, Some(space2), moved_frame);

    assert!(reactor.assign_test_window_to_workspace(space1, wid, space1_workspace));
    assert!(reactor.assign_test_window_to_workspace(space2, wid, space2_workspace));
    let txid = reactor.transactions.next_txid(wsid);
    reactor.transactions.insert(wsid, txid, moved_frame);
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2)
    );

    OneWindow {
        reactor,
        window: wid,
        server_id: wsid,
        space1,
        space2,
        frame: moved_frame,
    }
}

/// Two displays side by side, the window tiled on `space1` at `frame`.
pub fn reactor_with_window_on_space1_two_displays() -> OneWindowTwoDisplays {
    let mut reactor = test_reactor();
    let pid = 1;
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let screen2 = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let initial_frame = CGRect::new(CGPoint::new(100., 100.), CGSize::new(800., 600.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    let wid = WindowId::new(pid, 1);
    let wsid = WindowServerId::new(121);

    reactor.handle_event(space_state_event(vec![screen1, screen2], vec![
        Some(space1),
        Some(space2),
    ]));

    reactor.add_test_app(pid);

    let space1_workspace = reactor.test_workspace(space1, 0);
    let _ = reactor.test_workspace_ids(space2);

    reactor.add_test_window(wid, wsid, Some(space1), initial_frame);

    assert!(reactor.assign_test_window_to_workspace(space1, wid, space1_workspace));

    OneWindowTwoDisplays {
        reactor,
        window: wid,
        server_id: wsid,
        space1,
        space2,
        frame: initial_frame,
        screen2,
    }
}

pub fn reactor_with_floating_window() -> Floating {
    let OneWindow {
        mut reactor,
        window: wid,
        space1,
        frame: screen,
        ..
    } = reactor_with_window_on_space1();
    reactor.send_layout_event(LayoutEvent::WindowAdded(space1, wid));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space1, wid));
    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    assert!(reactor.layout_engine.is_window_floating(wid));

    let workspace = reactor.layout_engine.active_workspace(space1).expect("workspace");
    let floating_frame = CGRect::new(CGPoint::new(100., 100.), CGSize::new(400., 300.));
    if let Some(w) = reactor.state.windows.window_mut(wid) {
        w.frame_monotonic = floating_frame;
    }
    reactor
        .layout_engine
        .store_floating_position(space1, workspace, wid, floating_frame);

    Floating {
        reactor,
        window: wid,
        space: space1,
        screen,
        floating_frame,
    }
}

pub fn window_server_appeared(
    reactor: &mut Reactor,
    wsid: WindowServerId,
    space: SpaceId,
    kind: SpaceEventKind,
) {
    space_event_handler::handle_window_server_appeared(reactor, wsid, space, kind);
}

pub fn window_server_destroyed(
    reactor: &mut Reactor,
    wsid: WindowServerId,
    space: SpaceId,
    kind: SpaceEventKind,
) {
    space_event_handler::handle_window_server_destroyed(
        reactor,
        space_event_handler::WindowServerLifecyclePayload {
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
) -> FullscreenStartup {
    let mut workspace_cfg = crate::app::config::VirtualWorkspaceSettings {
        workspace_names: crate::workspaces::settings::numbered_names(2),
        ..crate::app::config::VirtualWorkspaceSettings::default()
    };
    if with_app_rule {
        workspace_cfg.app_rules = vec![crate::windows::domain::rules::WindowRule {
            open_on_workspace: Some("2".into()),
            ..crate::windows::domain::rules::WindowRule::when(
                crate::windows::domain::rules::RuleMatch::app("com.testapp1"),
            )
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

    FullscreenStartup {
        reactor,
        window: wid,
        user_space,
        default_workspace,
        secondary_workspace,
    }
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
        vec![(new_wid, WindowInfo {
            sys_id: old_info.sys_id,
            ..old_info
        })],
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
    let gaps = reactor.config.settings.layout.gaps.effective_for_display(None);
    reactor
        .layout_engine
        .calculate_layout(space, screen, &gaps)
        .iter()
        .any(|(layout_wid, _)| *layout_wid == wid)
}

pub fn test_layout(
    reactor: &mut Reactor,
    space: SpaceId,
    screen: CGRect,
) -> Vec<(WindowId, CGRect)> {
    let gaps = reactor.config.settings.layout.gaps.effective_for_display(None);
    reactor.layout_engine.calculate_layout(space, screen, &gaps)
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
    let gaps = reactor.config.settings.layout.gaps.effective_for_display(None);
    reactor
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
pub fn set_space_membership(reactor: &Reactor, entries: &[(SpaceId, &[u32])]) {
    for (space, wsids) in entries {
        reactor.window_server().set_space_window_list(space.get(), Some(wsids.to_vec()));
    }
}

/// One strip movement the reactor sent.
pub struct SurfaceFlight {
    pub windows: Vec<crate::transitions::domain::request::SurfaceWindow>,
    pub from_offset: SurfacePoint,
    pub to_offset: SurfacePoint,
    pub final_frames: Vec<(WindowId, ScreenRect)>,
    pub focus: Option<WindowId>,
    pub focused_before: Option<WindowId>,
    pub key: Option<WindowId>,
    pub cue: crate::animation::domain::choreography::Cue,
    pub accent: Option<crate::animation::domain::motion::accents::Accent>,
    pub duration: std::time::Duration,
}

/// Every strip movement the reactor sent since the last look.
pub fn strip_movements(
    rx: &mut crate::app::channels::Receiver<crate::transitions::platform::engine::Event>,
) -> Vec<SurfaceFlight> {
    let mut out = Vec::new();
    while let Ok((_, event)) = rx.try_recv() {
        if let crate::transitions::platform::engine::Event::AnimateSurface {
            windows,
            from_offset,
            to_offset,
            final_frames,
            focus,
            focused_before,
            key,
            cue,
            accent,
            duration,
            ..
        } = event
        {
            out.push(SurfaceFlight {
                windows,
                from_offset,
                to_offset,
                final_frames,
                focus,
                focused_before,
                key,
                cue,
                accent,
                duration,
            });
        }
    }
    out
}
