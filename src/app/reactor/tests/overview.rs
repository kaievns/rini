//! The overview on the reactor's side (`specs/overview.md`): its scene laid out from the real
//! layout, every press inside it run on that layout with nothing flying, the mouse kept from focus
//! and the pointer until it closes, and nothing held once it has.

use objc2_core_foundation::{CGPoint, CGSize};
use rini_geometry::space::{OverlayVector, ToCg};
use rini_ipc::protocol::{ReactorCommand, WorkspaceSelector};
use test_log::test;

use crate::animation::domain::choreography::Cue;
use crate::animation::domain::motion::plan::EDGE_BOUNCE_OVERSHOOT;
use crate::app::channels;
use crate::app::reactor::overview::Press;
use crate::app::reactor::testing::*;
use crate::app::reactor::tests::fixtures::strip_movements;
use crate::app::reactor::*;
use crate::input::domain::switch_session::{OverviewSignal, Signal};
use crate::input::platform::input_tap::Request as Tap;
use crate::transitions::platform::engine::Event as Engine;
use crate::windows::domain::request::Request;
use crate::workspaces::{Direction, LayoutCommand};

type Flights = channels::Receiver<Engine>;

/// The laptop's display, and its frame below the 32pt bar.
fn display() -> CGRect { CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.)) }

fn usable() -> CGRect { CGRect::new(CGPoint::new(0., 32.), CGSize::new(1728., 1085.)) }

fn space() -> SpaceId { SpaceId::new(1) }

fn w(idx: u32) -> WindowId { WindowId::new(1, idx) }

/// Four workspaces on the laptop's display: w1 and w2 on the first, w1 focused there last, w3 to
/// w5 on the second, which it shows with w3 focused, none on the third, and w6 on the fourth.
fn desk() -> (Apps, Reactor) {
    let mut reactor = test_reactor_with_workspace_count(4);
    reactor.reads_mut().display_bounds = |_| display();
    let mut apps = Apps::new();
    let mut screens = make_screen_snapshots(vec![usable()], vec![Some(space())]);
    screens[0].bounds = display();
    reactor.handle_event(space_state_event_from_screens(screens));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(6));
    for (window, workspace) in [(3, 1), (4, 1), (5, 1), (6, 3)] {
        reactor.handle_test_layout_command(LayoutCommand::MoveWindowToWorkspace {
            workspace: WorkspaceSelector::Index(workspace),
            follow: false,
            window_id: Some(window),
        });
        apps.simulate_until_quiet(&mut reactor);
    }
    focus(&mut apps, &mut reactor, w(1));
    reactor.handle_test_layout_command(LayoutCommand::SwitchToWorkspace(1));
    apps.simulate_until_quiet(&mut reactor);
    focus(&mut apps, &mut reactor, w(3));
    (apps, reactor)
}

fn focus(apps: &mut Apps, reactor: &mut Reactor, window: WindowId) {
    reactor.send_layout_event(LayoutEvent::WindowFocused(space(), window));
    reactor.handle_event(Event::WindowServerFocusChanged(window, space()));
    reactor.set_test_focus(window);
    apps.simulate_until_quiet(reactor);
}

/// `events` as one batch of the reactor's loop, then the apps' answers.
fn batch(apps: &mut Apps, reactor: &mut Reactor, events: Vec<Event>) {
    reactor.handle_test_batch(events);
    apps.simulate_until_quiet(reactor);
}

fn overview(signal: OverviewSignal) -> Event { Event::Switch(Signal::Overview(signal)) }

fn layout(command: LayoutCommand) -> Event { Event::Command(Command::Layout(command)) }

fn open(apps: &mut Apps, reactor: &mut Reactor) {
    batch(apps, reactor, vec![overview(OverviewSignal::Open)]);
    assert!(reactor.overview_is_open());
}

/// A press, in a batch of its own.
fn press(apps: &mut Apps, reactor: &mut Reactor, command: LayoutCommand) {
    batch(apps, reactor, vec![layout(command)]);
}

/// Whether the engine was sent anything that flies since the last look.
fn flew(rx: &mut Flights) -> bool {
    let mut flew = false;
    while let Ok((_, event)) = rx.try_recv() {
        flew |= event.pressed().is_some();
    }
    flew
}

fn the_tap_was_sent(rx: &mut channels::Receiver<Tap>) -> Vec<Tap> {
    let mut sent = Vec::new();
    while let Ok((_, request)) = rx.try_recv() {
        sent.push(request);
    }
    sent
}

fn selected(reactor: &Reactor) -> (usize, Option<WindowId>) {
    let selection = reactor.overview_scene().expect("the overview is open").selected;
    (selection.row, selection.window)
}

/// The overview opens and closes on the tap's signals, changing no focus either way and opening no
/// switch: every press inside it runs as it does outside.
#[test]
fn the_overview_opens_and_closes_on_the_taps_signals_and_changes_nothing() {
    let (mut apps, mut reactor) = desk();
    let opened = reactor.dispatch_test_switch(Signal::Overview(OverviewSignal::Open));
    assert!(reactor.overview_is_open());
    assert!(!reactor.switch_is_open());
    assert!(opened.raise_requests.is_empty() && opened.focused_window.is_none());

    let closed = reactor.dispatch_test_switch(Signal::Overview(OverviewSignal::Close));
    assert!(!reactor.overview_is_open());
    assert!(closed.raise_requests.is_empty() && closed.focused_window.is_none());
    assert_eq!(reactor.layout().focused_window(), Some(w(3)));
    apps.requests();
}

/// Opening lays every workspace of the display out as a row, in the stack's order, the empty one
/// included, each holding its own windows, the shown one's focused window selected.
#[test]
fn opening_lays_out_every_workspace_of_the_display_as_a_row() {
    let (mut apps, mut reactor) = desk();
    open(&mut apps, &mut reactor);
    let scene = reactor.overview_scene().unwrap();
    let rows: Vec<Vec<WindowId>> = scene
        .rows
        .iter()
        .map(|row| {
            let mut windows: Vec<WindowId> = row.tiles.iter().map(|tile| tile.window).collect();
            windows.sort();
            windows
        })
        .collect();
    assert_eq!(rows, vec![
        vec![w(1), w(2)],
        vec![w(3), w(4), w(5)],
        vec![],
        vec![w(6)]
    ]);
    assert!(
        scene.rows[2].outline.is_some(),
        "the empty workspace keeps its row"
    );
    assert_eq!(selected(&reactor), (1, Some(w(3))));
    let tops: Vec<f64> = scene.rows.iter().map(|row| row.band.min_y()).collect();
    assert!(
        tops.windows(2).all(|pair| pair[0] < pair[1]),
        "top to bottom: {tops:?}"
    );
}

/// Each row is aligned on the window a switch to its workspace focuses: switched to inside the
/// overview, each workspace focuses the window its row was aligned on, w1 on the first, focused
/// there last though w2 came after it.
#[test]
fn the_window_a_row_aligns_on_is_the_one_a_switch_to_it_focuses() {
    let (mut apps, mut reactor) = desk();
    open(&mut apps, &mut reactor);
    let scene = reactor.overview_scene().unwrap().clone();
    assert_eq!(scene.rows[1].aligned_on, Some(w(3)), "the shown row, on focus");
    assert_eq!(scene.rows[0].aligned_on, Some(w(1)));
    assert_eq!(scene.rows[3].aligned_on, Some(w(6)));
    for row in [0, 3] {
        press(&mut apps, &mut reactor, LayoutCommand::SwitchToWorkspace(row));
        assert_eq!(
            reactor.layout().focused_window(),
            scene.rows[row].aligned_on,
            "workspace {row}"
        );
        assert_eq!(selected(&reactor), (row, scene.rows[row].aligned_on));
    }
}

/// A focus press inside the overview moves the real focus, and the scene's selection and its row's
/// alignment with it.
#[test]
fn a_focus_press_inside_the_overview_moves_real_focus_and_the_selection() {
    let (mut apps, mut reactor) = desk();
    open(&mut apps, &mut reactor);
    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocus(Direction::Right),
    );
    assert_eq!(reactor.layout().focused_window(), Some(w(4)));
    assert_eq!(selected(&reactor), (1, Some(w(4))));
    let scene = reactor.overview_scene().unwrap();
    assert_eq!(scene.rows[1].aligned_on, Some(w(4)));
    let tile = scene.rows[1].tile(w(4)).unwrap();
    assert!((tile.frame.center().x - scene.axis).abs() < 1e-6, "{tile:?}");
}

/// Nothing flies while the overview is open: a press that would pan the strip sends the flight
/// engine nothing, the real windows are placed at once where the flight would have placed them,
/// and the scene is told the press as the flight named it.
#[test]
fn nothing_flies_while_the_overview_is_open_and_frames_are_placed() {
    let (mut apps, mut reactor) = desk();
    reactor.settings_mut().animate = true;
    let mut rx = reactor.connect_test_transitions();
    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocus(Direction::Right),
    );
    let flights = strip_movements(&mut rx);
    let Some(flight) = flights.iter().find(|flight| flight.from_offset.x.abs() >= 1.0) else {
        panic!(
            "outside the overview the press pans the strip: {} flights",
            flights.len()
        );
    };
    let Cue::Focus { .. } = flight.cue else {
        panic!("{:?}", flight.cue)
    };
    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocus(Direction::Left),
    );
    flew(&mut rx);

    open(&mut apps, &mut reactor);
    reactor.handle_test_batch(vec![layout(LayoutCommand::MoveFocus(Direction::Right))]);
    assert!(!flew(&mut rx), "nothing flies while it is open");
    let written: Vec<(WindowId, CGRect)> = apps
        .requests()
        .into_iter()
        .filter_map(|request| match request {
            Request::SetWindowFrame { window, frame, .. } => Some((window, frame)),
            _ => None,
        })
        .collect();
    for (window, frame) in &flight.final_frames {
        assert!(
            written.contains(&(*window, frame.to_cg())),
            "{window:?} placed at {frame:?}: {written:?}"
        );
    }
    assert_eq!(
        reactor.overview_press(),
        Some(Press::Moved(Cue::Focus { from: w(3), to: w(4) }))
    );
}

/// P and ; inside the overview switch the real workspace: the selection steps onto the empty row,
/// then onto the last one's window, each a switch the scene is told of.
#[test]
fn a_workspace_press_inside_the_overview_switches_the_real_workspace() {
    let (mut apps, mut reactor) = desk();
    reactor.settings_mut().animate = true;
    let mut rx = reactor.connect_test_transitions();
    open(&mut apps, &mut reactor);
    press(&mut apps, &mut reactor, LayoutCommand::NextWorkspace(None));
    let workspaces = reactor.test_workspace_ids(space());
    assert_eq!(reactor.test_active_workspace(space()), Some(workspaces[2]));
    assert_eq!(selected(&reactor), (2, None));
    assert_eq!(reactor.overview_press(), Some(Press::Moved(Cue::Switch)));

    press(&mut apps, &mut reactor, LayoutCommand::NextWorkspace(None));
    assert_eq!(reactor.test_active_workspace(space()), Some(workspaces[3]));
    assert_eq!(reactor.layout().focused_window(), Some(w(6)));
    assert_eq!(selected(&reactor), (3, Some(w(6))));
    assert!(!flew(&mut rx));
}

/// Shift and ; inside the overview moves the window to the workspace below and follows it: its
/// thumb leaves its row for that one, where it is selected.
#[test]
fn a_move_to_another_workspace_inside_the_overview_moves_its_thumb_to_that_row() {
    let (mut apps, mut reactor) = desk();
    open(&mut apps, &mut reactor);
    press(&mut apps, &mut reactor, LayoutCommand::MoveWindowToWorkspace {
        workspace: WorkspaceSelector::Relative(rini_ipc::protocol::RelativeWorkspace::Next),
        follow: true,
        window_id: None,
    });
    let scene = reactor.overview_scene().unwrap();
    assert_eq!(scene.find(w(3)).map(|(row, _)| row), Some(2));
    assert!(scene.rows[1].tile(w(3)).is_none());
    assert!(scene.rows[2].outline.is_none(), "no longer empty");
    assert_eq!(selected(&reactor), (2, Some(w(3))));
}

/// The overview closes on the window its last press focused, on that window's workspace, and the
/// close itself focuses and switches nothing more.
#[test]
fn the_overview_closes_on_the_window_its_last_press_focused() {
    let (mut apps, mut reactor) = desk();
    open(&mut apps, &mut reactor);
    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocus(Direction::Right),
    );
    press(&mut apps, &mut reactor, LayoutCommand::PrevWorkspace(None));
    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocus(Direction::Right),
    );
    let last = selected(&reactor);
    assert_eq!(last, (0, Some(w(2))));

    batch(&mut apps, &mut reactor, vec![overview(OverviewSignal::Close)]);
    assert!(!reactor.overview_is_open());
    assert_eq!(reactor.layout().focused_window(), Some(w(2)));
    let workspaces = reactor.test_workspace_ids(space());
    assert_eq!(reactor.test_active_workspace(space()), Some(workspaces[0]));
}

/// Two displays side by side, w1 on the left one's space and focused, w2 on the right one's.
fn two_displays() -> (Apps, Reactor, SpaceId) {
    let mut reactor = test_reactor();
    let mut apps = Apps::new();
    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let right = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let (space1, space2) = (SpaceId::new(1), SpaceId::new(2));
    reactor.handle_event(space_state_event(vec![left, right], vec![
        Some(space1),
        Some(space2),
    ]));
    let mut windows = make_windows(2);
    windows[1].frame.origin.x += 1440.;
    apps.make_app_and_settle(&mut reactor, 1, windows);
    reactor.send_layout_event(LayoutEvent::WindowFocused(space1, w(1)));
    reactor.set_test_focus(w(1));
    (apps, reactor, space2)
}

/// A press that takes focus onto another display closes the overview there and then, and tells the
/// tap its session is over, so it stops holding keys for it.
#[test]
fn focus_leaving_the_display_closes_the_overview() {
    let (mut apps, mut reactor, _) = two_displays();
    let mut tap = reactor.connect_test_event_tap();
    open(&mut apps, &mut reactor);
    the_tap_was_sent(&mut tap);

    let wsid = reactor.test_window_server_id(w(2));
    batch(&mut apps, &mut reactor, vec![Event::Command(Command::Reactor(
        ReactorCommand::FocusWindow {
            window_id: w(2).into(),
            window_server_id: Some(wsid.as_u32()),
        },
    ))]);
    assert_eq!(reactor.layout().focused_window(), Some(w(2)));
    assert!(!reactor.overview_is_open());
    assert!(
        the_tap_was_sent(&mut tap)
            .iter()
            .any(|request| matches!(request, Tap::EndSwitchSession)),
        "the tap's session is ended"
    );
}

/// A flight over another display while the overview is open is held as the overview's are, its
/// windows placed, but it is no press of the overview's: its scene is not told of it.
#[test]
fn a_flight_over_another_display_is_held_but_is_no_press_of_the_overviews() {
    let (mut apps, mut reactor, other) = two_displays();
    let mut rx = reactor.connect_test_transitions();
    open(&mut apps, &mut reactor);
    let push = |reactor: &Reactor| Engine::Bounce {
        windows: Vec::new(),
        overshoot: OverlayVector::new(-EDGE_BOUNCE_OVERSHOOT, 0.0),
        final_frames: Vec::new(),
        focus: None,
        duration: std::time::Duration::ZERO,
        look: reactor.settings().look,
        cue: Cue::Switch,
        cause: *reactor.cause(),
    };
    let flight = push(&reactor);
    assert!(reactor.send_flight(other, flight));
    batch(&mut apps, &mut reactor, Vec::new());
    assert!(!flew(&mut rx), "held");
    assert_eq!(reactor.overview_press(), None);

    let flight = push(&reactor);
    reactor.send_flight(space(), flight);
    batch(&mut apps, &mut reactor, Vec::new());
    assert!(matches!(reactor.overview_press(), Some(Press::Pushed { .. })));
}

/// The overview does not open where the focused display shows a space rini does not manage, and
/// the tap is told, so it does not hold keys for an overview that is not there.
#[test]
fn the_overview_does_not_open_on_a_space_rini_does_not_manage() {
    let (mut apps, mut reactor) = desk();
    reactor.handle_event(Event::Command(Command::Reactor(
        ReactorCommand::ToggleSpaceActivated,
    )));
    apps.simulate_until_quiet(&mut reactor);
    let mut tap = reactor.connect_test_event_tap();
    batch(&mut apps, &mut reactor, vec![overview(OverviewSignal::Open)]);
    assert!(!reactor.overview_is_open());
    assert!(
        the_tap_was_sent(&mut tap)
            .iter()
            .any(|request| matches!(request, Tap::EndSwitchSession))
    );
}

/// Nothing warps the pointer while the overview is open, not the warp a focus press asks for nor
/// a switch's; the last one asked for is carried out as it closes, at the window focus is on.
#[test]
fn no_warp_inside_the_overview_and_the_last_one_at_close() {
    let (mut apps, mut reactor) = desk();
    reactor.settings_mut().warp_mouse_to_focus = true;
    let mut raises = reactor.connect_test_raises();
    let mut tap = reactor.connect_test_event_tap();
    let mut warped = |tap: &mut channels::Receiver<Tap>| {
        let mut points: Vec<CGPoint> = the_tap_was_sent(tap)
            .into_iter()
            .filter_map(|request| match request {
                Tap::Warp(point) => Some(point),
                _ => None,
            })
            .collect();
        while let Ok((_, event)) = raises.try_recv() {
            if let crate::windows::domain::raise::Event::RaiseRequest(request) = event
                && let Some((_, Some(point))) = request.focus_window
            {
                points.push(point);
            }
        }
        points
    };
    press(&mut apps, &mut reactor, LayoutCommand::SwitchToWorkspace(3));
    assert!(
        !warped(&mut tap).is_empty(),
        "outside it, a switch warps the pointer"
    );
    press(&mut apps, &mut reactor, LayoutCommand::SwitchToWorkspace(1));
    warped(&mut tap);

    open(&mut apps, &mut reactor);
    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocus(Direction::Right),
    );
    press(&mut apps, &mut reactor, LayoutCommand::NextWorkspace(None));
    press(&mut apps, &mut reactor, LayoutCommand::NextWorkspace(None));
    assert_eq!(warped(&mut tap), Vec::new());

    batch(&mut apps, &mut reactor, vec![overview(OverviewSignal::Close)]);
    assert_eq!(warped(&mut tap), vec![reactor.test_frame(w(6)).mid()]);
}

/// The pointer entering a window moves no focus and raises nothing while the overview is open,
/// and nothing asks the tap to stop following it: a new event mask replaces the tap, which closes
/// the overview. Once it closes, focus follows the mouse again.
#[test]
fn focus_does_not_follow_the_mouse_inside_the_overview() {
    let (mut apps, mut reactor) = desk();
    reactor.settings_mut().focus_follows_mouse = true;
    let mut raises = reactor.connect_test_raises();
    let mut tap = reactor.connect_test_event_tap();
    open(&mut apps, &mut reactor);
    let under = reactor.test_window_server_id(w(4));
    batch(&mut apps, &mut reactor, vec![Event::MouseMoved(under)]);
    assert_eq!(reactor.layout().focused_window(), Some(w(3)));
    assert!(raises.try_recv().is_err(), "nothing raised");
    let sent = the_tap_was_sent(&mut tap);
    assert!(
        !sent
            .iter()
            .any(|request| matches!(request, Tap::SetFocusFollowsMouseEnabled(_))),
        "{sent:?}"
    );

    batch(&mut apps, &mut reactor, vec![overview(OverviewSignal::Close)]);
    batch(&mut apps, &mut reactor, vec![Event::MouseMoved(under)]);
    assert_eq!(reactor.layout().focused_window(), Some(w(4)));
}

/// A press against the strip's end inside the overview flies no bounce; the scene is told it as a
/// push, the strip giving the way it was pushed.
#[test]
fn a_press_at_a_strip_end_inside_the_overview_is_cued_as_a_push() {
    let (mut apps, mut reactor) = desk();
    reactor.settings_mut().animate = true;
    let mut rx = reactor.connect_test_transitions();
    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocusToEnd(rini_ipc::protocol::StripEnd::Last),
    );
    let last = reactor.layout().focused_window().unwrap();
    flew(&mut rx);
    open(&mut apps, &mut reactor);
    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocus(Direction::Right),
    );
    assert!(!flew(&mut rx));
    assert_eq!(reactor.layout().focused_window(), Some(last));
    assert_eq!(
        reactor.overview_press(),
        Some(Press::Pushed {
            press: Cue::Focus { from: last, to: last },
            overshoot: OverlayVector::new(-EDGE_BOUNCE_OVERSHOOT, 0.0),
        })
    );
}

/// A flight sent before the overview opened that lands inside it puts back no window a press
/// inside it has placed since; the windows only that flight moved it still places.
#[test]
fn a_flight_landing_inside_the_overview_puts_back_no_window_a_press_there_placed() {
    let (mut apps, mut reactor) = desk();
    reactor.settings_mut().animate = true;
    let mut rx = reactor.connect_test_transitions();
    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocus(Direction::Right),
    );
    let landing: Vec<(WindowId, CGRect)> = strip_movements(&mut rx)
        .into_iter()
        .flat_map(|flight| flight.final_frames)
        .map(|(window, frame)| (window, frame.to_cg()))
        .collect();
    assert!(!landing.is_empty());

    open(&mut apps, &mut reactor);
    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocus(Direction::Right),
    );
    let pressed: Vec<WindowId> = landing
        .iter()
        .map(|(window, _)| *window)
        .filter(|window| {
            reactor.test_frame(*window)
                != landing.iter().find(|(w, _)| w == window).map(|(_, f)| *f).unwrap()
        })
        .collect();
    assert!(
        !pressed.is_empty(),
        "the press inside moved some window the flight moved"
    );

    batch(&mut apps, &mut reactor, vec![Event::ApplyOverlayFrames(
        landing.clone(),
    )]);
    for window in &pressed {
        let put_back = landing.iter().find(|(w, _)| w == window).unwrap().1;
        assert_ne!(
            reactor.test_frame(*window),
            put_back,
            "{window:?} is not put back"
        );
    }

    batch(&mut apps, &mut reactor, vec![overview(OverviewSignal::Close)]);
    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocus(Direction::Left),
    );
    let next: Vec<(WindowId, CGRect)> = strip_movements(&mut rx)
        .into_iter()
        .flat_map(|flight| flight.final_frames)
        .map(|(window, frame)| (window, frame.to_cg()))
        .filter(|(window, _)| pressed.contains(window))
        .collect();
    assert!(!next.is_empty(), "the next flight moves them again");
    let elsewhere: Vec<(WindowId, CGRect)> = next
        .iter()
        .map(|&(window, frame)| (window, CGRect::new(frame.origin, CGSize::new(1., 1.))))
        .collect();
    for (window, frame) in &elsewhere {
        reactor.set_test_frame(*window, *frame);
    }
    batch(&mut apps, &mut reactor, vec![Event::ApplyOverlayFrames(
        next.clone(),
    )]);
    for (window, frame) in next {
        assert_eq!(
            reactor.test_frame(window),
            frame,
            "a flight sent since lands {window:?}"
        );
    }
}

/// Opening warms every workspace of the display, so the thumbs of the rows not shown have
/// pictures.
#[test]
fn opening_warms_every_workspace_of_the_display() {
    use crate::pictures::domain::schedule::Priority;
    let (mut apps, mut reactor) = desk();
    let mut pictures = reactor.connect_test_pictures();
    open(&mut apps, &mut reactor);
    let warmed = asked_at(&pictures_asked(&mut pictures), Priority::Warm);
    for window in (1..=6).map(w) {
        assert!(warmed.contains(&window), "{window:?}: {warmed:?}");
    }
}

/// Released as soon as a press inside it went, the overview leaves nothing held: the press's
/// windows are placed, its warp carried out once, the next press flies and the mouse moves focus.
#[test]
fn nothing_is_held_once_the_overview_closes() {
    let (mut apps, mut reactor) = desk();
    reactor.settings_mut().animate = true;
    reactor.settings_mut().warp_mouse_to_focus = true;
    reactor.settings_mut().focus_follows_mouse = true;
    let mut rx = reactor.connect_test_transitions();
    let mut tap = reactor.connect_test_event_tap();
    batch(&mut apps, &mut reactor, vec![
        overview(OverviewSignal::Open),
        layout(LayoutCommand::MoveFocus(Direction::Right)),
        overview(OverviewSignal::Close),
    ]);
    assert!(!reactor.overview_is_open());
    assert!(!flew(&mut rx));
    let warped = the_tap_was_sent(&mut tap)
        .into_iter()
        .filter(|request| matches!(request, Tap::Warp(_)))
        .count();
    assert_eq!(warped, 1);

    press(
        &mut apps,
        &mut reactor,
        LayoutCommand::MoveFocus(Direction::Right),
    );
    assert!(flew(&mut rx), "the next press flies");
    let under = reactor.test_window_server_id(w(3));
    batch(&mut apps, &mut reactor, vec![Event::MouseMoved(under)]);
    assert_eq!(reactor.layout().focused_window(), Some(w(3)));
}
