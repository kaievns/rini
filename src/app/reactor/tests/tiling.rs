//! Layout passes: strips, columns, folding, resizes, drags and the animation that carries them.
//!
//! Named `tiling` rather than `layout` because a module called `layout` inside `tests` shadows the
//! `layout` alias the siblings use for `crate::workspaces`.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::{WindowServerId, pid_t};
use rini_geometry::SameAs;
use test_log::test;

use super::fixtures::*;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::windows::domain::info::WindowInfo;
use crate::windows::domain::request::Request;
use crate::workspaces::{Direction, LayoutCommand, LayoutEvent};

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
fn appeared_reassigns_window_without_pending_rini_move() {
    let (mut reactor, wid, wsid, space1, space2, _frame) = reactor_with_window_on_space1();

    // No pending transaction: this is a genuine external space change, so Rini should
    // follow it and reassign the window to the reported space.
    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
        Some(space1)
    );

    window_server_appeared(&mut reactor, wsid, space2, SpaceEventKind::User);

    assert_eq!(
        reactor.affinity().assigned_space_for_window_id(wid),
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
    assert!(!outcome.arrange.requested);

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
    assert!(crate::app::reactor::animation::AnimationManager::animate_layout(
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
        crate::windows::platform::window_server::set_window_ordered_in_override(wsid, Some(false));
        reactor.handle_event(Event::WindowDestroyed(middle));
        crate::windows::platform::window_server::set_window_ordered_in_override(wsid, None);

        assert!(reactor.state.windows.record(middle).is_none());
        assert_eq!(
            test_layout(&mut reactor, space, screen).len(),
            2,
            "ordered-out AX destruction must remove its slot completely",
        );

        reactor.track_test_window_server_info(wsid, pid, middle_info.frame);
        reactor.mark_test_window_visible_in_space(wsid, space);
        reactor.discover_test_windows(
            pid,
            vec![(middle, middle_info.clone())],
            vec![WindowId::new(pid, 1), middle, WindowId::new(pid, 3)],
        );
        assert_eq!(
            test_layout(&mut reactor, space, screen).len(),
            3,
            "rediscovery must restore exactly one slot",
        );
    }
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

mod strip_regroup {
    use test_log::test;

    use super::*;
    use crate::animation::domain::motion::z_group::StackGroup::{Floating, Tiled};
    use crate::app::reactor::{StackedWindow, strip_group_to_lift_for};
    use crate::windows::domain::raise::{self as raise_manager, RaiseRequest};

    const SCREEN: CGRect = CGRect {
        origin: CGPoint { x: 0., y: 0. },
        size: CGSize { width: 1728., height: 1117. },
    };

    /// Two Ghostty-like strip windows side by side, a third parked off the right edge, and a floating
    /// Settings window over the middle of the screen, all one app on one workspace. The raise manager's
    /// channel is handed back so the test reads the `RaiseRequest` the reactor sends.
    fn reactor_with_sandwich() -> (Reactor, channels::Receiver<raise_manager::Event>, SpaceId) {
        let mut reactor = test_reactor();
        let (raise_tx, mut raise_rx) = channels::channel();
        reactor.communication_manager.raise_manager_tx = raise_tx;
        let space = SpaceId::new(1);
        set_space_membership(&[(space, &[901, 902, 903, 904])]);
        reactor.handle_event(space_state_event(vec![SCREEN], vec![Some(space)]));
        reactor.add_test_app(1);
        let workspace = reactor.test_workspace(space, 0);
        let left = CGRect::new(CGPoint::new(4., 32.), CGSize::new(859., 1081.));
        let right = CGRect::new(CGPoint::new(867., 32.), CGSize::new(859., 1081.));
        let parked = CGRect::new(CGPoint::new(1727., 32.), CGSize::new(859., 1081.));
        let settings = CGRect::new(CGPoint::new(500., 200.), CGSize::new(723., 781.));
        for (idx, wsid, frame) in [
            (1, 901u32, left),
            (2, 902, right),
            (3, 903, parked),
            (4, 904, settings),
        ] {
            let window = WindowId::new(1, idx);
            reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), frame);
            assert!(reactor.assign_test_window_to_workspace(space, window, workspace));
            reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
        }
        // The strip's selection is the left column, so the layout shows the first two columns and
        // parks the third; then the fourth window floats over them.
        reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(1, 1)));
        reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(1, 4)));
        reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
        assert!(reactor.layout_manager.layout_engine.is_window_floating(WindowId::new(1, 4)));
        // The frames the strip has on screen right now: two columns visible, the third parked off
        // the right edge. Set directly; the engine's scroll position is not what these tests are about.
        for (idx, frame) in [(1, left), (2, right), (3, parked), (4, settings)] {
            if let Some(w) = reactor.state.windows.window_mut(WindowId::new(1, idx)) {
                w.frame_monotonic = frame;
            }
        }
        for idx in [1, 2] {
            assert!(
                !reactor.affinity().is_window_parked_offscreen(WindowId::new(1, idx)),
                "setup: {idx} on screen"
            );
        }
        assert!(
            reactor.affinity().is_window_parked_offscreen(WindowId::new(1, 3)),
            "setup: 3 parked"
        );
        while raise_rx.try_recv().is_ok() {}
        (reactor, raise_rx, space)
    }

    fn raise_request(rx: &mut channels::Receiver<raise_manager::Event>) -> Option<RaiseRequest> {
        match rx.try_recv().ok()?.1 {
            raise_manager::Event::RaiseRequest(request) => Some(request),
            other => panic!("unexpected raise manager event: {other:?}"),
        }
    }

    fn stacked(idx: u32, depth: usize, floating: bool) -> StackedWindow {
        StackedWindow {
            window: WindowId::new(1, idx),
            depth,
            group: if floating { Floating } else { Tiled },
        }
    }

    /// The measured sandwich: the floating window between the two visible columns. Raise order: the
    /// strip back to front, the focused window last whatever its depth.
    #[test]
    fn a_sandwich_lifts_the_strip_back_to_front_with_the_focused_window_last() {
        let order = [
            stacked(2, 0, false),
            stacked(4, 1, true),
            stacked(1, 2, false),
        ];
        assert_eq!(
            strip_group_to_lift_for(&order, WindowId::new(1, 2), Tiled),
            vec![WindowId::new(1, 1), WindowId::new(1, 2)]
        );
        // Focus on the column that was behind (a keyboard move, not raised yet): it still goes last.
        assert_eq!(
            strip_group_to_lift_for(&order, WindowId::new(1, 1), Tiled),
            vec![WindowId::new(1, 2), WindowId::new(1, 1)]
        );
        // A focus not in the order (no server depth yet) is left to the caller's own raise.
        assert_eq!(
            strip_group_to_lift_for(&order, WindowId::new(1, 9), Tiled),
            vec![WindowId::new(1, 1), WindowId::new(1, 2)]
        );
    }

    #[test]
    fn a_grouped_order_or_a_floating_focus_lifts_nothing() {
        let grouped = [
            stacked(2, 0, false),
            stacked(1, 1, false),
            stacked(4, 2, true),
        ];
        assert!(strip_group_to_lift_for(&grouped, WindowId::new(1, 2), Tiled).is_empty());
        let sandwich = [
            stacked(2, 0, false),
            stacked(4, 1, true),
            stacked(1, 2, false),
        ];
        assert!(strip_group_to_lift_for(&sandwich, WindowId::new(1, 4), Floating).is_empty());
    }

    /// A layout response focusing a strip window (the rini-initiated raise path) carries every
    /// on-screen strip window, back to front, the focused one last, when a floating window is in
    /// front of any of them. The parked column is not raised: it cannot be seen.
    #[test]
    fn a_focus_response_onto_the_strip_raises_the_on_screen_strip_over_the_floating_window() {
        let (mut reactor, mut raise_rx, _space) = reactor_with_sandwich();
        // Front to back: right column, Settings, left column, parked column.
        crate::windows::platform::window_server::set_front_to_back_override(Some(vec![
            902, 904, 901, 903,
        ]));

        reactor.handle_layout_response(
            layout::EventResponse {
                changed: true,
                raise_windows: vec![WindowId::new(1, 1)],
                focus_window: Some(WindowId::new(1, 1)),
                boundary_hit: None,
                edge_hit: None,
            },
            None,
        );
        crate::windows::platform::window_server::set_front_to_back_override(None);

        let request = raise_request(&mut raise_rx).expect("a raise request");
        assert_eq!(
            request.raise_windows,
            vec![vec![WindowId::new(1, 2), WindowId::new(1, 1)]],
            "back to front, focused last; never the floating window, never the parked column"
        );
        assert_eq!(request.focus_window.map(|(w, _)| w), Some(WindowId::new(1, 1)));
    }

    /// Focus landing on the floating window raises only what the layout asked for.
    #[test]
    fn a_focus_response_onto_the_floating_window_lifts_nothing_extra() {
        let (mut reactor, mut raise_rx, _space) = reactor_with_sandwich();
        crate::windows::platform::window_server::set_front_to_back_override(Some(vec![
            902, 904, 901, 903,
        ]));
        reactor.handle_layout_response(
            layout::EventResponse {
                changed: true,
                raise_windows: vec![WindowId::new(1, 4)],
                focus_window: Some(WindowId::new(1, 4)),
                boundary_hit: None,
                edge_hit: None,
            },
            None,
        );
        crate::windows::platform::window_server::set_front_to_back_override(None);
        let request = raise_request(&mut raise_rx).expect("a raise request");
        assert_eq!(request.raise_windows, vec![vec![WindowId::new(1, 4)]]);
    }

    /// An order that already obeys the rule costs nothing: the raise is what the layout asked for.
    #[test]
    fn a_grouped_order_is_left_alone_by_the_focus_response() {
        let (mut reactor, mut raise_rx, _space) = reactor_with_sandwich();
        crate::windows::platform::window_server::set_front_to_back_override(Some(vec![
            902, 901, 903, 904,
        ]));
        reactor.handle_layout_response(
            layout::EventResponse {
                changed: true,
                raise_windows: vec![WindowId::new(1, 1)],
                focus_window: Some(WindowId::new(1, 1)),
                boundary_hit: None,
                edge_hit: None,
            },
            None,
        );
        crate::windows::platform::window_server::set_front_to_back_override(None);
        let request = raise_request(&mut raise_rx).expect("a raise request");
        assert_eq!(request.raise_windows, vec![vec![WindowId::new(1, 1)]]);
    }

    /// Keyboard navigation between two windows of the same app: the layout's own raise names only
    /// the target column, so the regroup has to add the rest.
    #[test]
    fn same_app_keyboard_navigation_regroups_the_strip() {
        let (mut reactor, mut raise_rx, space) = reactor_with_sandwich();
        reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(1, 1)));
        while raise_rx.try_recv().is_ok() {}
        // Left column in front, Settings, then the right column and the parked one.
        crate::windows::platform::window_server::set_front_to_back_override(Some(vec![
            901, 904, 902, 903,
        ]));

        reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
        crate::windows::platform::window_server::set_front_to_back_override(None);

        assert_eq!(
            reactor.layout_manager.layout_engine.focused_window(),
            Some(WindowId::new(1, 2))
        );
        let request = raise_request(&mut raise_rx).expect("a raise request");
        assert_eq!(
            request.raise_windows,
            vec![vec![WindowId::new(1, 1), WindowId::new(1, 2)]],
            "the strip goes up as one group, the new focus last"
        );
        assert_eq!(request.focus_window.map(|(w, _)| w), Some(WindowId::new(1, 2)));
        assert!(
            raise_request(&mut raise_rx).is_none(),
            "no second regroup: nothing new came on screen"
        );
    }

    /// Keyboard navigation onto the parked column: the strip scrolls it in. It was behind the floating
    /// window, so once the layout pass has placed it a second, quiet raise lifts it and re-raises the
    /// focus over it. Before this, the floating window sat between the two columns until the next
    /// focus change.
    #[test]
    fn a_column_scrolled_in_behind_the_floating_window_is_lifted_after_the_layout() {
        let (mut reactor, mut raise_rx, space) = reactor_with_sandwich();
        reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(1, 2)));
        // The focus event re-arranged from the engine's own scroll; put the frames back to the strip
        // this test is about: two columns visible, the third parked off the right edge.
        let left = CGRect::new(CGPoint::new(4., 32.), CGSize::new(859., 1081.));
        let right = CGRect::new(CGPoint::new(867., 32.), CGSize::new(859., 1081.));
        let parked = CGRect::new(CGPoint::new(1727., 32.), CGSize::new(859., 1081.));
        for (idx, frame) in [(1, left), (2, right), (3, parked)] {
            if let Some(w) = reactor.state.windows.window_mut(WindowId::new(1, idx)) {
                w.frame_monotonic = frame;
            }
        }
        assert!(reactor.affinity().is_window_parked_offscreen(WindowId::new(1, 3)));
        while raise_rx.try_recv().is_ok() {}
        // The visible pair is grouped in front of Settings; the parked column is behind it.
        crate::windows::platform::window_server::set_front_to_back_override(Some(vec![
            902, 901, 904, 903,
        ]));

        reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
        crate::windows::platform::window_server::set_front_to_back_override(None);

        assert_eq!(
            reactor.layout_manager.layout_engine.focused_window(),
            Some(WindowId::new(1, 3))
        );
        assert!(
            !reactor.affinity().is_window_parked_offscreen(WindowId::new(1, 3)),
            "the layout pass brought the target on screen: {:?}",
            reactor.state.windows.window(WindowId::new(1, 3)).map(|w| w.frame_monotonic)
        );
        let first = raise_request(&mut raise_rx).expect("the focus move's own raise");
        assert_eq!(
            first.raise_windows,
            vec![vec![WindowId::new(1, 3)]],
            "judged before the scroll"
        );

        // What the strip shows after the pass, in the server's (old) order 2, 1, 3 front to back:
        // the on-screen columns back to front, the scrolled-in focus last.
        let mut expected: Vec<WindowId> = [2u32, 1]
            .into_iter()
            .map(|idx| WindowId::new(1, idx))
            .filter(|wid| !reactor.affinity().is_window_parked_offscreen(*wid))
            .rev()
            .collect();
        expected.push(WindowId::new(1, 3));
        let second = raise_request(&mut raise_rx).expect("the post-layout regroup");
        assert_eq!(second.raise_windows, vec![expected]);
        assert_eq!(second.focus_window.map(|(w, _)| w), Some(WindowId::new(1, 3)));
        assert_eq!(second.focus_quiet, Quiet::Yes);
        assert!(raise_request(&mut raise_rx).is_none(), "one regroup, not a loop");
    }

    /// A window-server focus report onto the strip (a click, raised by macOS, not by rini) sends the
    /// regroup as a raise of its own.
    #[test]
    fn a_click_onto_the_strip_regroups_the_strip() {
        let (mut reactor, mut raise_rx, space) = reactor_with_sandwich();
        reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(1, 4)));
        while raise_rx.try_recv().is_ok() {}
        // macOS raised the clicked left column over Settings; the right column stayed behind.
        crate::windows::platform::window_server::set_front_to_back_override(Some(vec![
            901, 904, 902, 903,
        ]));

        reactor.handle_event(Event::WindowServerFocusChanged(WindowId::new(1, 1), space));
        crate::windows::platform::window_server::set_front_to_back_override(None);

        let request = raise_request(&mut raise_rx).expect("a raise request");
        assert_eq!(
            request.raise_windows,
            vec![vec![WindowId::new(1, 2), WindowId::new(1, 1)]]
        );
        assert_eq!(request.focus_window.map(|(w, _)| w), Some(WindowId::new(1, 1)));
        assert_eq!(
            request.focus_quiet,
            Quiet::Yes,
            "rini's own raise, not the user moving"
        );
    }

    /// Switching to a Zoom call: macOS focuses Zoom's meeting toolbar, a window the layout does
    /// not hold (`is_standard: false`), so the layout's focus stays on the strip column that had
    /// it. The regroup must not read that stale focus as "the strip is focused" and lift the strip
    /// over the call; it did, and the call window showed for a frame before Kiro covered it.
    #[test]
    fn focus_on_a_window_outside_the_layout_does_not_regroup_the_strip() {
        let (mut reactor, mut raise_rx, space) = reactor_with_sandwich();
        reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(1, 1)));
        let zoom = 2;
        reactor.add_test_app(zoom);
        let toolbar = WindowId::new(zoom, 1);
        reactor.add_test_window_with_manageability(
            toolbar,
            WindowServerId::new(905),
            Some(space),
            CGRect::new(CGPoint::new(700., 32.), CGSize::new(301., 45.)),
            false,
        );
        reactor.handle_event(Event::ApplicationGloballyActivated(zoom));
        while raise_rx.try_recv().is_ok() {}
        // Zoom in front, then Settings (floating) over the strip: a sandwich if the strip were focused.
        crate::windows::platform::window_server::set_front_to_back_override(Some(vec![
            905, 904, 901, 902, 903,
        ]));

        reactor.handle_event(Event::WindowServerFocusChanged(toolbar, space));
        crate::windows::platform::window_server::set_front_to_back_override(None);

        assert_eq!(
            reactor.main_window(),
            Some(toolbar),
            "setup: the toolbar has focus"
        );
        assert_eq!(
            reactor.layout_manager.layout_engine.focused_window(),
            Some(WindowId::new(1, 1))
        );
        assert!(
            raise_request(&mut raise_rx).is_none(),
            "the strip must not be lifted over the call"
        );
    }
}

/// The measured artefact: a floating window re-placed by one point animated on its own, and because the
/// overlay is opaque and spans the display, every OTHER window on screen was replaced by wallpaper for
/// 350ms. Windows a pass does not move still have to be handed over, so the overlay can draw them.
#[test]
fn a_pass_that_moves_two_windows_still_hands_over_the_one_it_leaves_alone() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.config.settings.animate = true;

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(3));
    apps.requests();

    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.workspace_animation_tx = Some(animation_tx);

    // Two windows moving by opposite vectors, so this is not a strip pan and goes to the per-window
    // path. The third keeps the frame it already has.
    let held = WindowId::new(1, 3);
    let mut layout = Vec::new();
    for idx in 1..=3u32 {
        let wid = WindowId::new(1, idx);
        let frame = reactor.state.windows.window(wid).expect("window").frame_monotonic;
        let target = match idx {
            1 => CGRect::new(CGPoint::new(frame.origin.x + 120., frame.origin.y), frame.size),
            2 => CGRect::new(CGPoint::new(frame.origin.x - 120., frame.origin.y), frame.size),
            _ => frame,
        };
        layout.push((wid, target));
    }

    crate::app::reactor::animation::AnimationManager::animate_layout(
        &mut reactor,
        space,
        &layout,
        false,
        None,
    );

    let mut animated: Vec<crate::animation::domain::request::AnimationRequest> = Vec::new();
    while let Ok((_, event)) = animation_rx.try_recv() {
        match event {
            crate::animation::platform::engine::Event::Animate { windows, .. } => {
                animated = windows
            }
            // Opposite vectors are not a pan, so this must not reach the strip path: that path takes
            // its windows from the layout and so never had this bug to begin with.
            crate::animation::platform::engine::Event::AnimateSurface { .. } => {
                panic!("a layout that is not a pan must go to the per-window path")
            }
            _ => {}
        }
    }
    assert_eq!(
        animated.len(),
        3,
        "every window on the display, not just the movers: {animated:?}"
    );
    let still = animated
        .iter()
        .find(|request| request.window == held)
        .expect("the still window");
    assert_eq!(
        still.from, still.to,
        "it is handed over to be drawn, not to be moved"
    );
}

/// A floating window oscillated between x = 502 and x = 503 on every space-state refresh. Animating that
/// covers the display for 350ms and buys nothing, so a pass with no visible travel is placed instead.
#[test]
fn a_one_point_move_is_placed_rather_than_animated() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.config.settings.animate = true;

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    apps.requests();

    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.workspace_animation_tx = Some(animation_tx);

    let mut layout = Vec::new();
    for idx in 1..=2u32 {
        let wid = WindowId::new(1, idx);
        let frame = reactor.state.windows.window(wid).expect("window").frame_monotonic;
        let target = if idx == 1 {
            CGRect::new(CGPoint::new(frame.origin.x + 1., frame.origin.y), frame.size)
        } else {
            frame
        };
        layout.push((wid, target));
    }

    crate::app::reactor::animation::AnimationManager::animate_layout(
        &mut reactor,
        space,
        &layout,
        false,
        None,
    );

    while let Ok((_, event)) = animation_rx.try_recv() {
        assert!(
            !matches!(
                event,
                crate::animation::platform::engine::Event::Animate { .. }
                    | crate::animation::platform::engine::Event::AnimateSurface { .. }
            ),
            "a one-point move must not run an animation: {event:?}"
        );
    }
    assert!(
        apps.requests().iter().any(|request| matches!(
            request,
            Request::SetWindowFrame(..) | Request::SetBatchWindowFrame(..)
        )),
        "the window still has to be placed"
    );
}

/// P-3.13 (`.kiro/specs/exit-entrance-animation-regressions`): with animations off, a
/// close sends the engine nothing but the forget, and the window state is still removed.
#[test]
fn a_destroyed_window_does_not_exit_when_animations_are_off() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.config.settings.animate = false;

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    apps.requests();

    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.workspace_animation_tx = Some(animation_tx);

    let wid = WindowId::new(1, 1);
    reactor.handle_event(Event::WindowDestroyed(wid));

    while let Ok((_, event)) = animation_rx.try_recv() {
        assert!(
            !matches!(
                event,
                crate::animation::platform::engine::Event::Animate { .. }
                    | crate::animation::platform::engine::Event::AnimateSurface { .. }
            ),
            "nothing flies with animations off: {event:?}"
        );
    }
    assert!(
        reactor.state.windows.window(wid).is_none(),
        "the window state is still removed"
    );
}

/// P-3.13: with animations off, a layout pass places the windows and sends the engine no
/// flight.
#[test]
fn a_layout_pass_does_not_fly_when_animations_are_off() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.config.settings.animate = false;

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(3));
    apps.requests();

    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.workspace_animation_tx = Some(animation_tx);

    let mut layout = Vec::new();
    for idx in 1..=3u32 {
        let wid = WindowId::new(1, idx);
        let frame = reactor.state.windows.window(wid).expect("window").frame_monotonic;
        let target = match idx {
            1 => CGRect::new(CGPoint::new(frame.origin.x + 120., frame.origin.y), frame.size),
            2 => CGRect::new(CGPoint::new(frame.origin.x - 120., frame.origin.y), frame.size),
            _ => frame,
        };
        layout.push((wid, target));
    }

    crate::app::reactor::animation::AnimationManager::animate_layout(
        &mut reactor,
        space,
        &layout,
        false,
        None,
    );

    while let Ok((_, event)) = animation_rx.try_recv() {
        assert!(
            !matches!(
                event,
                crate::animation::platform::engine::Event::Animate { .. }
                    | crate::animation::platform::engine::Event::AnimateSurface { .. }
            ),
            "nothing flies with animations off: {event:?}"
        );
    }
    assert!(
        apps.requests().iter().any(|request| matches!(
            request,
            Request::SetWindowFrame(..) | Request::SetBatchWindowFrame(..)
        )),
        "the windows are still placed"
    );
}
