//! Who has focus and why: focus moves, focus-follows-mouse, raise echoes, cmd-tab, and the
//! per-application main window.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::WindowServerId;
use test_log::test;

use super::fixtures::*;
use crate::app::config::WorkspaceSelector;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::windows::domain::request::Request;
use crate::workspaces::{Direction, LayoutCommand, LayoutEvent};

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
fn passive_command_space_change_does_not_override_clicked_window_focus() {
    let (mut apps, mut reactor) = test_context();
    let (raise_manager_tx, mut raise_manager_rx) = channels::channel();
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
fn handle_layout_response_includes_handles_for_raise_and_focus_windows() {
    let (mut apps, mut reactor) = test_context();
    let (raise_manager_tx, mut raise_manager_rx) = channels::channel();
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
            edge_hit: None,
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
fn menu_open_state_is_cleared_when_owner_deactivates() {
    let mut reactor = test_reactor();
    let (event_tap_tx, mut event_tap_rx) = channels::channel();
    reactor.communication_manager.event_tap_tx = Some(event_tap_tx);

    reactor.handle_event(Event::MenuOpened(1));
    let disable = event_tap_rx.try_recv().expect("menu-open should update event tap").1;
    assert!(matches!(
        disable,
        crate::input::platform::input_tap::Request::SetFocusFollowsMouseEnabled(false)
    ));
    assert_eq!(reactor.menu_manager.menu_state, MenuState::Open(1));

    reactor.handle_event(Event::ApplicationDeactivated(1));
    let enable = event_tap_rx
        .try_recv()
        .expect("app deactivation should re-enable focus-follows-mouse")
        .1;
    assert!(matches!(
        enable,
        crate::input::platform::input_tap::Request::SetFocusFollowsMouseEnabled(true)
    ));
    assert_eq!(reactor.menu_manager.menu_state, MenuState::Closed);
}

#[test]
fn stale_menu_open_state_is_cleared_when_other_app_activates() {
    let mut reactor = test_reactor();
    let (event_tap_tx, mut event_tap_rx) = channels::channel();
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
        crate::input::platform::input_tap::Request::SetFocusFollowsMouseEnabled(true)
    ));
    assert_eq!(reactor.menu_manager.menu_state, MenuState::Closed);
}

#[test]
fn same_app_focus_change_hides_mouse_and_window_server_confirmation_reasserts_it() {
    let (mut apps, mut reactor) = test_context();
    let (event_tap_tx, mut event_tap_rx) = channels::channel();
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
    assert!(matches!(
        request,
        crate::input::platform::input_tap::Request::HideOnFocus
    ));

    reactor.handle_event(Event::WindowServerFocusChanged(second, space));

    let request = event_tap_rx
        .try_recv()
        .expect("WindowServer focus confirmation should reassert hidden mouse")
        .1;
    assert!(matches!(
        request,
        crate::input::platform::input_tap::Request::EnforceHidden
    ));
}

/// The strip is one z-order group (`model::z_group`): focus landing on a strip window while a floating
/// window sits in front of any strip window raises the whole strip over it.
/// The window an auto workspace switch focuses (`choose_switch_focus`): the window the switch is
/// for is taken visible or not, because it is parked until the layout lands. A cmd-tab onto Kiro
/// (parked on workspace 2, reported off screen by the window server) landed on a Ghostty window
/// this way and remembered Ghostty for the next time.
mod switch_focus {
    use crate::app::reactor::{choose_switch_focus, switch_focus_within_workspace};

    /// macOS picks the app's main window on cmd-tab. When the user was last in another window of
    /// the app on the same workspace, that one is the switch's focus; otherwise the pick stands.
    #[test]
    fn the_window_the_user_was_in_beats_the_pick_on_the_same_workspace() {
        assert_eq!(
            switch_focus_within_workspace("kiro-far", Some("kiro-near")),
            "kiro-near"
        );
        assert_eq!(switch_focus_within_workspace("kiro-far", None), "kiro-far");
    }

    #[test]
    fn the_window_the_switch_is_for_wins_even_when_the_server_reports_it_off_screen() {
        assert_eq!(
            choose_switch_focus(Some("kiro"), true, Some("ghostty"), Some("ghostty")),
            Some("kiro")
        );
        assert_eq!(choose_switch_focus(Some("kiro"), true, None, None), Some("kiro"));
    }

    #[test]
    fn a_preferred_window_outside_the_workspace_falls_back_to_the_remembered_then_first_visible() {
        assert_eq!(
            choose_switch_focus(Some("kiro"), false, Some("ghostty"), Some("word")),
            Some("ghostty")
        );
        assert_eq!(
            choose_switch_focus(Some("kiro"), false, None, Some("word")),
            Some("word")
        );
        assert_eq!(
            choose_switch_focus(None, false, Some("ghostty"), Some("word")),
            Some("ghostty")
        );
        assert_eq!(choose_switch_focus::<&str>(None, false, None, None), None);
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

/// A window renders differently focused, and none of the difference is a size change: measured on a 1pt
/// window border, 65 of 255 focused against 42 unfocused. The ordinary warm only recaptures a window whose
/// picture no longer fits its frame, so without this both tiles keep the wrong focus state and the border
/// pops when the overlay lifts.
#[test]
fn a_focus_change_asks_for_fresh_pictures_of_both_windows() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    let leaving = WindowId::new(1, 1);
    let arriving = WindowId::new(1, 2);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, leaving));

    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.workspace_animation_tx = Some(animation_tx);

    reactor.handle_event(Event::WindowServerFocusChanged(arriving, space));

    let mut refreshed = Vec::new();
    while let Ok((_, event)) = animation_rx.try_recv() {
        if let crate::animation::platform::engine::Event::RefreshFocus(target) = event {
            refreshed.push(target.window);
        }
    }
    assert!(
        refreshed.contains(&arriving),
        "the window gaining focus: {refreshed:?}"
    );
    assert!(
        refreshed.contains(&leaving),
        "the window losing it: {refreshed:?}"
    );
}

/// Focus landing where it already is changes no appearance, so it is not worth a capture.
#[test]
fn focus_that_does_not_move_asks_for_nothing() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    let window = WindowId::new(1, 1);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(1));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, window));

    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.workspace_animation_tx = Some(animation_tx);
    reactor.refresh_focus_pictures(window);

    let mut refreshed = Vec::new();
    while let Ok((_, event)) = animation_rx.try_recv() {
        if let crate::animation::platform::engine::Event::RefreshFocus(target) = event {
            refreshed.push(target.window);
        }
    }
    assert_eq!(
        refreshed,
        vec![window],
        "only the window itself, not a phantom second one"
    );
}

/// Pushing past an end bounces the view instead of doing nothing: focus right at the last column
/// nudges the strip left; the previous workspace at the top of the stack nudges the row up. Focus
/// stays where it was in both cases. See "Edge bounce" in `src/animation/docs/animation-smoothness.md`.
#[test]
fn pushing_past_an_end_bounces_the_strip_and_keeps_focus() {
    use crate::animation::domain::motion::plan::EDGE_BOUNCE_OVERSHOOT;
    use crate::animation::platform::engine::Event as Anim;
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.config.settings.animate = true;
    let mut settings = reactor.config.virtual_workspaces.clone();
    settings.prevent_wrapping = true;
    reactor
        .layout_manager
        .layout_engine
        .update_virtual_workspace_settings(&settings);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    let last_column = WindowId::new(1, 2);
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, last_column));
    apps.requests();

    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.workspace_animation_tx = Some(animation_tx);
    let bounces = |rx: &mut channels::Receiver<Anim>| {
        let mut out = Vec::new();
        while let Ok((_, event)) = rx.try_recv() {
            if let Anim::Bounce { overshoot, windows, .. } = event {
                out.push((overshoot, windows.len()));
            }
        }
        out
    };

    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    assert_eq!(
        bounces(&mut animation_rx),
        vec![(CGPoint::new(-EDGE_BOUNCE_OVERSHOOT, 0.0), 2)],
        "the strip's end: one bounce to the left carrying both columns"
    );
    assert_eq!(
        reactor.layout_manager.layout_engine.focused_window(),
        Some(last_column)
    );

    reactor.handle_test_layout_command(LayoutCommand::PrevWorkspace(None));
    assert_eq!(
        bounces(&mut animation_rx),
        vec![(CGPoint::new(0.0, EDGE_BOUNCE_OVERSHOOT), 2)],
        "the top of the stack: one bounce downward"
    );
    assert_eq!(
        reactor.layout_manager.layout_engine.focused_window(),
        Some(last_column)
    );

    // A step that lands somewhere is a switch, not a bounce.
    reactor.handle_test_layout_command(LayoutCommand::NextWorkspace(None));
    assert!(bounces(&mut animation_rx).is_empty());
}

/// A raise walks the whole workspace and macOS reports a focus change for every window it touches. Taking
/// those at face value moved the layout's selection down the raise list, and the strip scrolled to each in
/// turn: eight scroll targets from one keypress, ending where it started.
#[test]
fn a_focus_report_from_rinis_own_raise_does_not_move_the_selection() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    let intended = WindowId::new(1, 1);
    let echoed = WindowId::new(1, 2);

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, intended));
    assert_eq!(
        reactor.layout_manager.layout_engine.focused_window(),
        Some(intended)
    );

    reactor.raise_echo.expect(
        [intended, echoed].into_iter(),
        Some(intended),
        std::time::Instant::now(),
    );
    reactor.handle_event(Event::WindowServerFocusChanged(echoed, space));
    assert_eq!(
        reactor.layout_manager.layout_engine.focused_window(),
        Some(intended),
        "the echo must not become the selection"
    );

    // The user going somewhere is still honoured, even to a window the same raise touched, once the
    // cascade is over.
    reactor.raise_echo = crate::windows::domain::focus::RaiseEcho::default();
    reactor.handle_event(Event::WindowServerFocusChanged(echoed, space));
    assert_eq!(
        reactor.layout_manager.layout_engine.focused_window(),
        Some(echoed)
    );
}

mod main_window_tracking {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use test_log::test;

    use crate::app::reactor::testing::{Apps, make_windows, space_state_event};
    use crate::app::reactor::{Event, Quiet, Reactor, SpaceId, WindowId};
    use crate::workspaces::LayoutEngine;

    #[test]
    fn it_tracks_frontmost_app_and_main_window_correctly() {
        use Event::*;
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutEngine::new(
            &crate::app::config::VirtualWorkspaceSettings::default(),
            &crate::app::config::LayoutSettings::default(),
        ));
        let space = SpaceId::new(1);
        let screen_frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1920., 1080.));
        reactor.handle_event(space_state_event(vec![screen_frame], vec![Some(space)]));
        assert_eq!(None, reactor.main_window());

        reactor.handle_event(ApplicationGloballyActivated(1));
        reactor.handle_events(apps.make_app_with_opts(
            1,
            make_windows(2),
            Some(WindowId::new(1, 1)),
            true,
            true,
        ));
        reactor.handle_events(apps.make_app_with_opts(2, make_windows(2), None, false, true));
        assert_eq!(Some(WindowId::new(1, 1)), reactor.main_window());
        assert_eq!(
            reactor.layout_manager.layout_engine.selected_window(space),
            Some(WindowId::new(1, 1))
        );

        reactor.handle_event(ApplicationGloballyDeactivated(1));
        assert_eq!(None, reactor.main_window());
        reactor.handle_event(ApplicationActivated(2, Quiet::No));
        reactor.handle_event(ApplicationGloballyActivated(2));
        assert_eq!(None, reactor.main_window());
        reactor.handle_event(ApplicationMainWindowChanged(
            2,
            Some(WindowId::new(2, 2)),
            Quiet::No,
        ));
        assert_eq!(Some(WindowId::new(2, 2)), reactor.main_window());
        assert_eq!(
            reactor.layout_manager.layout_engine.selected_window(space),
            Some(WindowId::new(2, 2))
        );
        reactor.handle_event(ApplicationMainWindowChanged(
            1,
            Some(WindowId::new(1, 2)),
            Quiet::No,
        ));
        assert_eq!(Some(WindowId::new(2, 2)), reactor.main_window());
        reactor.handle_event(ApplicationDeactivated(1));
        assert_eq!(Some(WindowId::new(2, 2)), reactor.main_window());
        reactor.handle_event(ApplicationDeactivated(2));
        assert_eq!(None, reactor.main_window());

        reactor.handle_event(ApplicationGloballyActivated(3));
        assert_eq!(None, reactor.main_window());

        reactor.handle_events(apps.make_app_with_opts(
            3,
            make_windows(2),
            Some(WindowId::new(3, 1)),
            true,
            true,
        ));
        assert_eq!(Some(WindowId::new(3, 1)), reactor.main_window());
        assert_eq!(
            reactor.layout_manager.layout_engine.selected_window(space),
            Some(WindowId::new(3, 1))
        );
    }

    #[test]
    fn it_does_not_update_layout_for_quiet_raises() {
        use Event::*;
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutEngine::new(
            &crate::app::config::VirtualWorkspaceSettings::default(),
            &crate::app::config::LayoutSettings::default(),
        ));
        let space = SpaceId::new(1);
        let screen_frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1920., 1080.));
        reactor.handle_event(space_state_event(vec![screen_frame], vec![Some(space)]));

        reactor.handle_event(ApplicationGloballyActivated(1));
        reactor.handle_events(apps.make_app_with_opts(
            1,
            make_windows(2),
            Some(WindowId::new(1, 1)),
            true,
            true,
        ));
        reactor.handle_events(apps.make_app_with_opts(2, make_windows(2), None, false, true));
        assert_eq!(Some(WindowId::new(1, 1)), reactor.main_window());
        assert_eq!(
            reactor.layout_manager.layout_engine.selected_window(space),
            Some(WindowId::new(1, 1))
        );

        reactor.handle_event(ApplicationGloballyDeactivated(1));
        assert_eq!(None, reactor.main_window());
        reactor.handle_event(ApplicationGloballyActivated(2));
        reactor.handle_event(ApplicationActivated(2, Quiet::Yes));
        assert_eq!(None, reactor.main_window());
        reactor.handle_event(ApplicationMainWindowChanged(
            2,
            Some(WindowId::new(2, 2)),
            Quiet::Yes,
        ));
        assert_eq!(Some(WindowId::new(2, 2)), reactor.main_window());
        assert_eq!(
            reactor.layout_manager.layout_engine.selected_window(space),
            Some(WindowId::new(1, 1))
        );

        reactor.handle_event(ApplicationActivated(2, Quiet::No));
        assert_eq!(
            reactor.layout_manager.layout_engine.selected_window(space),
            Some(WindowId::new(2, 2))
        );

        reactor.handle_event(ApplicationMainWindowChanged(
            2,
            Some(WindowId::new(2, 1)),
            Quiet::Yes,
        ));
        assert_eq!(Some(WindowId::new(2, 1)), reactor.main_window());
        assert_eq!(
            reactor.layout_manager.layout_engine.selected_window(space),
            Some(WindowId::new(2, 2))
        );

        reactor.handle_event(ApplicationActivated(1, Quiet::Yes));
        reactor.handle_event(ApplicationGloballyActivated(1));
        assert_eq!(Some(WindowId::new(1, 1)), reactor.main_window());
        assert_eq!(
            reactor.layout_manager.layout_engine.selected_window(space),
            Some(WindowId::new(2, 2))
        );

        reactor.handle_event(ApplicationMainWindowChanged(
            1,
            Some(WindowId::new(1, 2)),
            Quiet::No,
        ));
        assert_eq!(Some(WindowId::new(1, 2)), reactor.main_window());
        assert_eq!(
            reactor.layout_manager.layout_engine.selected_window(space),
            Some(WindowId::new(1, 2))
        );
    }

    #[test]
    fn it_selects_main_window_when_space_is_enabled() {
        use Event::*;
        let mut apps = Apps::new();
        let mut reactor = Reactor::new_for_test(LayoutEngine::new(
            &crate::app::config::VirtualWorkspaceSettings::default(),
            &crate::app::config::LayoutSettings::default(),
        ));
        let pid = 3;
        let windows = make_windows(2);
        let space = SpaceId::new(1);
        let screen_frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1920., 1080.));
        reactor.handle_event(space_state_event(vec![screen_frame], vec![Some(space)]));

        reactor.handle_events(apps.make_app_with_opts(
            pid,
            windows,
            Some(WindowId::new(3, 1)),
            false,
            true,
        ));

        reactor.handle_event(space_state_event(vec![screen_frame], vec![None]));
        reactor.handle_event(ApplicationActivated(3, Quiet::No));
        reactor.handle_event(ApplicationGloballyActivated(3));
        reactor.handle_event(WindowsDiscovered {
            pid,
            new: vec![],
            known_visible: vec![WindowId::new(3, 1), WindowId::new(3, 2)],
        });
        assert_eq!(Some(WindowId::new(3, 1)), reactor.main_window());

        reactor.handle_event(space_state_event(vec![screen_frame], vec![Some(space)]));
        assert_eq!(
            reactor.layout_manager.layout_engine.selected_window(space),
            Some(WindowId::new(3, 1))
        );
    }
}
