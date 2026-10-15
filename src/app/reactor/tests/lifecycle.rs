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
    let mut wm_rx = reactor.connect_test_hotkeys();

    let mut updated = reactor.config().clone();
    updated.settings.focus_follows_mouse = !updated.settings.focus_follows_mouse;
    updated.settings.mouse_follows_focus = !updated.settings.mouse_follows_focus;

    reactor.handle_event(Event::ConfigUpdated(Box::new(updated.clone())));

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
}

/// The engine writes nothing until it hears, so the reactor tells it as it is connected: the setting
/// reaches the engine from the first transition, with no config reload.
#[test]
fn the_transitions_engine_is_told_whether_to_write_its_journal_as_it_is_connected() {
    let mut reactor = test_reactor();
    let (transitions_tx, mut transitions_rx) = channels::channel();
    reactor.connect_transitions(Some(transitions_tx));

    let mut told = Vec::new();
    while let Ok((_, event)) = transitions_rx.try_recv() {
        if let crate::transitions::platform::engine::Event::Journal(settings) = event {
            told.push(settings);
        }
    }
    assert_eq!(told, [reactor.settings().journal]);
    assert!(!told[0].write, "not written by default");
}

#[test]
fn a_config_reload_tells_the_transitions_engine_whether_to_write_its_journal() {
    let mut reactor = test_reactor();
    let mut transitions_rx = reactor.connect_test_transitions();

    let mut updated = reactor.config().clone();
    updated.settings.journal.write = true;
    reactor.handle_event(Event::ConfigUpdated(Box::new(updated)));

    let mut told = Vec::new();
    while let Ok((_, event)) = transitions_rx.try_recv() {
        if let crate::transitions::platform::engine::Event::Journal(settings) = event {
            told.push(settings.write);
        }
    }
    assert_eq!(told, [true]);
}

#[test]
fn known_window_server_appearance_restores_layout_membership_without_reassignment() {
    let OneWindow {
        mut reactor,
        window: wid,
        server_id: wsid,
        space1: user_space,
        frame,
        ..
    } = reactor_with_window_on_space1();

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

    reactor.window_server_appeared(wsid, user_space, SpaceEventKind::User);

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

    reactor.quarantine_mut().sleeping = true;
    assert!(reactor.should_quarantine_space_lifecycle_event(&Event::SpaceCreated(space)));

    reactor.quarantine_mut().sleeping = false;
    reactor.quarantine_mut().session_inactive = true;
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
        reactor.windows().window(wid).is_some_and(|window| !window.info.is_minimized),
        "reactor state must clear the minimized flag after restore"
    );
}

#[test]
fn wake_quarantine_waits_for_fresh_space_snapshot_before_refresh() {
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

    reactor.window_server().set_window_ordered_in(wsid, Some(false));
    reactor.handle_event(Event::WindowDestroyed(wid));

    assert!(reactor.windows().record(wid).is_none());
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
    reactor.quarantine_mut().display_churn_active = true;

    reactor.handle_event(Event::WindowDestroyed(wid));

    assert!(
        reactor.windows().window(wid).is_some(),
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

    reactor.window_server().set_window_ordered_in(closed_wsid, Some(false));
    reactor.handle_event(Event::SpaceStateChanged(recovered));
    reactor.discover_test_windows(1, vec![], vec![survivor]);

    assert!(reactor.windows().record(closed).is_none());
    assert!(!has_window_in_layout(&mut reactor, space, screen, closed));
    assert!(reactor.windows().contains_window(survivor));
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

    reactor.window_server().set_window_ordered_in(closed_wsid, Some(false));
    reactor.handle_event(Event::SpaceStateChanged(recovered));
    reactor.discover_test_windows(1, vec![], vec![]);

    assert!(reactor.windows().record(closed).is_none());
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

    reactor.window_server().set_any_space_window_list(Some(vec![]));
    reactor.refresh_window_server_snapshot_for_active_spaces();

    assert!(
        reactor.windows().is_window_visible(wsid),
        "a transient empty active-space WS-id result after wake must not blank windows we already know belong to the active space"
    );
    assert!(
        has_window_in_layout(&mut reactor, space, screen, wid),
        "preserving the visibility basis must also preserve the active workspace layout until discovery catches up"
    );
}

#[test]
fn floating_window_toggle_off_restore_previous_frame() {
    let Floating {
        mut reactor,
        window: wid,
        space: space1,
        screen,
        floating_frame,
    } = reactor_with_floating_window();
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
    let Floating {
        mut reactor,
        window: wid,
        space: space1,
        screen,
        floating_frame,
    } = reactor_with_floating_window();

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
    let _ = reactor.autosave_test_layout(path.clone(), active);
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreenWithinGaps);

    let laid_out = laid_out_frame(&mut reactor, space1, screen, wid).expect("window laid out");
    assert!(
        laid_out.same_as(floating_frame),
        "autosave must not disturb the floating restore frame: expected {floating_frame:?}, got {laid_out:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The autosave is where a space id macOS has stopped using leaves the file.
#[test]
fn an_autosave_forgets_a_space_the_display_moved_off_and_left_empty() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let (old, new) = (SpaceId::new(1), SpaceId::new(2));
    let dir = std::env::temp_dir().join(format!("rini-vacant-autosave-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    reactor.set_test_autosave_path(dir.join("layout.ron"));

    reactor.handle_event(space_state_event(vec![screen], vec![Some(old)]));
    reactor.handle_event(space_state_event(vec![screen], vec![Some(new)]));
    let spaces = || reactor.layout().virtual_workspace_manager().initialized_spaces();
    assert!(
        spaces().contains(&old),
        "the fixture has the old space to forget"
    );

    reactor.save_layout_now();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        reactor.layout().virtual_workspace_manager().initialized_spaces(),
        vec![new]
    );
}

#[test]
fn an_autosave_keeps_an_empty_desktop_macos_still_lists_off_screen() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let (other, shown) = (SpaceId::new(1), SpaceId::new(2));
    let dir = std::env::temp_dir().join(format!("rini-listed-autosave-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    reactor.set_test_autosave_path(dir.join("layout.ron"));

    reactor.handle_event(space_state_event(vec![screen], vec![Some(other)]));
    reactor.handle_event(space_state_event_with(
        vec![screen],
        vec![Some(shown)],
        |state| {
            state.display_space_ids.insert("test-display-0".into(), vec![other, shown]);
        },
    ));
    reactor.save_layout_now();
    let _ = std::fs::remove_dir_all(&dir);

    let mut spaces = reactor.layout().virtual_workspace_manager().initialized_spaces();
    spaces.sort_unstable();
    assert_eq!(spaces, vec![other, shown]);
}

#[test]
fn the_quit_save_keeps_an_empty_desktop_macos_still_lists_off_screen() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let (other, shown) = (SpaceId::new(1), SpaceId::new(2));
    let dir = std::env::temp_dir().join(format!("rini-listed-quit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    reactor.handle_event(space_state_event(vec![screen], vec![Some(other)]));
    reactor.handle_event(space_state_event_with(
        vec![screen],
        vec![Some(shown)],
        |state| {
            state.display_space_ids.insert("test-display-0".into(), vec![other, shown]);
        },
    ));
    let saved = reactor.save_layout_before_exit(dir.join("layout.ron"));
    let _ = std::fs::remove_dir_all(&dir);

    saved.unwrap();
    let mut spaces = reactor.layout().virtual_workspace_manager().initialized_spaces();
    spaces.sort_unstable();
    assert_eq!(spaces, vec![other, shown]);
}

/// Without the window server's list only the screens' current spaces are known, and an empty
/// Desktop off screen is not among them.
#[test]
fn a_save_forgets_nothing_when_the_window_server_did_not_list_the_spaces() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let (other, shown) = (SpaceId::new(1), SpaceId::new(2));
    let dir = std::env::temp_dir().join(format!("rini-unlisted-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    reactor.set_test_autosave_path(dir.join("autosave.ron"));

    reactor.handle_event(space_state_event(vec![screen], vec![Some(other)]));
    reactor.handle_event(space_state_event_with(
        vec![screen],
        vec![Some(shown)],
        |state| {
            state.display_space_ids.insert("test-display-0".into(), vec![shown]);
            state.display_space_ids_from_screens = true;
        },
    ));
    reactor.save_layout_now();
    let saved = reactor.save_layout_before_exit(dir.join("layout.ron"));
    let _ = std::fs::remove_dir_all(&dir);

    saved.unwrap();
    let mut spaces = reactor.layout().virtual_workspace_manager().initialized_spaces();
    spaces.sort_unstable();
    assert_eq!(spaces, vec![other, shown]);
}

#[test]
fn the_quit_save_forgets_a_space_the_display_moved_off_and_left_empty() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let (old, new) = (SpaceId::new(1), SpaceId::new(2));
    let dir = std::env::temp_dir().join(format!("rini-vacant-quit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    reactor.handle_event(space_state_event(vec![screen], vec![Some(old)]));
    reactor.handle_event(space_state_event(vec![screen], vec![Some(new)]));
    let saved = reactor.save_layout_before_exit(dir.join("layout.ron"));
    let _ = std::fs::remove_dir_all(&dir);

    saved.unwrap();
    assert_eq!(
        reactor.layout().virtual_workspace_manager().initialized_spaces(),
        vec![new]
    );
}

/// A discovery hands work back mid-drain, the layout events that add its window. They are sent
/// where it hands them back, so the pass the added window's layout event runs goes ahead of this
/// outcome's frame write ahead of the layout, which comes after the discovery, and the deferred
/// request still goes last.
#[test]
fn a_discovery_lays_its_window_out_where_it_hands_it_back() {
    let (mut apps, mut reactor) = test_context();
    let space = SpaceId::new(1);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    apps.requests();

    let (first, added) = (WindowId::new(1, 1), WindowId::new(1, 3));
    let mut info = make_window(3);
    let wsid = WindowServerId::new(10_003);
    info.sys_id = Some(wsid);
    reactor.track_test_window_server_info(wsid, 1, info.frame);
    reactor.windows_mut().set_window_server_space(wsid, Some(space));
    let placed = CGRect::new(CGPoint::new(5., 5.), CGSize::new(300., 200.));
    let outcome = EventOutcome::layout_changed(false)
        .with_discovery(crate::app::reactor::events::WindowDiscoveryRequest {
            pid: 1,
            new: vec![(added, info)],
            known_visible: vec![first, WindowId::new(1, 2), added],
            app_info: None,
        })
        .with_pre_layout_window_frame_write(first, placed)
        .with_app_request(1, Request::GetVisibleWindows);
    reactor.apply_event_outcome(outcome);

    let sent: Vec<_> = apps
        .requests()
        .into_iter()
        .map(|request| match request {
            Request::SetWindowFrame { window, frame, .. } => (Some(window), Some(frame == placed)),
            Request::GetVisibleWindows => (None, None),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(sent, [
        (Some(added), Some(false)),
        (Some(first), Some(true)),
        (None, None)
    ]);
}
