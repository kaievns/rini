//! Layout passes: strips, columns, folding, resizes, drags and the animation that carries them.
//!
//! Named `tiling` rather than `layout` because a module called `layout` inside `tests` shadows the
//! `layout` alias the siblings use for `crate::workspaces`.
use std::time::Instant;

use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::{WindowServerId, pid_t};
use rini_geometry::SameAs;
use test_log::test;

use super::fixtures::*;
use crate::animation::domain::motion::surface::SurfaceGeometry;
use crate::app::config::Edges;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::input::event::LeftPress;
use crate::layout::domain::area::compute_tiling_area;
use crate::layout::domain::edge_drag::End;
use crate::layout::domain::tile::window_in;
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
/// Reported: with Zoom and System Settings floating, Mod+J/L cycled those two rather than
/// the columns, and leaving them landed on the FIRST column instead of the one that had been
/// selected. Floating windows belong to a workspace but are not strip members; cmd-tab and
/// `switch-focus-between-floating-and-tiling` reach them.
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
    use crate::windows::domain::raise_order::StackGroup::{Floating, Tiled};
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
                edge_hit: None,
                moved: None,
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

    /// A window that has just opened is frontmost, and the strip must be left behind it.
    ///
    /// The symptom this is for: opening a Settings window made it appear and then instantly drop behind
    /// the columns, so it had to be cmd-tabbed back to. macOS raises a new window, so the order is
    /// `[Settings, right, left, parked]` — and the regroup called that broken, because any strip window
    /// behind the frontmost floating one counted. It raised the whole strip back over it.
    ///
    /// Nothing is sandwiched here. A floating window in front of the whole strip is the point of
    /// floating, so there is nothing to put right.
    #[test]
    fn a_newly_opened_floating_window_is_left_in_front_of_the_strip() {
        let (mut reactor, mut raise_rx, _space) = reactor_with_sandwich();
        // Front to back: the new Settings window, then the strip.
        crate::windows::platform::window_server::set_front_to_back_override(Some(vec![
            904, 902, 901, 903,
        ]));

        // The layout still believes a strip window has focus: rini has not yet been told the new
        // window took it, which is the race the old rule turned into a visible bug.
        reactor.send_layout_event(LayoutEvent::WindowFocused(SpaceId::new(1), WindowId::new(1, 1)));
        reactor.regroup_after_layout();
        crate::windows::platform::window_server::set_front_to_back_override(None);

        assert!(
            raise_request(&mut raise_rx).is_none(),
            "the strip must not be raised over a window that just opened"
        );
    }

    /// And the case the regroup exists for still works: a floating window BETWEEN two columns.
    #[test]
    fn a_floating_window_between_two_columns_still_lifts_the_strip() {
        let (mut reactor, mut raise_rx, _space) = reactor_with_sandwich();
        // Front to back: right column, Settings, left column, parked column.
        crate::windows::platform::window_server::set_front_to_back_override(Some(vec![
            902, 904, 901, 903,
        ]));

        reactor.send_layout_event(LayoutEvent::WindowFocused(SpaceId::new(1), WindowId::new(1, 1)));
        reactor.regroup_after_layout();
        crate::windows::platform::window_server::set_front_to_back_override(None);

        let request = raise_request(&mut raise_rx).expect("the sandwich is still put right");
        assert!(
            !request.raise_windows.iter().flatten().any(|w| *w == WindowId::new(1, 4)),
            "and the floating window is never raised with the strip"
        );
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
                edge_hit: None,
                moved: None,
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
                edge_hit: None,
                moved: None,
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
    reactor.communication_manager.transitions_tx = Some(animation_tx);

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

    let mut animated: Vec<crate::transitions::domain::request::AnimationRequest> = Vec::new();
    while let Ok((_, event)) = animation_rx.try_recv() {
        match event {
            crate::transitions::platform::engine::Event::Animate { windows, .. } => {
                animated = windows
            }
            // Opposite vectors are not a pan, so this must not reach the strip path: that path takes
            // its windows from the layout and so never had this bug to begin with.
            crate::transitions::platform::engine::Event::AnimateSurface { .. } => {
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

/// A layout pass hands each tiled window over with where the strip had it before the pass and has
/// it after, which is where a window the pass parks or unparks is drawn going or coming; a floating
/// window, not on the strip, with neither. A maximize of the first of three columns parks the second
/// and moves the third's place on the strip while its park stays put.
#[test]
fn a_pass_s_requests_carry_each_tiled_window_s_place_on_the_strip() {
    use crate::transitions::platform::engine::Event as Engine;
    use rini_geometry::Round;
    use rini_geometry::space::FromCg;

    let (mut apps, mut reactor, mut rx) = moving_strip(0.5, 4);
    let space = SpaceId::new(1);
    let (first, second, third, floating) =
        (WindowId::new(1, 1), WindowId::new(1, 2), WindowId::new(1, 3), WindowId::new(1, 4));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, floating));
    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, first));
    apps.simulate_until_quiet(&mut reactor);
    assert!(reactor.layout_manager.layout_engine.is_window_floating(floating));
    while rx.try_recv().is_ok() {}
    apps.requests();
    let before = reactor.test_strip_layout(space);

    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreenWithinGaps);
    let after = reactor.test_strip_layout(space);
    let mut passes = Vec::new();
    while let Ok((_, event)) = rx.try_recv() {
        if let Engine::Animate { windows, .. } = event {
            passes.push(windows);
        }
    }
    let [requests] = &passes[..] else { panic!("one pass: {passes:?}") };
    let place = |layout: &[(WindowId, CGRect)], window| {
        layout.iter().find(|(w, _)| *w == window).map(|(_, f)| ScreenRect::from_cg(*f).rounded())
    };
    let places: Vec<_> = requests.iter().map(|r| (r.window, r.strip_before, r.strip_after)).collect();
    let expected: Vec<_> = requests
        .iter()
        .map(|r| match r.window == floating {
            true => (r.window, None, None),
            false => (r.window, place(&before, r.window), place(&after, r.window)),
        })
        .collect();
    assert_eq!(places, expected);
    assert_eq!(requests.len(), 4, "{requests:?}");

    let request = |window| requests.iter().find(|r| r.window == window).expect("in the pass");
    let parked = request(second);
    assert_ne!(parked.strip_before, parked.strip_after, "{parked:?}");
    assert_eq!(parked.strip_before, Some(parked.from), "seen where the strip had it");
    let kept = request(third);
    assert_eq!(kept.from, kept.to, "its park stays put");
    assert!(kept.strip_before.is_some() && kept.strip_before != kept.strip_after, "{kept:?}");
    assert_eq!(request(floating).from, request(floating).to);
    assert!(place(&after, floating).is_some(), "the strip's layout holds the floating window's frame");
}

/// Focus moved from the first window to the second since the last batch: the second wears the ring
/// and holds focus where the pass ends and the first neither, each request saying so with the
/// decoration every managed window wears.
#[test]
fn a_request_carries_the_ring_and_focus_its_window_ends_with() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.config.settings.animate = true;

    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(3));
    apps.requests();
    let (first, second, third) = (WindowId::new(1, 1), WindowId::new(1, 2), WindowId::new(1, 3));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, first));
    reactor.handle_test_batch(Vec::new());
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, second));

    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.transitions_tx = Some(animation_tx);
    let layout: Vec<(WindowId, CGRect)> = [first, second, third]
        .into_iter()
        .map(|wid| {
            let frame = reactor.state.windows.window(wid).expect("window").frame_monotonic;
            let shift = if wid == first { 120. } else { -120. };
            (wid, CGRect::new(CGPoint::new(frame.origin.x + shift, frame.origin.y), frame.size))
        })
        .collect();
    crate::app::reactor::animation::AnimationManager::animate_layout(&mut reactor, space, &layout, false, None);

    let mut animated = Vec::new();
    while let Ok((_, event)) = animation_rx.try_recv() {
        if let crate::transitions::platform::engine::Event::Animate { windows, .. } = event {
            animated = windows;
        }
    }
    let worn = |wid| animated.iter().find(|request| request.window == wid).expect("requested").worn;
    let decoration = crate::decorations::domain::style::Decoration {
        ring: Some(reactor.config.settings.decorations.focus_ring.band),
        ..Default::default()
    };
    use crate::decorations::domain::style::Ring;
    for (wid, wears, holds) in [(first, Ring::None, false), (second, Ring::Active, true), (third, Ring::None, false)] {
        let worn = worn(wid);
        assert_eq!((worn.wears_ring, worn.holds_focus), (wears, holds), "{wid:?}");
        assert_eq!(worn.decoration, decoration);
    }
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
    reactor.communication_manager.transitions_tx = Some(animation_tx);

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
                crate::transitions::platform::engine::Event::Animate { .. }
                    | crate::transitions::platform::engine::Event::AnimateSurface { .. }
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

/// With animations off, a close sends the engine no flight, and the window state is still removed:
/// animation is never load-bearing (`specs/animation.md`).
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
    reactor.communication_manager.transitions_tx = Some(animation_tx);

    let wid = WindowId::new(1, 1);
    reactor.handle_event(Event::WindowDestroyed(wid));

    while let Ok((_, event)) = animation_rx.try_recv() {
        assert!(
            !matches!(
                event,
                crate::transitions::platform::engine::Event::Animate { .. }
                    | crate::transitions::platform::engine::Event::AnimateSurface { .. }
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
    reactor.communication_manager.transitions_tx = Some(animation_tx);

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
                crate::transitions::platform::engine::Event::Animate { .. }
                    | crate::transitions::platform::engine::Event::AnimateSurface { .. }
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

fn frames_written(requests: &[Request], wid: WindowId) -> Vec<CGRect> {
    let mut out = Vec::new();
    for request in requests {
        match request {
            Request::SetWindowFrame(w, frame, _, _) if *w == wid => out.push(*frame),
            Request::SetBatchWindowFrame(frames, _, _) => {
                out.extend(frames.iter().filter(|(w, _)| *w == wid).map(|(_, f)| *f))
            }
            _ => {}
        }
    }
    out
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
}

/// The whole display, and what the menu bar leaves of it: where AppKit zooms a window to.
const DISPLAY: CGRect = CGRect {
    origin: CGPoint { x: 0., y: 0. },
    size: CGSize { width: 1440., height: 900. },
};
const VISIBLE: CGRect = CGRect {
    origin: CGPoint { x: 0., y: 25. },
    size: CGSize { width: 1440., height: 875. },
};

/// Two windows of one app in two columns, on one display, with the app answering rini's writes.
struct Strip {
    apps: Apps,
    reactor: Reactor,
    window: WindowId,
    other: WindowId,
    space: SpaceId,
}

impl Strip {
    /// Two columns and gaps all round, so rini's full width is smaller than the screen an app
    /// zooms to.
    fn new() -> Self {
        Self::with_columns(2)
    }

    /// `count` columns of one window each, `window` the first and `other` the second.
    fn with_columns(count: usize) -> Self {
        let (mut apps, mut reactor) = test_context();
        reactor.config.settings.layout.gaps.struts = Edges {
            top: 8.,
            left: 8.,
            bottom: 8.,
            right: 8.,
        };
        reactor
            .layout_manager
            .layout_engine
            .set_layout_settings(&reactor.config.settings.layout);
        let space = SpaceId::new(1);
        let members: Vec<u32> = (1..=count as u32).collect();
        set_space_membership(&[(space, members.as_slice())]);
        reactor.handle_event(space_state_event_from_screens(vec![ScreenInfo {
            id: rini_core::ids::ScreenId::new(0),
            frame: VISIBLE,
            bounds: DISPLAY,
            display_uuid: "test-display-0".to_owned(),
            name: None,
            space: Some(space),
            is_builtin: false,
            scale: 2.0,
        }]));
        make_active_app_with_count(&mut apps, &mut reactor, 1, count, Some(WindowId::new(1, 1)));
        let window = WindowId::new(1, 1);
        reactor.send_layout_event(LayoutEvent::WindowFocused(space, window));
        apps.simulate_until_quiet(&mut reactor);
        Strip {
            apps,
            reactor,
            window,
            other: WindowId::new(1, 2),
            space,
        }
    }

    fn laid_out(&mut self, window: WindowId) -> CGRect {
        laid_out_frame(&mut self.reactor, self.space, VISIBLE, window).expect("the window is laid out")
    }

    fn tiling(&self) -> CGRect {
        compute_tiling_area(VISIBLE, &self.reactor.config.settings.layout.gaps.effective_for_display(None))
    }

    fn frame_of(&self, window: WindowId) -> CGRect {
        self.reactor.state.windows.window(window).expect("tracked").frame_monotonic
    }

    fn press(&mut self, location: CGPoint, double_click: bool) {
        self.reactor.handle_event(Event::LeftMouseDown(LeftPress {
            location,
            at: Instant::now(),
            double_click,
        }));
    }

    /// The app reporting a frame it chose itself.
    fn app_reports(&mut self, window: WindowId, frame: CGRect, mouse: MouseState) {
        let app_window = self.apps.windows.get_mut(&window).expect("test window");
        app_window.frame = frame;
        let txid = app_window.last_seen_txid;
        self.reactor.handle_event(Event::WindowFrameChanged(
            window,
            frame,
            Some(txid),
            Requested(false),
            Some(mouse),
        ));
    }

    /// Carries rini's requests out as the app would, and returns what they wrote `window`.
    fn settle(&mut self, window: WindowId) -> Vec<CGRect> {
        let mut written = Vec::new();
        loop {
            let requests = self.apps.requests();
            if requests.is_empty() {
                return written;
            }
            written.extend(frames_written(&requests, window));
            for event in self.apps.simulate_events_for_requests(requests) {
                self.reactor.handle_event(event);
            }
        }
    }

    fn on_screen(&self, window: WindowId) -> CGRect {
        self.apps.windows[&window].frame
    }

    /// The config reloaded with niri's border on at `width`, and what that writes carried out.
    fn with_border(mut self, width: f64) -> Self {
        reload_with_border(&mut self.reactor, width);
        self.apps.simulate_until_quiet(&mut self.reactor);
        self
    }
}

fn reload_with_border(reactor: &mut Reactor, width: f64) {
    let mut config = reactor.config.clone();
    config.settings.decorations.border.on = true;
    config.settings.decorations.border.band.width = width;
    reactor.handle_event(Event::ConfigUpdated(config));
}

/// A border takes its width out of each tiled window's slot, and a reload changing it lays the
/// strip out again at once.
#[test]
fn a_reload_changing_the_border_width_lays_the_strip_out_again() {
    let mut strip = Strip::new();
    let before = strip.laid_out(strip.window);
    assert_eq!(strip.on_screen(strip.window), before);
    reload_with_border(&mut strip.reactor, 4.);
    let inset = window_in(before, 4.);
    assert_eq!(strip.laid_out(strip.window), inset);
    assert_eq!(strip.settle(strip.window).last(), Some(&inset), "written by the reload");
    assert_eq!(strip.on_screen(strip.window), inset);
}

/// The slot gives up the border as the band is drawn, in whole device pixels: 1.3pt at 2x is 1.5.
#[test]
fn a_border_between_device_pixels_takes_out_of_the_slot_what_the_band_draws() {
    let mut strip = Strip::new();
    let before = strip.laid_out(strip.window);
    reload_with_border(&mut strip.reactor, 1.3);
    assert_eq!(strip.laid_out(strip.window), window_in(before, 1.5));
}

/// rini's own write coming back is no resize, with the border on as without; an app's own size is
/// the window's, its slot the border wider.
#[test]
fn a_window_at_its_written_frame_is_not_taken_for_a_user_resize_with_a_border_on() {
    let mut strip = Strip::new().with_border(4.);
    let written = strip.laid_out(strip.window);
    strip.app_reports(strip.window, written, MouseState::Up);
    assert_eq!(strip.laid_out(strip.window), written);
    assert_eq!(strip.settle(strip.window), Vec::<CGRect>::new(), "nothing to write back");

    let narrower = CGRect::new(written.origin, CGSize::new(written.size.width - 100., written.size.height));
    strip.app_reports(strip.window, narrower, MouseState::Up);
    assert_eq!(strip.laid_out(strip.window).size, narrower.size, "the window at the app's width, not 8pt less");
}

#[test]
fn a_floating_window_keeps_its_frame_under_a_border() {
    let (mut apps, mut reactor) = test_context();
    let space = SpaceId::new(1);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    make_active_app_with_count(&mut apps, &mut reactor, 1, 2, Some(WindowId::new(1, 1)));
    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    apps.simulate_until_quiet(&mut reactor);
    let floating = WindowId::new(1, 1);
    assert!(reactor.layout_manager.layout_engine.is_window_floating(floating));
    let before = test_layout(&mut reactor, space, screen);

    reload_with_border(&mut reactor, 4.);
    apps.simulate_until_quiet(&mut reactor);
    let after = test_layout(&mut reactor, space, screen);
    let frame = |frames: &[(WindowId, CGRect)], window| frames.iter().find(|(w, _)| *w == window).map(|(_, f)| *f);
    assert_eq!(frame(&after, floating), frame(&before, floating), "it wears the band outside");
    let tiled = WindowId::new(1, 2);
    assert_eq!(frame(&after, tiled), frame(&before, tiled).map(|slot| window_in(slot, 4.)));
}

#[test]
fn the_drag_session_keeps_the_frame_its_window_started_from() {
    let mut strip = Strip::new();
    let start = strip.frame_of(strip.window);
    let mut last = start;
    for grown in [30., 60., 90.] {
        last = rect(start.origin.x, start.origin.y, start.size.width + grown, start.size.height);
        strip.app_reports(strip.window, last, MouseState::Down);
    }
    let session = active_session(&strip);
    assert_eq!(session.start_frame, start, "the frame before the first report");
    assert_eq!(session.last_frame, last, "the latest report");
}

fn active_session(strip: &Strip) -> &DragSession {
    let DragState::Active { session } = &strip.reactor.drag_manager.drag_state else {
        panic!("a drag under the held button: {:?}", strip.reactor.drag_manager.drag_state);
    };
    session
}

/// The end a drag moved last is the drag's, so it ends with the release and the next drag of the
/// same window, from the same frame, starts without it.
#[test]
fn the_drag_session_keeps_the_end_its_reports_moved_last_until_the_release() {
    let mut strip = Strip::new();
    let start = strip.frame_of(strip.window);
    let shorter = rect(start.origin.x, start.origin.y, start.size.width, start.size.height - 40.);
    strip.app_reports(strip.window, shorter, MouseState::Down);
    assert_eq!(active_session(&strip).dragged_end, Some(End::Bottom), "moved");
    strip.app_reports(strip.window, start, MouseState::Down);
    assert_eq!(active_session(&strip).dragged_end, Some(End::Bottom), "back at the start");

    strip.reactor.handle_event(Event::MouseUp);
    strip.settle(strip.window);
    let start = strip.frame_of(strip.window);
    let wider = rect(start.origin.x, start.origin.y, start.size.width + 40., start.size.height);
    strip.app_reports(strip.window, wider, MouseState::Down);
    assert_eq!(active_session(&strip).dragged_end, None, "the next drag");
}

/// Every report of a drag measures from the frame the drag started at, not the report before it.
/// Under `"always"` the middle column takes twice the edge's travel: 100pt over two reports is
/// 200pt, where measuring each report from the last would end at 150.
#[test]
fn every_report_of_a_drag_measures_from_where_the_drag_started() {
    let mut strip = Strip::with_columns(3);
    strip.reactor.config.settings.layout.scrolling.center_focused_column =
        crate::layout::settings::CenterFocusedColumn::Always;
    strip
        .reactor
        .layout_manager
        .layout_engine
        .set_layout_settings(&strip.reactor.config.settings.layout);
    strip.reactor.send_layout_event(LayoutEvent::WindowFocused(strip.space, strip.other));
    strip.settle(strip.other);
    let start = strip.on_screen(strip.other);

    for travel in [50., 100.] {
        let wider = rect(start.origin.x, start.origin.y, start.size.width + travel, start.size.height);
        strip.app_reports(strip.other, wider, MouseState::Down);
    }
    strip.reactor.handle_event(Event::MouseUp);
    strip.settle(strip.other);

    let width = strip.on_screen(strip.other).size.width;
    assert!((width - (start.size.width + 200.)).abs() <= 3., "{start:?} -> {width}");
}

/// The window the window server finds under the cursor, for as long as the guard lives.
struct CursorOver;

impl CursorOver {
    fn window(reactor: &Reactor, window: WindowId) -> Self {
        let id = reactor.state.windows.window(window).and_then(|w| w.info.sys_id).expect("sys id");
        crate::windows::platform::window_server::set_window_under_cursor_override(Some(id.as_u32()));
        CursorOver
    }
}

impl Drop for CursorOver {
    fn drop(&mut self) {
        crate::windows::platform::window_server::set_window_under_cursor_override(None);
    }
}

/// `other`, column 1, dragged left by its left edge over 3 reports and let go with `under_cursor`
/// under the cursor, which the release focuses. Its right edge must end where it started.
fn left_edge_drag_released_over(under_cursor: Option<fn(&Strip) -> WindowId>) -> Strip {
    let mut strip = Strip::new();
    // Narrow enough that column 1 stands clear of the screen's right edge, where the reveal alone
    // would put its right edge back.
    strip.reactor.config.settings.layout.scrolling.column_width_ratio = 0.4;
    strip
        .reactor
        .layout_manager
        .layout_engine
        .set_layout_settings(&strip.reactor.config.settings.layout);
    strip.reactor.send_layout_event(LayoutEvent::WindowFocused(strip.space, strip.other));
    strip.settle(strip.other);
    let start = strip.on_screen(strip.other);
    let window_start = strip.on_screen(strip.window);
    assert!(start.max().x < strip.tiling().max().x - 200., "{start:?}");

    strip.press(CGPoint::new(start.origin.x, start.mid().y), false);
    for grown in [30., 60., 90.] {
        strip.app_reports(
            strip.other,
            rect(start.origin.x - grown, start.origin.y, start.size.width + grown, start.size.height),
            MouseState::Down,
        );
    }
    let cursor = under_cursor.map(|window| CursorOver::window(&strip.reactor, window(&strip)));
    strip.reactor.handle_event(Event::MouseUp);
    drop(cursor);
    strip.settle(strip.other);

    let end = strip.on_screen(strip.other);
    assert!((end.max().x - start.max().x).abs() <= 2., "{start:?} -> {end:?}");
    let travel = start.origin.x - end.origin.x;
    assert!(travel > 80., "{start:?} -> {end:?}");
    let window_travel = window_start.origin.x - strip.on_screen(strip.window).origin.x;
    assert!((window_travel - travel).abs() <= 1., "{window_travel} against {travel}");
    strip
}

/// "Resizing with the mouse" in `specs/strip.md`.
#[test]
fn a_left_edge_drag_lands_at_release_with_its_right_edge_where_it_started() {
    left_edge_drag_released_over(None);
}

#[test]
fn a_left_edge_drag_let_go_over_its_own_window_keeps_its_right_edge() {
    left_edge_drag_released_over(Some(|strip| strip.other));
}

/// The cursor lets go on the dragged edge, where the window server can answer with the column
/// behind it.
#[test]
fn a_left_edge_drag_let_go_over_the_window_behind_its_edge_keeps_its_focus_and_right_edge() {
    let strip = left_edge_drag_released_over(Some(|strip| strip.window));
    assert_eq!(strip.reactor.layout_manager.layout_engine.focused_window(), Some(strip.other));
}

/// Kai, 2026-10-06, after the left edge first grew to the left: "it makes a weird back and fourth
/// wobble on release". The release scrolls the strip by the left edge's travel, and the window the
/// drag resized already stands where that scroll puts it: its tile flies from where it was let go,
/// not from its new frame shifted back by the scroll, and the column before it pans from where it
/// stands. "Resizing with the mouse" in `specs/strip.md`.
#[test]
fn a_left_edge_drags_release_draws_the_dragged_window_from_where_it_was_let_go() {
    let mut strip = Strip::new();
    strip.reactor.config.settings.layout.scrolling.column_width_ratio = 0.4;
    strip
        .reactor
        .layout_manager
        .layout_engine
        .set_layout_settings(&strip.reactor.config.settings.layout);
    strip.reactor.send_layout_event(LayoutEvent::WindowFocused(strip.space, strip.other));
    strip.settle(strip.other);
    let start = strip.on_screen(strip.other);
    let before = strip.on_screen(strip.window);
    strip.reactor.config.settings.animate = true;
    let (animation_tx, mut rx) = crate::app::channels::channel();
    strip.reactor.communication_manager.transitions_tx = Some(animation_tx);

    strip.press(CGPoint::new(start.origin.x, start.mid().y), false);
    let mut dropped = start;
    for grown in [30., 60., 90.] {
        dropped = rect(
            start.origin.x - grown,
            start.origin.y,
            start.size.width + grown,
            start.size.height,
        );
        strip.app_reports(strip.other, dropped, MouseState::Down);
    }
    strip.reactor.handle_event(Event::MouseUp);

    let flights = strip_movements(&mut rx);
    assert_eq!(flights.len(), 1, "the release scrolls the strip once");
    let flight = &flights[0];
    let offset = flight.from_offset;
    assert!((offset.x + 90.).abs() <= 2., "by the edge's travel: {offset:?}");
    let travel = |window| {
        flight
            .windows
            .iter()
            .find(|w| w.window == window)
            .expect("on the surface")
            .travel(flight.from_offset, SurfacePoint::origin())
    };
    let (from, to) = travel(strip.other);
    assert!(
        (from.origin.x - dropped.origin.x).abs() <= 2.,
        "drawn from where it was let go, {dropped:?}, not {from:?}"
    );
    assert!((to.origin.x + to.size.width - dropped.max().x).abs() <= 2., "{to:?}");
    let (from, to) = travel(strip.window);
    assert!(
        (from.origin.x - before.origin.x).abs() <= 1.,
        "pans from where it stands, {before:?}, not {from:?}"
    );
    assert!((from.origin.x - to.origin.x - 90.).abs() <= 2., "{from:?} -> {to:?}");
}

/// Kai, 2026-10-07: "wher i resize a ghosty window by the left size it still does a small 1-2px
/// wobble during the animation when i release". With gaps, a dragged column gave up its share of
/// the gap, so the window landed 1-3pt from where it was let go and its tile slid there. Kai's gaps
/// of 3, and a drag to a width two of which fit abreast, whose share is 1.5pt.
#[test]
fn a_left_edge_drags_release_leaves_the_dragged_window_exactly_where_it_was_let_go() {
    let mut strip = Strip::with_columns(3);
    let last = WindowId::new(1, 3);
    strip.reactor.config.settings.layout.gaps.gaps = 3.;
    strip.reactor.config.settings.layout.scrolling.column_width_ratio = 0.5;
    strip.reactor.config.settings.layout.scrolling.center_focused_column =
        crate::layout::settings::CenterFocusedColumn::Never;
    strip
        .reactor
        .layout_manager
        .layout_engine
        .set_layout_settings(&strip.reactor.config.settings.layout);
    for _ in 0..2 {
        strip.reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
        strip.settle(last);
    }
    let start = strip.on_screen(last);
    assert!((start.max().x - strip.tiling().max().x).abs() <= 1., "flush with the right edge: {start:?}");
    strip.reactor.config.settings.animate = true;
    let (animation_tx, mut rx) = crate::app::channels::channel();
    strip.reactor.communication_manager.transitions_tx = Some(animation_tx);

    strip.press(CGPoint::new(start.origin.x, start.mid().y), false);
    let mut dropped = start;
    for grown in [30., 60., 90.] {
        dropped = rect(
            start.origin.x - grown,
            start.origin.y,
            start.size.width + grown,
            start.size.height,
        );
        strip.app_reports(last, dropped, MouseState::Down);
    }
    let ratio = dropped.size.width / strip.tiling().size.width;
    assert_eq!((1. / ratio).round(), 2., "two abreast: {ratio}");
    strip.reactor.handle_event(Event::MouseUp);

    let flights = strip_movements(&mut rx);
    assert_eq!(flights.len(), 1, "the release scrolls the strip once");
    let flight = &flights[0];
    let (from, to) = flight
        .windows
        .iter()
        .find(|w| w.window == last)
        .expect("on the surface")
        .travel(flight.from_offset, SurfacePoint::origin());
    for (end, drawn) in [("from", from), ("to", to)] {
        assert_eq!(
            (drawn.origin.x, drawn.size.width),
            (dropped.origin.x, dropped.size.width),
            "{end}: let go at {dropped:?}"
        );
    }
    assert_eq!(strip.settle(last), Vec::<CGRect>::new(), "nothing to write");
    assert_eq!(strip.on_screen(last), dropped);
}

/// Kai, 2026-10-07: "if the window's right edge is at the screens right edge and i _shrink_ the
/// window by the left edge, on release during the strip animation the off-screen window on the
/// right jumps under the resized window by what looks like ~100px and then animates away". The
/// shrink keeps the right edge, so the column after it keeps its place beyond the screen's right
/// edge while the columns before it scroll right. "Resizing with the mouse" in `specs/strip.md`.
#[test]
fn a_left_edge_shrink_flush_right_leaves_the_column_beyond_the_right_edge_where_it_was() {
    let mut strip = Strip::with_columns(3);
    let settings = &mut strip.reactor.config.settings.layout;
    settings.scrolling.column_width_ratio = 0.6;
    // The live display's gaps: the column after the flush one starts on the screen's edge and is
    // parked in the corner.
    settings.gaps.gaps = 3.;
    settings.gaps.struts = Edges { top: 0., left: 0., bottom: 0., right: 0. };
    strip
        .reactor
        .layout_manager
        .layout_engine
        .set_layout_settings(&strip.reactor.config.settings.layout);
    strip.reactor.send_layout_event(LayoutEvent::WindowFocused(strip.space, strip.window));
    strip.settle(strip.window);
    strip.reactor.send_layout_event(LayoutEvent::WindowFocused(strip.space, strip.other));
    strip.settle(strip.other);
    let beyond = WindowId::new(1, 3);
    let start = strip.on_screen(strip.other);
    assert!((start.max().x - strip.tiling().max().x).abs() <= 1., "flush right: {start:?}");
    let parked = strip.on_screen(beyond);
    assert!(parked.origin.y > start.max().y, "parked in the corner: {parked:?}");
    let window_before = strip.on_screen(strip.window);
    strip.reactor.config.settings.animate = true;
    let (animation_tx, mut rx) = crate::app::channels::channel();
    strip.reactor.communication_manager.transitions_tx = Some(animation_tx);

    strip.press(CGPoint::new(start.origin.x, start.mid().y), false);
    for shrunk in [30., 60., 90.] {
        strip.app_reports(
            strip.other,
            rect(start.origin.x + shrunk, start.origin.y, start.size.width - shrunk, start.size.height),
            MouseState::Down,
        );
    }
    strip.reactor.handle_event(Event::MouseUp);

    let flights = strip_movements(&mut rx);
    assert_eq!(flights.len(), 1, "the release scrolls the strip once");
    let flight = &flights[0];
    assert!((flight.from_offset.x - 90.).abs() <= 2., "by the shrink: {:?}", flight.from_offset);
    let travel = |window| {
        flight
            .windows
            .iter()
            .find(|w| w.window == window)
            .expect("on the surface")
            .travel(flight.from_offset, SurfacePoint::origin())
    };
    let (from, to) = travel(beyond);
    assert!(from.origin.x >= DISPLAY.max().x, "off screen throughout, not from {from:?}");
    assert!((from.origin.x - to.origin.x).abs() <= 1., "where it was: {from:?} -> {to:?}");
    let (from, to) = travel(strip.window);
    assert!(
        (from.origin.x - window_before.origin.x).abs() <= 1.,
        "the column before pans from where it stands, {window_before:?}, not {from:?}"
    );
    assert!((to.origin.x - from.origin.x - 90.).abs() <= 2., "by the shrink: {from:?} -> {to:?}");
}

/// A double-click on a title bar has the app zoom its window, and rini has to follow it to its own
/// full width rather than take it for a resize. "Full width and height" in `specs/strip.md`.
mod title_bar_zoom {
    use std::time::Duration;

    use test_log::test;

    use super::*;
    use crate::app::channels;
    use crate::windows::domain::zoom::Evidence;

    impl Strip {
        fn clamped_width(&self) -> f64 {
            let max = self.reactor.config.settings.layout.scrolling.max_column_width_ratio;
            (self.tiling().size.width * max).round()
        }

        fn full_width(&self) -> Option<bool> {
            self.reactor
                .layout_manager
                .layout_engine
                .full_width_of_column(self.space, self.window)
        }

        fn title_bar_of(&self, window: WindowId) -> CGPoint {
            let frame = self.frame_of(window);
            CGPoint::new(frame.mid().x, frame.origin.y + 12.)
        }

        fn double_click_title_bar(&mut self) {
            let title_bar = self.title_bar_of(self.window);
            self.press(title_bar, true);
        }

        fn zoom(&mut self, frame: CGRect) {
            self.double_click_title_bar();
            self.app_reports(self.window, frame, MouseState::Up);
        }

        /// A later click back onto the window: where the zoom used to be undone.
        fn click_back(&mut self) {
            let content = self.frame_of(self.window).mid();
            self.press(content, false);
            self.reactor.handle_event(Event::MouseUp);
            self.reactor
                .send_layout_event(LayoutEvent::WindowFocused(self.space, self.window));
        }

        fn assert_ends_at_full_width(&mut self, case: &str) {
            assert_eq!(self.full_width(), Some(true), "{case}");
            let full = self.laid_out(self.window);
            assert!(
                full.size.same_as(self.tiling().size),
                "{case}: rini's full width, not {full:?}"
            );
            let written = self.settle(self.window);
            assert_eq!(
                written.last(),
                Some(&full),
                "{case}: written at once: {written:?}"
            );
            assert_eq!(self.on_screen(self.window), full, "{case}");

            self.click_back();
            assert_eq!(
                self.settle(self.window),
                Vec::<CGRect>::new(),
                "{case}: the next click writes nothing back"
            );
            assert_eq!(self.full_width(), Some(true), "{case}");
        }

        fn assert_adopted_as_before(&mut self, case: &str) {
            assert_eq!(self.full_width(), Some(false), "{case}");
            assert_eq!(self.laid_out(self.window).size.width, self.clamped_width(), "{case}");
        }
    }

    #[test]
    fn a_title_bar_double_click_and_a_frame_filling_the_screen_make_the_full_width_column() {
        let mut strip = Strip::new();
        strip.zoom(VISIBLE);
        strip.assert_ends_at_full_width("zoomed to the screen");
    }

    #[test]
    fn a_zoom_that_fits_the_window_to_its_content_also_ends_at_full_width() {
        let mut strip = Strip::new();
        strip.zoom(rect(140., 40., 1150., 700.));
        strip.assert_ends_at_full_width("zoomed to its content");
    }

    #[test]
    fn a_double_click_in_the_content_or_on_another_window_is_not_a_zoom() {
        for case in ["content", "another window's title bar"] {
            let mut strip = Strip::new();
            let at = match case {
                "content" => {
                    let frame = strip.frame_of(strip.window);
                    CGPoint::new(frame.mid().x, frame.origin.y + 200.)
                }
                _ => strip.title_bar_of(strip.other),
            };
            strip.press(at, true);
            strip.app_reports(strip.window, VISIBLE, MouseState::Up);
            strip.assert_adopted_as_before(case);
        }
    }

    #[test]
    fn native_fullscreen_is_not_a_zoom() {
        let mut strip = Strip::new();
        strip.zoom(DISPLAY);
        assert_eq!(strip.full_width(), Some(false));
        assert_eq!(strip.reactor.drag_manager.title_bar_click, None, "spent");
    }

    #[test]
    fn a_resize_with_no_double_click_behind_it_is_adopted_as_before() {
        for stale in [false, true] {
            let mut strip = Strip::new();
            if stale {
                let long_ago = Instant::now() - Duration::from_secs(2);
                strip.reactor.drag_manager.title_bar_click =
                    Some(Evidence::new(strip.window, long_ago));
            }
            strip.app_reports(strip.window, VISIBLE, MouseState::Up);
            strip.assert_adopted_as_before(if stale { "stale" } else { "none" });
        }
    }

    // From the default 0.7, the next preset `Mod+R` reaches is 1.0: a width, not the mode.
    #[test]
    fn mod_f_or_mod_r_after_a_double_click_keeps_the_width_it_gave() {
        for (command, maximised) in [
            (LayoutCommand::ToggleFullscreenWithinGaps, true),
            (LayoutCommand::CyclePresetColumnWidth, false),
        ] {
            let mut strip = Strip::new();
            strip.double_click_title_bar();
            assert!(strip.reactor.drag_manager.title_bar_click.is_some());

            strip.reactor.handle_test_layout_command(command.clone());
            assert_eq!(strip.reactor.drag_manager.title_bar_click, None, "{command:?}");
            strip.settle(strip.window);
            assert_eq!(strip.full_width(), Some(maximised), "{command:?}");
            let tiling = strip.tiling();
            let full = strip.laid_out(strip.window);
            assert_eq!((full.origin.x, full.size.width), (tiling.origin.x, tiling.size.width));

            // The app snapping its height to a grid after rini's write.
            let snapped =
                CGRect::new(full.origin, CGSize::new(full.size.width, full.size.height - 4.));
            strip.app_reports(strip.window, snapped, MouseState::Up);
            assert_eq!(strip.full_width(), Some(maximised), "{command:?}");
            let after = strip.laid_out(strip.window);
            assert_eq!((after.origin.x, after.size.width), (tiling.origin.x, tiling.size.width));
        }
    }

    #[test]
    fn the_rest_of_a_zoom_ends_at_rini_s_frame_and_does_not_pin_the_window() {
        let mut strip = Strip::new();
        strip.zoom(VISIBLE);
        strip.settle(strip.window);
        let full = strip.laid_out(strip.window);
        assert_eq!(strip.on_screen(strip.window), full);

        // The app putting its own zoom frame back after rini's write.
        strip.app_reports(strip.window, VISIBLE, MouseState::Up);
        assert_ne!(
            strip.reactor.drag_manager.skip_layout_for_window,
            Some(strip.window)
        );
        assert_eq!(strip.settle(strip.window).last(), Some(&full), "rini's frame again, at once");
        assert_eq!(strip.on_screen(strip.window), full);
        assert_eq!(strip.full_width(), Some(true), "one double-click toggles once");

        strip.click_back();
        assert_eq!(strip.settle(strip.window), Vec::<CGRect>::new());
    }

    #[test]
    fn a_second_title_bar_double_click_restores_the_width_it_had() {
        for unzoomed_to in ["its own frame", "the screen again"] {
            let mut strip = Strip::new();
            let before = strip.laid_out(strip.window);
            strip.zoom(VISIBLE);
            strip.settle(strip.window);
            assert_eq!(strip.full_width(), Some(true));

            strip.zoom(if unzoomed_to == "its own frame" {
                before
            } else {
                VISIBLE
            });
            assert_eq!(strip.full_width(), Some(false), "{unzoomed_to}");
            assert_eq!(strip.laid_out(strip.window), before, "{unzoomed_to}: back to its width");
            strip.settle(strip.window);
            strip.click_back();
            assert_eq!(strip.laid_out(strip.window), before, "{unzoomed_to}: and it stays there");
        }
    }

    /// rini's full width is the tiling area's, and the window ends inside it by the border.
    #[test]
    fn a_title_bar_zoom_with_a_border_on_ends_at_the_inset_full_width_frame() {
        let mut strip = Strip::new().with_border(4.);
        strip.zoom(VISIBLE);
        assert_eq!(strip.full_width(), Some(true));
        let full = strip.laid_out(strip.window);
        assert_eq!(full, window_in(strip.tiling(), 4.));
        assert_eq!(strip.settle(strip.window).last(), Some(&full), "written at once");
        assert_eq!(strip.on_screen(strip.window), full);
        strip.click_back();
        assert_eq!(strip.settle(strip.window), Vec::<CGRect>::new(), "the next click writes nothing back");
        assert_eq!(strip.full_width(), Some(true));
    }

    #[test]
    fn a_zoom_with_the_button_still_down_ends_at_full_width() {
        let mut strip = Strip::new();
        strip.double_click_title_bar();
        strip.app_reports(strip.window, VISIBLE, MouseState::Down);
        strip.assert_ends_at_full_width("the second press still held");
    }

    // rini's last write still pending when the app zooms: the report used to read as that write
    // landing a few points off and be swallowed, and the click wrote the column back.
    #[test]
    fn a_zoom_over_a_pending_write_still_ends_at_full_width() {
        let mut strip = Strip::new();
        let wsid = strip.reactor.test_window_server_id(strip.window);
        let before = strip.apps.windows[&strip.window].frame;
        let txid = strip.apps.windows[&strip.window].last_seen_txid;
        let pending = CGRect::new(
            before.origin,
            CGSize::new(before.size.width - 3.0, before.size.height),
        );
        strip.reactor.transaction_manager.store_txid(wsid, txid, pending);

        strip.zoom(VISIBLE);
        strip.assert_ends_at_full_width("over a pending write");
    }

    #[test]
    fn a_drag_that_starts_after_the_double_click_is_a_resize() {
        let mut strip = Strip::new();
        strip.double_click_title_bar();
        let frame = strip.frame_of(strip.window);
        strip.press(CGPoint::new(frame.max().x, frame.mid().y), false);
        for width in [1020., 1050., 1080.] {
            strip.app_reports(strip.window, 
                CGRect::new(frame.origin, CGSize::new(width, frame.size.height)),
                MouseState::Down,
            );
        }
        strip.reactor.handle_event(Event::MouseUp);
        assert_eq!(strip.full_width(), Some(false));
        assert_eq!(strip.laid_out(strip.window).size.width, 1080.);
    }

    // The input tap hands the press straight to the reactor's channel, while the app answers the
    // press from its own thread: whatever order the two land in, the press is handled first.
    #[test]
    fn the_press_from_the_tap_is_handled_before_a_frame_report_sent_after_it() {
        let mut strip = Strip::new();
        let (tx, mut rx) = channels::channel();
        let sink = Event::left_press_sink(tx.clone());
        sink(LeftPress {
            location: strip.title_bar_of(strip.window),
            at: Instant::now(),
            double_click: true,
        });
        let window = strip.apps.windows.get_mut(&strip.window).expect("test window");
        window.frame = VISIBLE;
        tx.send(Event::WindowFrameChanged(
            strip.window,
            VISIBLE,
            Some(window.last_seen_txid),
            Requested(false),
            Some(MouseState::Up),
        ));
        // One by one rather than as a loop batch, which would mark the window server busy for the
        // whole process and hold up the display-churn tests that follow.
        while let Ok((_, event)) = rx.try_recv() {
            strip.reactor.handle_event(event);
        }

        strip.assert_ends_at_full_width("through the channel");
    }

    #[test]
    fn a_floating_window_is_left_to_its_app() {
        let (mut reactor, wid, space, screen, floating_frame) = reactor_with_floating_window();
        let before = laid_out_frame(&mut reactor, space, screen, wid);

        reactor.handle_event(Event::LeftMouseDown(LeftPress {
            location: CGPoint::new(floating_frame.mid().x, floating_frame.origin.y + 10.),
            at: Instant::now(),
            double_click: true,
        }));
        assert_eq!(reactor.drag_manager.title_bar_click, None);
        reactor.handle_event(Event::WindowFrameChanged(
            wid,
            screen,
            None,
            Requested(false),
            Some(MouseState::Up),
        ));

        assert!(reactor.layout_manager.layout_engine.is_window_floating(wid));
        assert_eq!(
            reactor.layout_manager.layout_engine.full_width_of_column(space, wid),
            None
        );
        assert_eq!(laid_out_frame(&mut reactor, space, screen, wid), before);
        assert_eq!(before, Some(floating_frame));
    }
}

/// One 1728pt display with `count` columns `ratio` of the viewport wide, the first focused, flying
/// through a captured animation channel from here on.
fn moving_strip(
    ratio: f64,
    count: usize,
) -> (
    Apps,
    Reactor,
    crate::app::channels::Receiver<crate::transitions::platform::engine::Event>,
) {
    moving_strip_with(ratio, count, |_| {})
}

/// `moving_strip` with the strip's settings changed by `configure` first.
fn moving_strip_with(
    ratio: f64,
    count: usize,
    configure: impl FnOnce(&mut crate::layout::settings::ScrollingLayoutSettings),
) -> (
    Apps,
    Reactor,
    crate::app::channels::Receiver<crate::transitions::platform::engine::Event>,
) {
    let mut layout = crate::app::config::LayoutSettings::default();
    layout.scrolling.column_width_ratio = ratio;
    layout.scrolling.max_column_width_ratio = ratio.max(layout.scrolling.max_column_width_ratio);
    configure(&mut layout.scrolling);
    let mut reactor = Reactor::new_for_test(crate::workspaces::LayoutEngine::new(
        &crate::app::config::VirtualWorkspaceSettings::default(),
        &layout,
    ));
    let mut apps = Apps::new();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(count));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(1, 1)));
    apps.simulate_until_quiet(&mut reactor);
    reactor.config.settings.animate = true;
    let (animation_tx, animation_rx) = crate::app::channels::channel();
    reactor.communication_manager.transitions_tx = Some(animation_tx);
    (apps, reactor, animation_rx)
}

/// Every layout pass the reactor handed the overlay since the last look, each landed as the engine
/// lands it, and every write carried out: the app answers each with the frame it was given.
fn land_layout_passes(
    apps: &mut Apps,
    reactor: &mut Reactor,
    rx: &mut crate::app::channels::Receiver<crate::transitions::platform::engine::Event>,
) -> Vec<Vec<crate::transitions::domain::request::AnimationRequest>> {
    use crate::transitions::domain::travel::is_moving;
    use crate::transitions::platform::engine::Event as Engine;
    let mut passes = Vec::new();
    while let Ok((_, event)) = rx.try_recv() {
        let frames: Vec<(WindowId, CGRect)> = match event {
            Engine::Animate { windows, .. } => {
                let frames = windows
                    .iter()
                    .filter(|request| is_moving(request.from, request.to))
                    .map(|request| (request.window, request.to.to_cg()))
                    .collect();
                passes.push(windows);
                frames
            }
            Engine::AnimateSurface { final_frames, .. } => {
                final_frames.iter().map(|&(window, frame)| (window, frame.to_cg())).collect()
            }
            _ => continue,
        };
        reactor.handle_event(Event::ApplyOverlayFrames(frames));
        apps.simulate_until_quiet(reactor);
    }
    apps.simulate_until_quiet(reactor);
    passes
}

/// Kai's report of 2026-10-09, "alt-R is broken now, doesn't animate anything": the strip's last
/// column at the left edge, the columns before it parked, is resized where it stands by `alt-R` with
/// nothing else on screen moving, and the pass was placed rather than flown. Every press flies, from
/// each width the column can be at, the app answering each write with the frame it was given.
#[test]
fn every_alt_r_on_the_last_column_flies() {
    use crate::layout::ResizeOrientation;
    use crate::layout::settings::CenterFocusedColumn;
    let space = SpaceId::new(1);
    for columns in 1..=4 {
        for start in ["a third", "a half", "two thirds", "Mod+F", "ratio 1.0"] {
            let case = format!("{columns} columns from {start}");
            let (mut apps, mut reactor, mut rx) = moving_strip_with(0.5, columns, |scrolling| {
                scrolling.min_column_width_ratio = 0.33333;
                scrolling.max_column_width_ratio = if start == "ratio 1.0" { 1.0 } else { 0.66667 };
                scrolling.preset_column_widths = vec![0.33333, 0.5, 0.66667, 1.0];
                scrolling.center_focused_column = CenterFocusedColumn::Never;
            });
            let last = WindowId::new(1, columns as u32);
            reactor.send_layout_event(LayoutEvent::WindowFocused(space, last));
            let resize_by = |amount| LayoutCommand::ResizeWindowBy {
                amount,
                orientation: ResizeOrientation::Horizontal,
            };
            match start {
                "a third" => reactor.handle_test_layout_command(resize_by(0.33333 - 0.5)),
                "two thirds" => reactor.handle_test_layout_command(resize_by(0.66667 - 0.5)),
                "ratio 1.0" => reactor.handle_test_layout_command(resize_by(0.5)),
                "Mod+F" => {
                    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreenWithinGaps)
                }
                _ => {}
            }
            land_layout_passes(&mut apps, &mut reactor, &mut rx);
            let full = reactor.layout_manager.layout_engine.full_width_of_column(space, last);
            assert_eq!(full, Some(start == "Mod+F"), "{case}");

            for press in 0..5 {
                let case = format!("{case}, press {press}");
                press_flies(&mut apps, &mut reactor, &mut rx, last, LayoutCommand::CyclePresetColumnWidth, &case);
            }
        }
    }
}

/// `command` resizes `window`'s column, and the pass that does it flies the window from the width it
/// had to the frame the app is left at.
fn press_flies(
    apps: &mut Apps,
    reactor: &mut Reactor,
    rx: &mut crate::app::channels::Receiver<crate::transitions::platform::engine::Event>,
    window: WindowId,
    command: LayoutCommand,
    case: &str,
) {
    let width = apps.windows[&window].frame.size.width;
    reactor.handle_test_layout_command(command);
    let passes = land_layout_passes(apps, reactor, rx);
    let resized = apps.windows[&window].frame;
    assert_ne!(resized.size.width, width, "{case}: the width changed");
    let flown = passes.iter().flatten().find(|request| request.window == window && request.to.to_cg() == resized);
    let flown = flown.unwrap_or_else(|| panic!("{case}: {passes:?}"));
    assert_eq!(flown.from.size.width, width, "{case}");
}

/// The other two ways `specs/animation.md` names of resizing a column where it stands, on the same
/// last column with the columns before it parked: a resize key, and the full-width toggle both ways.
#[test]
fn a_resize_key_and_the_full_width_toggle_on_the_last_column_fly() {
    use crate::layout::ResizeOrientation;
    use crate::layout::settings::CenterFocusedColumn;
    let space = SpaceId::new(1);
    for columns in 1..=4 {
        let (mut apps, mut reactor, mut rx) = moving_strip_with(0.5, columns, |scrolling| {
            scrolling.min_column_width_ratio = 0.33333;
            scrolling.max_column_width_ratio = 0.66667;
            scrolling.center_focused_column = CenterFocusedColumn::Never;
        });
        let last = WindowId::new(1, columns as u32);
        reactor.send_layout_event(LayoutEvent::WindowFocused(space, last));
        land_layout_passes(&mut apps, &mut reactor, &mut rx);
        let presses = [
            ("a shrink key", LayoutCommand::ResizeWindowBy { amount: -0.1, orientation: ResizeOrientation::Horizontal }),
            ("a grow key", LayoutCommand::ResizeWindowBy { amount: 0.1, orientation: ResizeOrientation::Horizontal }),
            ("Mod+F", LayoutCommand::ToggleFullscreenWithinGaps),
            ("Mod+F back", LayoutCommand::ToggleFullscreenWithinGaps),
        ];
        for (press, command) in presses {
            press_flies(&mut apps, &mut reactor, &mut rx, last, command, &format!("{columns} columns, {press}"));
        }
        assert_eq!(reactor.layout_manager.layout_engine.full_width_of_column(space, last), Some(false));
    }
}

/// A click or an app activation reaches the reactor as a focus report before the pass it causes, and a
/// key press as a command before rini's raise is reported: either way the strip movement names the
/// window focused as the event began, which is still drawn focused on screen at frame zero.
#[test]
fn a_strip_movement_names_the_window_focused_as_its_event_began() {
    let (_apps, mut reactor, mut rx) = moving_strip(0.5, 4);
    let space = SpaceId::new(1);
    let (first, second, third) = (WindowId::new(1, 1), WindowId::new(1, 2), WindowId::new(1, 3));
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    strip_movements(&mut rx);
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    let pressed = strip_movements(&mut rx);
    let flight = pressed.last().expect("the press scrolls the strip");
    assert_eq!((flight.focus, flight.focused_before), (Some(third), Some(second)));

    // cmd-` onto the column scrolled off: macOS makes it key, and reports it, before the layout moves.
    reactor.handle_event(Event::WindowServerFocusChanged(first, space));
    let reported = strip_movements(&mut rx);
    let flight = reported.last().expect("the report scrolls the strip");
    assert_eq!((flight.focus, flight.focused_before), (Some(first), Some(third)));
}

/// Frame zero pictures the window focus leaves drawn focused only if it was the key window as the
/// request went out. A key press goes out before rini's raise is reported, so it names the window focus
/// leaves; another application's panel taking the keys leaves none; a click's report has made its
/// target key first.
#[test]
fn a_strip_movement_names_the_key_window_as_it_went_out() {
    let (_apps, mut reactor, mut rx) = moving_strip(0.5, 4);
    let space = SpaceId::new(1);
    let (first, second, third) = (WindowId::new(1, 1), WindowId::new(1, 2), WindowId::new(1, 3));
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    strip_movements(&mut rx);
    reactor.main_window_tracker.set_focus_for_test(second);
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    let flight = strip_movements(&mut rx).pop().expect("the press scrolls the strip");
    assert_eq!((flight.focused_before, flight.key), (Some(second), Some(second)));

    reactor.handle_event(Event::WindowServerFocusChanged(WindowId::new(2, 90), space));
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    let flight = strip_movements(&mut rx).pop().expect("the press scrolls the strip");
    assert_eq!((flight.focused_before, flight.key), (Some(third), None), "a panel has the keys");

    reactor.handle_event(Event::WindowServerFocusChanged(first, space));
    let flight = strip_movements(&mut rx).pop().expect("the report scrolls the strip");
    assert_eq!(flight.key, Some(first), "the click made its target key before the request");
}

/// Every request the reactor sent the flight engine since the last look, by kind, with the event it
/// answers.
fn causes(
    rx: &mut crate::app::channels::Receiver<crate::transitions::platform::engine::Event>,
) -> Vec<(&'static str, crate::transitions::domain::request::Cause)> {
    use crate::transitions::platform::engine::Event as Engine;
    let mut out = Vec::new();
    while let Ok((_, event)) = rx.try_recv() {
        out.push(match event {
            Engine::Animate { cause, .. } => ("pass", cause),
            Engine::AnimateSurface { cause, .. } => ("movement", cause),
            Engine::Bounce { cause, .. } => ("bounce", cause),
            Engine::DebugSlide { cause, .. } => ("slide", cause),
            _ => continue,
        });
    }
    out
}

/// A flight's journal starts at the press: each request names the command it answers, stamped when
/// the reactor took it, whichever of the five ways it goes to the flight engine. `None` is a press
/// that may send nothing: the second column is in view.
#[test]
fn a_commands_flight_carries_its_cause() {
    let (_apps, mut reactor, mut rx) = moving_strip(0.5, 4);
    let layout = |command| (Command::Layout(command), None);
    let sends = |command, request| (Command::Layout(command), Some(request));
    let cases = [
        sends(LayoutCommand::MoveFocus(Direction::Left), "bounce"),
        layout(LayoutCommand::MoveFocus(Direction::Right)),
        sends(LayoutCommand::MoveFocus(Direction::Right), "movement"),
        sends(LayoutCommand::ToggleFullscreenWithinGaps, "pass"),
        sends(LayoutCommand::NextWorkspace(None), "movement"),
        sends(LayoutCommand::PrevWorkspace(None), "movement"),
        (
            Command::Reactor(ReactorCommand::DebugOverlaySlide { dx: 0, dy: 40, duration_ms: 350 }),
            Some("slide"),
        ),
    ];
    for (command, request) in cases {
        let what: &'static str = match &command {
            Command::Layout(command) => command.into(),
            Command::Reactor(command) => command.into(),
            Command::Metrics(_) => unreachable!(),
        };
        let before = Instant::now();
        reactor.handle_event(Event::Command(command));
        let after = Instant::now();
        let sent = causes(&mut rx);
        if let Some(request) = request {
            assert!(sent.iter().any(|(kind, _)| *kind == request), "{what}: {sent:?}");
        }
        for (kind, cause) in sent {
            assert_eq!(cause.what, what, "{kind}");
            assert!(before <= cause.at && cause.at <= after, "{what}: taken as it was handled");
        }
    }
}

/// An app's frame report that releases a drag handles the mouse up inside it: the mouse up's flight
/// is its own, and the report's passes after it are the report's again.
#[test]
fn a_nested_event_leaves_the_outer_cause() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    reactor.handle_event(space_state_event(vec![screen], vec![Some(SpaceId::new(1))]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    reactor.config.settings.animate = true;
    let (animation_tx, mut rx) = crate::app::channels::channel();
    reactor.communication_manager.transitions_tx = Some(animation_tx);
    let window = WindowId::new(1, 1);
    let frame = reactor.state.windows.window(window).expect("window").frame_monotonic;
    let dragged = |dx, mouse| {
        let to = CGRect::new(CGPoint::new(frame.origin.x + dx, frame.origin.y + 50.), frame.size);
        Event::WindowFrameChanged(window, to, None, Requested(false), Some(mouse))
    };
    reactor.handle_event(dragged(200., MouseState::Down));
    causes(&mut rx);

    let report =
        crate::transitions::domain::request::Cause { at: Instant::now(), what: "window_frame_changed" };
    reactor.cause = report;
    let outcome = reactor.dispatch_workflow(dragged(210., MouseState::Up)).unwrap();
    assert!(outcome.dispatch_mouse_up, "the release is handled inside the report");
    reactor.apply_event_outcome(outcome);

    let sent = causes(&mut rx);
    assert!(!sent.is_empty(), "the mouse up lays the strip out again");
    assert!(sent.iter().all(|(_, cause)| cause.what == "mouse_up" && cause.at >= report.at), "{sent:?}");
    assert_eq!(reactor.cause, report, "what the report does after it carries the report");
}

/// Reported 2026-09-30: only the 50/50 pair at the start of a strip showed a move; everywhere else
/// the window jumped to its new place and a navigation pan played on top. A move that scrolls the
/// strip draws the moved window and the column it passes starting from each other's slots.
#[test]
fn a_move_that_scrolls_the_strip_starts_the_pair_from_each_others_slots() {
    let (_apps, mut reactor, mut rx) = moving_strip(0.5, 4);
    let moved = WindowId::new(1, 2);
    let passed = WindowId::new(1, 3);
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    strip_movements(&mut rx);

    reactor.handle_test_layout_command(LayoutCommand::MoveNode(Direction::Right));
    let flights = strip_movements(&mut rx);
    assert_eq!(flights.len(), 1, "one strip movement for the press");
    let SurfaceFlight { windows, from_offset, .. } = &flights[0];
    assert!(from_offset.x.abs() >= 1.0, "the strip scrolled: {from_offset:?}");
    let find = |window| windows.iter().find(|w| w.window == window).expect("on the surface");
    let (w, n) = (find(moved), find(passed));
    let w_from = w.from.expect("the moved window starts from its old slot");
    let n_from = n.from.expect("the column it passed starts from its old slot");
    assert_eq!(
        w_from.origin.x, n.frame.origin.x,
        "W left from where the passed column is now"
    );
    assert_eq!(
        n_from.origin.x,
        w.frame.origin.x + w.frame.size.width - n.frame.size.width,
        "the passed column left from W's side of the pair"
    );
    for other in windows.iter().filter(|s| s.window != moved && s.window != passed) {
        assert_eq!(other.from, None, "{:?} only pans", other.window);
    }
    let (start, end) = w.travel(*from_offset, SurfacePoint::origin());
    assert!(
        (end.origin.x - start.origin.x).abs() < 1728.0 / 4.0,
        "the camera followed W: {start:?} to {end:?}"
    );
    assert_eq!(
        flights[0].accent, None,
        "a half-width column is seen moving and never steps"
    );

    // The move is spent: the next scroll is a plain pan.
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Left));
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Left));
    for flight in strip_movements(&mut rx) {
        assert!(flight.windows.iter().all(|w| w.from.is_none()));
        assert_eq!(flight.cue, crate::animation::domain::choreography::Cue::Plain);
        assert_eq!(flight.accent, None);
    }
}

/// Reported 2026-09-30: only a full-size window should wobble, and the wobble goes the other way.
/// A full-size window the camera follows does not move on screen at all, so the move steps the
/// whole strip a third of the display away from where it went and back, the column it passed held
/// on the strip beneath it. That column's real window still goes to its layout frame. The reactor
/// asks for the plain duration: the look stretches the flight only if it takes the step.
#[test]
fn a_full_width_move_steps_the_strip_away_and_holds_the_column_it_passes() {
    for (direction, sign) in [(Direction::Right, -1.0), (Direction::Left, 1.0)] {
        let (_apps, mut reactor, mut rx) = moving_strip(1.0, 3);
        let (moved, passed) = match direction {
            Direction::Right => (WindowId::new(1, 1), WindowId::new(1, 2)),
            _ => {
                reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
                strip_movements(&mut rx);
                (WindowId::new(1, 2), WindowId::new(1, 1))
            }
        };

        reactor.handle_test_layout_command(LayoutCommand::MoveNode(direction));
        let flights = strip_movements(&mut rx);
        assert_eq!(flights.len(), 1, "{direction:?}");
        let flight = &flights[0];
        assert_eq!(
            flight.cue,
            crate::animation::domain::choreography::Cue::Move {
                window: moved,
                direction
            },
            "the look is told the press"
        );
        assert!(
            flight.from_offset.x.abs() >= 1.0,
            "{direction:?}: the strip scrolled to follow it"
        );
        let find = |window| *flight.windows.iter().find(|w| w.window == window).unwrap();
        let (w, n) = (find(moved), find(passed));
        let w_from = w.from.expect("the moved window starts from its old slot");
        assert_eq!(n.from, None, "{direction:?}: the column passed is held");
        let held_x = match direction {
            Direction::Right => w.frame.origin.x + w.frame.size.width - n.frame.size.width,
            _ => w.frame.origin.x,
        };
        assert_eq!(n.frame.origin.x, held_x, "{direction:?}: at its old slot");
        assert!(
            (w_from.origin.x - w.frame.origin.x).abs() >= 1.0,
            "{direction:?}: W starts from its old slot"
        );
        let display =
            ScreenRect::from_cg(CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.)));
        let (_, n_end) = n.travel(flight.from_offset, SurfacePoint::origin());
        let (_, w_end) = w.travel(flight.from_offset, SurfacePoint::origin());
        assert!(
            n_end.origin.x >= w_end.origin.x && n_end.max().x <= w_end.max().x,
            "{direction:?}: drawn ending beneath W, {n_end:?} under {w_end:?}"
        );
        let real = flight.final_frames.iter().find(|(window, _)| *window == passed);
        let real = real.expect("its real window is placed").1;
        assert!(
            rini_geometry::is_off_screen(display, real),
            "{direction:?}: its real window goes to the layout's frame, off screen: {real:?}"
        );

        let Some(crate::animation::domain::motion::accents::Accent::Step { offset: nudge, seconds }) =
            flight.accent
        else {
            panic!("{direction:?}: a full-width move steps");
        };
        assert!(
            (nudge.x - sign * 1728.0 / 3.0).abs() < 1e-9,
            "{direction:?}: {nudge:?}"
        );
        assert_eq!(nudge.y, 0.0);
        let plain = reactor.config.settings.animation_duration;
        assert!(
            (flight.duration.as_secs_f64() - plain).abs() < 1e-6,
            "{:?}",
            flight.duration
        );
        assert!((seconds - plain).abs() < 1e-6, "the step runs over its press's length: {seconds}");
    }
}

/// Only a full-size window steps: half and two-thirds columns are seen moving, so both columns
/// cross and nothing steps.
#[test]
fn a_narrower_move_never_steps() {
    for ratio in [0.5, 2.0 / 3.0] {
        let (_apps, mut reactor, mut rx) = moving_strip(ratio, 3);
        reactor.handle_test_layout_command(LayoutCommand::MoveNode(Direction::Right));
        let flights = strip_movements(&mut rx);
        assert_eq!(flights.len(), 1, "{ratio}");
        let flight = &flights[0];
        assert_eq!(flight.accent, None, "{ratio}");
        assert_eq!(
            flight.windows.iter().filter(|w| w.from.is_some()).count(),
            2,
            "{ratio}: both columns cross"
        );
    }
}

/// A half-width move the camera does not follow: the pair crosses with no step, and a move that
/// scrolls nothing is still a strip movement.
#[test]
fn a_move_the_camera_does_not_follow_is_not_nudged() {
    let (_apps, mut reactor, mut rx) = moving_strip(0.5, 3);
    reactor.handle_test_layout_command(LayoutCommand::MoveNode(Direction::Right));
    let flights = strip_movements(&mut rx);
    assert_eq!(flights.len(), 1);
    let flight = &flights[0];
    assert_eq!(flight.from_offset.x, 0.0, "the pair was already in view");
    assert_eq!(flight.windows.iter().filter(|w| w.from.is_some()).count(), 2);
    assert_eq!(flight.accent, None);
    assert!(
        (flight.duration.as_secs_f64() - reactor.config.settings.animation_duration).abs() < 1e-6
    );
}

/// A move that scrolls nothing places only the two windows that changed places: a column on screen
/// that did not move is not sent its own frame again, as on the per-window path.
#[test]
fn a_move_that_scrolls_nothing_places_only_the_pair() {
    let (_apps, mut reactor, mut rx) = moving_strip(0.33, 4);
    let unmoved = WindowId::new(1, 3);
    let on_screen = reactor.state.windows.window(unmoved).unwrap().frame_monotonic;
    assert!(on_screen.max().x <= 1728.0, "{on_screen:?}");

    reactor.handle_test_layout_command(LayoutCommand::MoveNode(Direction::Right));
    let flights = strip_movements(&mut rx);
    assert_eq!(flights.len(), 1);
    assert_eq!(flights[0].from_offset.x, 0.0, "nothing scrolled");
    let mut placed: Vec<WindowId> = flights[0].final_frames.iter().map(|(w, _)| *w).collect();
    placed.sort_by_key(|w| w.idx);
    assert_eq!(placed, vec![WindowId::new(1, 1), WindowId::new(1, 2)]);
}

/// Reported in review 2026-09-30: with `center-focused-column "never"`, centring a column
/// and then moving it drops the centring, so every window moves 576pt on screen with the scroll
/// offset unchanged, and the move was drawn as the finished layout at frame zero. Every window
/// starts where it really was.
#[test]
fn a_move_that_drops_the_centring_starts_every_window_where_it_was() {
    let (_apps, mut reactor, mut rx) = moving_strip_with(1.0 / 3.0, 5, |scrolling| {
        scrolling.center_focused_column = crate::layout::settings::CenterFocusedColumn::Never;
    });
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    reactor.handle_test_layout_command(LayoutCommand::CenterSelection);
    strip_movements(&mut rx);
    let was: Vec<(WindowId, CGRect)> = (1..=5)
        .map(|idx| {
            let window = WindowId::new(1, idx);
            (
                window,
                reactor.state.windows.window(window).unwrap().frame_monotonic,
            )
        })
        .collect();

    reactor.handle_test_layout_command(LayoutCommand::MoveNode(Direction::Right));
    let flights = strip_movements(&mut rx);
    assert_eq!(flights.len(), 1);
    let flight = &flights[0];
    assert!(
        flight.from_offset.x.abs() >= 1.0,
        "every window moved on screen: {:?}",
        flight.from_offset
    );
    assert_eq!(flight.windows.len(), 5);
    let display =
            ScreenRect::from_cg(CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.)));
    let viewport = rini_geometry::to_overlay_space(display, display);
    let mut seen = 0;
    for window in &flight.windows {
        let (start, _) = window.travel(flight.from_offset, SurfacePoint::origin());
        let (_, before) = was.iter().find(|(w, _)| *w == window.window).unwrap();
        if rini_geometry::is_off_screen(display, ScreenRect::from_cg(*before)) {
            assert!(
                rini_geometry::is_off_screen(viewport, start),
                "{:?} was parked, and is drawn from off screen, not {start:?}",
                window.window
            );
        } else {
            seen += 1;
            assert_eq!(
                start.origin.to_cg(), before.origin,
                "{:?} is drawn from where it was",
                window.window
            );
        }
    }
    assert_eq!(seen, 3, "the three columns in view");
    assert_eq!(
        flight.accent, None,
        "a third-width column is seen moving and never steps"
    );
}

/// Navigating is a pan and nothing else: no window starts anywhere but where the pan puts it, and
/// nothing is nudged.
#[test]
fn a_navigation_scroll_pans_with_no_swap_and_no_nudge() {
    let (_apps, mut reactor, mut rx) = moving_strip(0.5, 4);
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    let flights = strip_movements(&mut rx);
    assert!(!flights.is_empty(), "the strip scrolled to the third column");
    for flight in flights {
        assert!(flight.from_offset.x.abs() >= 1.0);
        assert!(flight.windows.iter().all(|w| w.from.is_none()));
        assert_eq!(flight.accent, None);
    }
}
