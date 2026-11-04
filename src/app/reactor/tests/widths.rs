//! What a width the user set survives: the display sleeping and waking, the window server taking a
//! window away and giving it back, and a drag of its edge followed by either.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::WindowServerId;
use test_log::test;

use super::fixtures::*;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::workspaces::{LayoutCommand, LayoutEvent};

/// The built-in display's usable frame on 2026-11-04 (err.log, the 15:08:52 display-off pass).
const SCREEN: CGRect = CGRect {
    origin: CGPoint { x: 0., y: 32. },
    size: CGSize { width: 1728., height: 1085. },
};
const PID: rini_core::ids::pid_t = 1;

fn space() -> SpaceId { SpaceId::new(1) }

fn window() -> WindowId { WindowId::new(PID, 1) }

fn sibling() -> WindowId { WindowId::new(PID, 2) }

fn server_id(window: WindowId) -> WindowServerId {
    WindowServerId::new((PID as u32) * 10_000 + window.idx.get())
}

/// One app's two windows tiled on the built-in display, `window()` focused.
fn two_windows() -> (Apps, Reactor) {
    let (mut apps, mut reactor) = test_context();
    apps.make_app_and_settle_on_screen(&mut reactor, SCREEN, space(), PID, make_windows(2));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space(), window()));
    apps.simulate_until_quiet(&mut reactor);
    (apps, reactor)
}

fn width_of(reactor: &mut Reactor, window: WindowId) -> f64 {
    laid_out_frame(reactor, space(), SCREEN, window)
        .expect("the window is laid out")
        .size
        .width
}

fn set_manageable(reactor: &mut Reactor, window: WindowId, manageable: bool) {
    reactor
        .windows_mut()
        .window_mut(window)
        .expect("the window is tracked")
        .is_manageable = manageable;
}

/// The authoritative pass that finds each window where the window server has it.
fn reconcile(reactor: &mut Reactor) {
    for window in [window(), sibling()] {
        reactor.windows_mut().set_window_server_space(server_id(window), Some(space()));
    }
    let _ = reactor.reconcile_windows_with_authoritative_spaces();
}

/// A window made full width keeps it across the display turning off and waking: as the display
/// turns off a window can be briefly not manageable and the authoritative pass takes it out of
/// every layout, then the system sleeps, and the snapshot after the wake and the refresh it
/// releases put it back (err.log:1216, the window laid full at the display-off pass, and :1259, the
/// user's full-width press after the wake).
#[test]
fn a_full_width_window_keeps_its_width_across_the_display_sleeping_and_waking() {
    let (mut apps, mut reactor) = two_windows();
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreenWithinGaps);
    let full = width_of(&mut reactor, window());
    assert!(full > SCREEN.size.width * 0.9, "full width: {full}");

    set_manageable(&mut reactor, window(), false);
    reconcile(&mut reactor);
    assert_eq!(
        laid_out_frame(&mut reactor, space(), SCREEN, window()),
        None,
        "the display-off pass takes the window out"
    );

    reactor.handle_event(Event::SystemWillSleep);
    reactor.handle_event(Event::SystemWoke);
    set_manageable(&mut reactor, window(), true);
    let mut woken = forwarded_space_state(make_screen_snapshots(vec![SCREEN], vec![Some(space())]));
    woken.releases_lifecycle_refresh_quarantine = true;
    for window in [window(), sibling()] {
        woken.active_window_spaces.insert(server_id(window), space());
    }
    reactor.handle_event(Event::SpaceStateChanged(woken));
    apps.simulate_until_quiet(&mut reactor);
    assert_eq!(width_of(&mut reactor, window()), full);
}

/// A window the window server leaves out of an authoritative snapshot is taken out of the layout,
/// and keeps its width for when the window server reports it again (err.log:646, the re-add
/// `from=None` 3s before Zen's full-width press).
#[test]
fn a_window_the_window_server_removes_and_re_adds_keeps_its_width() {
    let (mut apps, mut reactor) = two_windows();
    reactor.handle_test_layout_command(LayoutCommand::ToggleFullscreenWithinGaps);
    let full = width_of(&mut reactor, window());

    reactor.refresh_windows_after_mission_control_with_active_windows(vec![(
        server_id(sibling()),
        Some(space()),
    )]);
    apps.simulate_until_quiet(&mut reactor);
    assert_eq!(
        laid_out_frame(&mut reactor, space(), SCREEN, window()),
        None,
        "the snapshot that leaves the window out takes it out"
    );

    reactor.refresh_windows_after_mission_control_with_active_windows(vec![
        (server_id(window()), Some(space())),
        (server_id(sibling()), Some(space())),
    ]);
    apps.simulate_until_quiet(&mut reactor);
    reconcile(&mut reactor);
    assert_eq!(width_of(&mut reactor, window()), full);
}

/// Who widens `window` by its right edge.
#[derive(Clone, Copy)]
enum By {
    /// The user, dragging the edge and letting go.
    Drag,
    /// Its app, with no button held.
    App,
}

/// `window`'s right edge moved `by` points wider by `who`, and what that writes carried out.
fn widen(apps: &mut Apps, reactor: &mut Reactor, window: WindowId, by: f64, who: By) {
    let app_window = apps.windows.get_mut(&window).expect("test window");
    let start = app_window.frame;
    let wider = CGRect::new(
        start.origin,
        CGSize::new(start.size.width + by, start.size.height),
    );
    app_window.frame = wider;
    let last_seen = Some(app_window.last_seen_txid);
    let mouse = match who {
        By::Drag => MouseState::Down,
        By::App => MouseState::Up,
    };
    reactor.handle_event(Event::WindowFrameChanged {
        window,
        frame: wider,
        last_seen,
        requested: Requested(false),
        mouse_state: Some(mouse),
    });
    if matches!(who, By::Drag) {
        reactor.handle_event(Event::MouseUp);
    }
    apps.simulate_until_quiet(reactor);
}

fn recorded_width(reactor: &Reactor, window: WindowId) -> Option<crate::layout::ColumnWidth> {
    reactor
        .display_affinity()
        .window_width(&rini_core::ids::DisplayUuid::new("test-display-0"), window)
}

/// The window server moving `window` to another space and back.
fn moved_away_and_back(reactor: &mut Reactor, window: WindowId) {
    let elsewhere = SpaceId::new(2);
    let _ = reactor.test_workspace_ids(elsewhere);
    reactor.reassign_window_to_authoritative_space(window, elsewhere);
    reconcile(reactor);
}

/// A width set by dragging an edge is recorded as the drag ends, as a width command's is, so the
/// window reads it back wherever it is laid out again: a drag once reached only the layout, never
/// the record a re-added window reads.
#[test]
fn a_width_set_by_a_drag_is_recorded_and_survives_the_window_being_taken_away_and_given_back() {
    let (mut apps, mut reactor) = two_windows();
    let before = width_of(&mut reactor, window());
    widen(&mut apps, &mut reactor, window(), 200., By::Drag);
    let dragged = width_of(&mut reactor, window());
    assert!(dragged > before + 150., "{before} -> {dragged}");
    assert!(
        recorded_width(&reactor, window()).is_some(),
        "recorded at the drag's end"
    );

    set_manageable(&mut reactor, window(), false);
    reconcile(&mut reactor);
    set_manageable(&mut reactor, window(), true);
    reconcile(&mut reactor);
    assert_eq!(width_of(&mut reactor, window()), dragged);
}

/// A width only the layout holds, here one the app gave its own window, is recorded before the
/// window server moves the window away, so it comes back at that width.
#[test]
fn a_width_only_the_layout_holds_survives_the_window_server_moving_the_window_away_and_back() {
    let (mut apps, mut reactor) = two_windows();
    let before = width_of(&mut reactor, window());
    widen(&mut apps, &mut reactor, window(), 200., By::App);
    let widened = width_of(&mut reactor, window());
    assert!(widened > before + 150., "{before} -> {widened}");
    assert_eq!(
        recorded_width(&reactor, window()),
        None,
        "only the layout holds it"
    );

    moved_away_and_back(&mut reactor, window());
    assert_eq!(width_of(&mut reactor, window()), widened);
}

/// A window of `PID`'s app, with the bundle id its app has and its standard windows carry.
fn terminal_window(idx: u32) -> crate::windows::domain::info::WindowInfo {
    let frame = CGRect::new(
        CGPoint::new(100.0 * f64::from(idx), 100.0),
        CGSize::new(50.0, 50.0),
    );
    make_window_info(
        frame,
        Some(WindowServerId::new(idx)),
        &format!("Window{idx}"),
        Some(TERMINAL),
    )
}

/// The bundle id `test_context`'s apps launch with.
const TERMINAL: &str = "com.testapp1";

/// The app's one window on the built-in display, focused, with its bundle id.
fn one_terminal() -> (Apps, Reactor) {
    let (mut apps, mut reactor) = test_context();
    apps.make_app_and_settle_on_screen(&mut reactor, SCREEN, space(), PID, vec![terminal_window(
        1,
    )]);
    reactor.send_layout_event(LayoutEvent::WindowFocused(space(), window()));
    apps.simulate_until_quiet(&mut reactor);
    (apps, reactor)
}

/// The app creates its next window, as Cmd+N does, and the width that window is laid at.
fn next_window_s_width(apps: &mut Apps, reactor: &mut Reactor) -> f64 {
    reactor.handle_event(Event::WindowCreated {
        window: sibling(),
        info: terminal_window(2),
        server_info: None,
        mouse_state: Some(MouseState::Up),
    });
    apps.simulate_until_quiet(reactor);
    width_of(reactor, sibling())
}

/// Cmd+N in a running app, end to end: the screens' shapes reach the engine, the preset cycle
/// records the app's width on the display's shape, and the window the app creates next opens at it
/// (`specs/displays.md`, Kai's 2026-11-04 report, failure 1).
#[test]
fn a_new_window_of_a_running_app_opens_at_the_width_its_app_last_had() {
    let (mut apps, mut reactor) = one_terminal();
    let default = width_of(&mut reactor, window());
    reactor.handle_test_layout_command(LayoutCommand::CyclePresetColumnWidth);
    apps.simulate_until_quiet(&mut reactor);
    let given = width_of(&mut reactor, window());
    assert_ne!(given, default, "the cycle gave the window a width");
    assert_eq!(next_window_s_width(&mut apps, &mut reactor), given);
}

/// A width set by dragging an edge is the app's last width too, as the drag ends.
#[test]
fn a_new_window_of_a_running_app_opens_at_the_width_a_drag_gave_its_app() {
    let (mut apps, mut reactor) = one_terminal();
    let before = width_of(&mut reactor, window());
    widen(&mut apps, &mut reactor, window(), 200., By::Drag);
    let dragged = width_of(&mut reactor, window());
    assert!(dragged > before + 150., "{before} -> {dragged}");
    assert_eq!(next_window_s_width(&mut apps, &mut reactor), dragged);
}

/// An edit to `default-column-width` read from the config drops the application widths made under
/// the old value, so none of them is written back to the file.
#[test]
fn an_edited_default_column_width_read_from_the_config_drops_the_app_widths_made_under_the_old() {
    let (mut apps, mut reactor) = one_terminal();
    reactor.handle_test_layout_command(LayoutCommand::CyclePresetColumnWidth);
    apps.simulate_until_quiet(&mut reactor);
    assert!(!reactor.rini_state().display_memory.app_widths.is_empty());

    let mut unchanged = reactor.config().clone();
    reactor.handle_event(Event::ConfigUpdated(Box::new(unchanged.clone())));
    assert!(
        !reactor.rini_state().display_memory.app_widths.is_empty(),
        "the same default"
    );

    unchanged.settings.layout.scrolling.column_width_ratio -= 0.1;
    reactor.handle_event(Event::ConfigUpdated(Box::new(unchanged)));
    assert!(reactor.rini_state().display_memory.app_widths.is_empty());
}
