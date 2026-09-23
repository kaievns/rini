//! macOS spaces: which space is current, what rini does while it cannot tell, and how a space's
//! window membership is resolved.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::{WindowServerId, pid_t};
use test_log::test;

use super::fixtures::*;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::displays::platform::spaces::SpaceKinds;
use crate::workspaces::LayoutCommand;

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

    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(space1),
        Some(space2),
    ]));

    assert!(reactor.is_space_active(space1));
    assert!(
        !reactor.is_space_active(space2),
        "forwarded raw active spaces must not bypass one_space filtering"
    );
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
fn best_space_prefers_authoritative_window_server_space_over_geometry() {
    let mut reactor = test_reactor();
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    let wid = WindowId::new(1, 1);
    let wsid = WindowServerId::new(11);

    reactor.handle_event(space_state_event(vec![frame], vec![Some(space2)]));
    reactor.insert_test_window(wid, wsid, Some(space1), frame, true);

    assert_eq!(reactor.affinity().best_space_for_window_id(wid), Some(space1));
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

    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, Some(true));
    window_server_destroyed(&mut reactor, wsid, space1, SpaceEventKind::User);
    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, None);

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

    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, Some(false));
    window_server_destroyed(&mut reactor, wsid, space1, SpaceEventKind::User);
    crate::windows::platform::window_server::set_window_ordered_in_override(wsid, None);

    assert!(!reactor.state.windows.contains_window(wid));
    assert_eq!(reactor.state.windows.tracked_window_id(wsid), None);
    assert_eq!(reactor.affinity().assigned_space_for_window_id(wid), None);
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
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space1)
    );
    assert_eq!(
        reactor.affinity().authoritative_space_for_window_id(wid),
        Some(space1),
        "late appearance from the old display should be ignored once Rini has already committed the new server-space target"
    );
}

#[test]
fn stale_user_space_appearance_is_ignored_when_authoritative_window_space_differs() {
    let (mut reactor, wid, wsid, space1, space2, _frame) = reactor_with_window_moved_to_space2();
    crate::windows::platform::window_server::set_window_spaces_override(
        wsid,
        Some(vec![space2.get()]),
    );

    window_server_appeared(&mut reactor, wsid, space1, SpaceEventKind::User);

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
fn central_space_resolution_prefers_recent_move_target_over_stale_server_space() {
    let (mut reactor, wid, wsid, space1, space2, moved_frame) =
        reactor_with_window_moved_to_space2();

    reactor.state.windows.set_window_server_space(wsid, Some(space1));

    assert_eq!(
        reactor.affinity().authoritative_space_for_window_id(wid),
        Some(space2)
    );
    assert_eq!(
        reactor.affinity().best_space_for_window(&moved_frame, Some(wsid)),
        Some(space2),
        "core space resolution should prefer the recent move target when geometry and assignment agree"
    );
}

#[test]
fn active_space_membership_refresh_does_not_overwrite_recent_move_target() {
    let (mut reactor, wid, wsid, space1, space2, _) = reactor_with_window_moved_to_space2();

    reactor.refresh_active_space_window_membership(vec![(wsid, Some(space1))]);

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space2)
    );
    assert_eq!(
        reactor.state.windows.window_server_space(wsid),
        Some(space2),
        "active-space reconciliation must not overwrite a recent cross-display move with stale membership"
    );
    assert!(reactor.state.windows.is_window_visible(wsid));
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
    crate::windows::platform::window_server::set_window_spaces_override(
        moved_wsid,
        Some(vec![inactive_space.get()]),
    );

    reactor.reconcile_authoritative_active_window_snapshot(
        vec![(retained_wsid, Some(active_space))],
        false,
    );

    crate::windows::platform::window_server::set_window_spaces_override(moved_wsid, None);

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(moved),
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
        reactor.affinity().assigned_space_for_window_id(retained),
        Some(active_space),
        "other visible windows on the active space must remain untouched"
    );
}

#[test]
fn authoritative_active_space_membership_comes_from_space_window_ids_directly() {
    let mut reactor = test_reactor();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wsid_a = WindowServerId::new(41);
    let wsid_b = WindowServerId::new(42);

    crate::windows::platform::window_server::set_space_window_list_for_connection_override(Some(
        vec![wsid_a.as_u32(), wsid_b.as_u32()],
    ));

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    let snapshot = reactor.authoritative_active_space_windows();

    crate::windows::platform::window_server::set_space_window_list_for_connection_override(None);

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

    crate::windows::platform::window_server::set_space_window_list_for_space_override(
        space1.get(),
        Some(vec![wsid_left.as_u32()]),
    );
    crate::windows::platform::window_server::set_space_window_list_for_space_override(
        space2.get(),
        Some(vec![wsid_right.as_u32()]),
    );
    crate::windows::platform::window_server::set_window_spaces_override(
        wsid_left,
        Some(vec![space1.get()]),
    );
    crate::windows::platform::window_server::set_window_spaces_override(
        wsid_right,
        Some(vec![space2.get()]),
    );

    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(space1),
        Some(space2),
    ]));
    let mut snapshot = reactor.authoritative_active_space_windows();

    crate::windows::platform::window_server::set_space_window_list_for_space_override(
        space1.get(),
        None,
    );
    crate::windows::platform::window_server::set_space_window_list_for_space_override(
        space2.get(),
        None,
    );
    crate::windows::platform::window_server::set_window_spaces_override(wsid_left, None);
    crate::windows::platform::window_server::set_window_spaces_override(wsid_right, None);

    snapshot.sort_unstable_by_key(|(wsid, _)| wsid.as_u32());
    assert_eq!(
        snapshot,
        vec![(wsid_left, Some(space1)), (wsid_right, Some(space2))],
        "multi-display active-space membership should be collected per active space so stale union snapshots do not keep windows visible after topology changes"
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
            reactor.affinity().resolve_native_space(wsid, Some(space1)),
            Some(space2),
        ));
    }

    // A direct observation of the target confirms the pending move.
    {
        let (reactor, _wid, wsid, _space1, space2, _) = reactor_with_window_moved_to_space2();
        let resolved = reactor.affinity().resolve_native_space(wsid, Some(space2));
        reactor.clear_pending_target_if_confirmed_space(wsid, space2);
        cases.push(("confirmed target", resolved, Some(space2)));
    }

    // With no pending Rini move, a live WindowServer observation is an external move.
    {
        let (reactor, _wid, wsid, _space1, space2, _) = reactor_with_window_on_space1();
        crate::windows::platform::window_server::set_window_spaces_override(
            wsid,
            Some(vec![space2.get()]),
        );
        let resolved = reactor.affinity().resolve_native_space(wsid, Some(space2));
        crate::windows::platform::window_server::set_window_spaces_override(wsid, None);
        cases.push(("newer external move", resolved, Some(space2)));
    }

    // With only an accepted prior observation, a partial sample keeps it.
    {
        let (reactor, _wid, wsid, space1, _space2, _) = reactor_with_window_on_space1();
        cases.push((
            "partial observation",
            reactor.affinity().resolve_native_space(wsid, None),
            Some(space1),
        ));
    }

    // Geometry is used only when no native or prior WindowServer state exists.
    {
        let mut reactor = test_reactor();
        let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
        let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
        let space2 = SpaceId::new(2);
        reactor.handle_event(space_state_event(vec![left, right], vec![
            Some(SpaceId::new(1)),
            Some(space2),
        ]));
        let frame = CGRect::new(CGPoint::new(1200., 100.), CGSize::new(400., 400.));
        cases.push((
            "geometry fallback",
            reactor
                .affinity()
                .best_space_for_window(&frame, Some(WindowServerId::new(9999))),
            Some(space2),
        ));
    }

    for (case, resolved, expected) in cases {
        assert_eq!(resolved, expected, "resolver case: {case}");
    }
}

/// The id `NON_USER_SPACE` is a login or system space: `SLSSpaceGetType != 0`.
const NON_USER_SPACE: u64 = 4242;

fn classifier_with_a_non_user_space() -> SpaceKinds {
    SpaceKinds {
        is_fullscreen: |_| false,
        is_user: |space| space.get() != NON_USER_SPACE,
    }
}

/// A window that vanished from the active space follows macOS to wherever it went — but only if
/// that is a real space. A login or fullscreen space is transient native state, and assigning a
/// window to one strands it somewhere the user cannot reach.
///
/// This rule was unreachable from a test until the classifier became injectable: the branch here
/// read `#[cfg(test)] { true }`, so under test there was no such thing as a non-user space.
#[test]
fn a_window_is_not_followed_onto_a_non_user_space() {
    let (mut reactor, wid, wsid, active_space, _other, _frame) = reactor_with_window_on_space1();
    reactor.space_kinds = classifier_with_a_non_user_space();
    let login_space = SpaceId::new(NON_USER_SPACE);

    // macOS now reports the window on a login space, and it is gone from the active snapshot.
    reactor.state.windows.set_window_server_space(wsid, Some(login_space));
    reactor.reconcile_authoritative_active_window_snapshot(vec![], true);

    assert_ne!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(login_space),
        "a login space is not somewhere a window can be assigned"
    );
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(active_space),
        "so it keeps the space it had"
    );
}

/// The same setup with an ordinary space, which is what makes the test above a test of the rule
/// rather than of the snapshot plumbing: here the window DOES follow.
#[test]
fn a_window_is_followed_onto_an_inactive_user_space() {
    let (mut reactor, wid, wsid, active_space, other_space, _frame) =
        reactor_with_window_on_space1();
    reactor.space_kinds = classifier_with_a_non_user_space();
    assert_ne!(other_space, SpaceId::new(NON_USER_SPACE));

    reactor.state.windows.set_window_server_space(wsid, Some(other_space));
    reactor.reconcile_authoritative_active_window_snapshot(vec![], true);

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(other_space),
        "an ordinary inactive space is somewhere the window can go"
    );
    assert_ne!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(active_space)
    );
}
