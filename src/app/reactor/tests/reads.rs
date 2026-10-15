//! What the reactor asks of the machine: it asks only the reads it was handed, and a test reactor's
//! reads know nothing until the test states it.
use std::cell::RefCell;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::WindowServerId;
use test_log::test;

use super::fixtures::*;
use crate::app::hotkeys::WmEvent;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::windows::domain::info::{AppInfo, WindowServerInfo};
use crate::windows::platform::window_server::Command as Told;
use crate::workspaces::{Direction, LayoutCommand, LayoutEvent};

thread_local! {
    static NOTED: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    static WATCHED: RefCell<Vec<Vec<u32>>> = const { RefCell::new(Vec::new()) };
}

#[test]
fn a_test_reactor_reads_unknown_from_everything_it_was_not_told() {
    let reactor = test_reactor();
    let (window, space) = (WindowServerId::new(10001), SpaceId::new(1));
    let port = &*reactor.reads.window_server;
    assert_eq!(port.get_window(window).map(|info| info.id), None);
    assert_eq!(port.window_ordered_in(window), None);
    assert!(port.space_window_list_for_connection(&[space.get()]).is_empty());
    assert!(port.front_to_back_depths().is_empty());
    assert_eq!(reactor.window_id_under_cursor(), None);
    assert_eq!((reactor.reads.mouse)(), None);
    assert!((reactor.reads.running_app)(4242).is_none());
    assert!(!(reactor.reads.low_power)());
    assert!(!(reactor.reads.display_churn)());
    assert_eq!(
        (reactor.reads.display_bounds)(rini_core::ids::ScreenId::new(1)),
        CGRect::ZERO
    );
    assert!(!port.focus_desktop("test-display-0", CGRect::new(CGPoint::ZERO, CGSize::new(1., 1.))));
}

/// The flight decision reads Low Power Mode from the reactor's reads, so no bounce flies in it.
#[test]
fn in_low_power_mode_pushing_past_an_end_does_not_bounce() {
    use crate::transitions::platform::engine::Event as Anim;
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.config.settings.animate = true;
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(1, 2)));
    apps.requests();
    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.transitions_tx = Some(animation_tx);
    let bounced = |rx: &mut channels::Receiver<Anim>| {
        std::iter::from_fn(|| rx.try_recv().ok())
            .any(|(_, event)| matches!(event, Anim::Bounce { .. }))
    };

    reactor.reads.low_power = || true;
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    assert!(!bounced(&mut animation_rx));

    reactor.reads.low_power = || false;
    reactor.handle_test_layout_command(LayoutCommand::MoveFocus(Direction::Right));
    assert!(bounced(&mut animation_rx), "the same push bounces out of it");
}

/// A window the window server shows for an application with no thread yet is read as that
/// application's launch, from the reactor's running-application read.
#[test]
fn a_window_of_an_app_with_no_thread_reads_its_launch_from_the_running_apps() {
    let mut reactor = test_reactor();
    let (wm_tx, mut wm_rx) = channels::channel();
    reactor.communication_manager.wm_sender = Some(wm_tx);
    let space = SpaceId::new(1);
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    let wsid = WindowServerId::new(611);
    reactor.window_server().set_window(
        wsid,
        Some(WindowServerInfo {
            id: wsid,
            pid: 77,
            layer: 0,
            frame: CGRect::new(CGPoint::new(10., 10.), CGSize::new(600., 400.)),
            min_frame: CGSize::ZERO,
            max_frame: CGSize::ZERO,
        }),
    );
    reactor.reads.running_app = |pid| {
        (pid == 77).then(|| AppInfo {
            bundle_id: Some("com.example.late".into()),
            localized_name: Some("Late".into()),
        })
    };

    window_server_appeared(&mut reactor, wsid, space, SpaceEventKind::User);

    let launched: Vec<_> = std::iter::from_fn(|| wm_rx.try_recv().ok())
        .filter_map(|(_, event)| match event {
            WmEvent::AppLaunch(pid, info) => Some((pid, info.bundle_id)),
            _ => None,
        })
        .collect();
    assert_eq!(launched, [(77, Some("com.example.late".to_string()))]);
}

/// Each window event the reactor takes is noted on the reads' activity clock, never the process's.
#[test]
fn a_window_event_is_noted_on_the_reads_activity_clock() {
    let mut reactor = test_reactor();
    reactor.reads.note_activity = |id| NOTED.with(|noted| noted.borrow_mut().push(id));
    reactor.handle_test_batch(vec![Event::WindowDestroyed(WindowId::new(5, 31))]);
    assert_eq!(NOTED.with(|noted| noted.borrow().clone()), [31]);
}

/// The windows the reactor tracks are handed to the reads to watch, once each time they change.
#[test]
fn the_tracked_windows_are_handed_to_the_reads_to_watch() {
    let (mut apps, mut reactor) = test_context();
    reactor.reads.watch_windows =
        |ids| WATCHED.with(|watched| watched.borrow_mut().push(ids.to_vec()));
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    reactor.handle_event(space_state_event(vec![screen], vec![Some(SpaceId::new(1))]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    let watched = WATCHED.with(|watched| watched.borrow().clone());
    let mut last = watched.last().cloned().expect("watched");
    last.sort_unstable();
    let tracked = [1, 2].map(|idx| reactor.test_window_server_id(WindowId::new(1, idx)).as_u32());
    assert_eq!(last, tracked);
}

/// Closing the last window of the active workspace makes the display's desktop key, through the
/// port, so the display keeps focus with no app window on it.
#[test]
fn closing_the_last_window_of_the_workspace_makes_the_display_s_desktop_key() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);
    apps.make_app_and_settle_on_screen(&mut reactor, screen, space, 1, make_windows(1));
    let desktop = WindowServerId::new(900);
    reactor
        .window_server()
        .set_desktop_windows("test-display-0", vec![desktop])
        .set_window(
            desktop,
            Some(WindowServerInfo {
                id: desktop,
                pid: 77,
                layer: -2147483603,
                frame: screen,
                min_frame: CGSize::ZERO,
                max_frame: CGSize::ZERO,
            }),
        );

    reactor.send_layout_event(LayoutEvent::WindowFocused(space, wid));
    assert_eq!(reactor.layout_engine.focused_window(), Some(wid));
    let wsid = reactor.test_window_server_id(wid);
    reactor.window_server().set_window_ordered_in(wsid, Some(true));
    reactor.handle_event(Event::WindowDestroyed(wid));

    assert!(
        reactor
            .window_server()
            .commands()
            .contains(&Told::MakeKeyWindow { pid: 77, window: desktop }),
        "the desktop was made key: {:?}",
        reactor.window_server().commands()
    );
}

/// The overlay spans the whole display the reads give, menu bar and Dock included, not the frame
/// left for windows.
#[test]
fn the_animation_overlay_spans_the_display_bounds_its_reads_give() {
    let (_apps, mut reactor) = test_context();
    reactor.handle_event(space_state_event(
        vec![
            CGRect::new(CGPoint::new(0., 25.), CGSize::new(1728., 1092.)),
            CGRect::new(CGPoint::new(1728., 25.), CGSize::new(1920., 1055.)),
        ],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
    ));
    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.transitions_tx = Some(animation_tx);
    reactor.reads.display_bounds = |screen| {
        if screen.as_u32() == 1 {
            CGRect::new(CGPoint::new(1728., 0.), CGSize::new(1920., 1080.))
        } else {
            CGRect::ZERO
        }
    };

    reactor.publish_animation_display_for(Some(SpaceId::new(2)));

    match animation_rx.try_recv().expect("a display is published").1 {
        crate::transitions::platform::engine::Event::SetDisplay { id, frame, .. } => {
            assert_eq!(id, 1);
            assert_eq!(
                frame,
                rini_geometry::space::ScreenRect::from_cg(CGRect::new(
                    CGPoint::new(1728., 0.),
                    CGSize::new(1920., 1080.)
                ))
            );
        }
        other => panic!("expected SetDisplay, got {other:?}"),
    }
}

/// A layout pass reads Low Power Mode from the reactor's reads, so no window flies in it.
#[test]
fn in_low_power_mode_a_layout_pass_flies_no_window() {
    use crate::transitions::platform::engine::Event as Anim;
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.config.settings.animate = true;
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    apps.requests();
    let (animation_tx, mut animation_rx) = channels::channel();
    reactor.communication_manager.transitions_tx = Some(animation_tx);
    let layout = |reactor: &Reactor, by: f64| -> Vec<(WindowId, CGRect)> {
        [(1, by), (2, -by)]
            .map(|(idx, dx)| {
                let wid = WindowId::new(1, idx);
                let frame = reactor.state.windows.window(wid).expect("window").frame_monotonic;
                (
                    wid,
                    CGRect::new(CGPoint::new(frame.origin.x + dx, frame.origin.y), frame.size),
                )
            })
            .to_vec()
    };
    let flew = |rx: &mut channels::Receiver<Anim>| {
        std::iter::from_fn(|| rx.try_recv().ok())
            .any(|(_, event)| matches!(event, Anim::Animate { .. }))
    };

    reactor.reads.low_power = || true;
    let moved = layout(&reactor, 120.);
    reactor.animate_layout(space, &moved, false, None);
    assert!(!flew(&mut animation_rx));

    reactor.reads.low_power = || false;
    let moved = layout(&reactor, 120.);
    reactor.animate_layout(space, &moved, false, None);
    assert!(flew(&mut animation_rx), "the same pass flies out of it");
}

/// A frame report that carries no mouse state takes the pointer from the reactor's reads: held
/// down, a report after rini's own write is the user's drag, and rini's target for it is dropped.
#[test]
fn a_frame_report_with_no_mouse_state_reads_the_pointer_from_the_reads() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    reactor.handle_event(space_state_event(vec![screen], vec![Some(SpaceId::new(1))]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(1));
    let wid = WindowId::new(1, 1);
    let wsid = reactor.test_window_server_id(wid);
    let report = |reactor: &mut Reactor| {
        let target = CGRect::new(CGPoint::new(200., 200.), CGSize::new(600., 400.));
        let txid = reactor.transactions.next_txid(wsid);
        reactor.transactions.insert(wsid, txid, target);
        let elsewhere = CGRect::new(CGPoint::new(260., 230.), CGSize::new(600., 400.));
        reactor.handle_event(Event::WindowFrameChanged(
            wid,
            elsewhere,
            Some(txid),
            Requested(false),
            None,
        ));
        reactor.transactions.target(&wsid)
    };

    assert!(
        report(&mut reactor).is_some(),
        "the pointer unknown, rini's write still stands"
    );
    reactor.reads.mouse = || Some(MouseState::Down);
    assert_eq!(
        report(&mut reactor),
        None,
        "held down, the report is the user's drag"
    );
}
