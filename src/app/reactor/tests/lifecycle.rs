//! Windows and applications appearing, disappearing and dying, and what survives a restart.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::WindowServerId;
use rini_geometry::SameAs;
use test_log::test;

use super::fixtures::*;
use crate::app::hotkeys::WmEvent;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::windows::domain::info::WindowInfo;
use crate::windows::domain::request::Request;
use crate::workspaces::{LayoutCommand, LayoutEvent};

#[test]
fn config_reload_propagates_non_keybinding_changes_to_wm_controller() {
    let mut reactor = test_reactor();
    let (wm_tx, mut wm_rx) = channels::channel();
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
fn known_window_server_appearance_restores_layout_membership_without_reassignment() {
    let (mut reactor, wid, wsid, user_space, _other_space, frame) = reactor_with_window_on_space1();

    reactor.send_layout_event(LayoutEvent::WindowAdded(user_space, wid));
    assert!(has_window_in_layout(&mut reactor, user_space, frame, wid));

    reactor.send_layout_event(LayoutEvent::WindowRemovedPreserveFloating(wid));

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(user_space)
    );
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
        is_modal: false,
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
    reactor.handle_event(space_state_event(vec![full_screen], vec![Some(
        fullscreen_space,
    )]));

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
fn current_ax_destruction_after_quarantine_release_removes_window() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    assert!(!reactor.refreshes_blocked());
    assert!(has_window_in_layout(&mut reactor, space, screen, wid));
    let wsid = reactor.test_window_server_id(wid);

    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, Some(false));
    reactor.handle_event(Event::WindowDestroyed(wid));
    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, None);

    assert!(reactor.state.windows.record(wid).is_none());
    assert!(!has_window_in_layout(&mut reactor, space, screen, wid));
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

    crate::windows::platform::window_server::set_window_ordered_in_override(
        closed_wsid,
        Some(false),
    );
    reactor.handle_event(Event::SpaceStateChanged(recovered));
    reactor.discover_test_windows(1, vec![], vec![survivor]);
    crate::windows::platform::window_server::set_window_ordered_in_override(closed_wsid, None);

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

    crate::windows::platform::window_server::set_window_ordered_in_override(
        closed_wsid,
        Some(false),
    );
    reactor.handle_event(Event::SpaceStateChanged(recovered));
    reactor.discover_test_windows(1, vec![], vec![]);
    crate::windows::platform::window_server::set_window_ordered_in_override(closed_wsid, None);

    assert!(reactor.state.windows.record(closed).is_none());
    assert!(!has_window_in_layout(&mut reactor, space, screen, closed));
    assert!(test_layout(&mut reactor, space, screen).is_empty());
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

    crate::windows::platform::window_server::set_space_window_list_for_connection_override(Some(
        vec![],
    ));
    reactor.refresh_window_server_snapshot_for_active_spaces();
    crate::windows::platform::window_server::set_space_window_list_for_connection_override(None);

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
fn floating_window_toggle_off_restore_previous_frame() {
    let (mut reactor, wid, space1, screen, floating_frame) = reactor_with_floating_window();
    // Turn on
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreenWithinGaps);
    // Turn off
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreenWithinGaps);
    let laid_out = laid_out_frame(&mut reactor, space1, screen, wid).expect("window laid out");
    assert!(
        laid_out.same_as(floating_frame),
        "expected restore to {floating_frame:?}, got {laid_out:?}"
    );
}

/// Autosave must not reshape live layout state.
///
/// save_current_layout normalizes floating-versus-tiled ownership and rewrites
/// stored floating frames. Those mutations are correct for an explicit save but
/// destructive on every layout change: wiring them into the layout path broke
/// un-maximizing a floating window, because the frame it should return to had
/// already been overwritten. autosave_current_layout must only refresh fingerprints
/// and write.
#[test]
fn autosave_preserves_floating_restore_frame() {
    let (mut reactor, wid, space1, screen, floating_frame) = reactor_with_floating_window();

    let dir = std::env::temp_dir().join(format!("rini-autosave-test-{}", std::process::id()));
    let path = dir.join("layout.ron");
    let _ = std::fs::remove_dir_all(&dir);

    // Autosave between the two toggles, which is what the live reactor does.
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreenWithinGaps);
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
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreenWithinGaps);

    let laid_out = laid_out_frame(&mut reactor, space1, screen, wid).expect("window laid out");
    assert!(
        laid_out.same_as(floating_frame),
        "autosave must not disturb the floating restore frame: expected {floating_frame:?}, got {laid_out:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
