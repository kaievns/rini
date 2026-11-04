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

/// A `default-column-width` read for a while, as the reference config's when the user's fails to
/// parse or a `config set` trial's, costs no application width, though a width command writes the
/// app's width under it: once the user's default is back, the next window opens at the width its
/// app last had under it (`specs/displays.md`).
#[test]
fn an_app_s_width_is_read_again_once_its_default_column_width_is_back() {
    let (mut apps, mut reactor) = one_terminal();
    reactor.handle_test_layout_command(LayoutCommand::CyclePresetColumnWidth);
    apps.simulate_until_quiet(&mut reactor);
    let given = width_of(&mut reactor, window());

    let users = reactor.config().clone();
    let mut another = users.clone();
    another.settings.layout.scrolling.column_width_ratio += 0.2;
    reactor.handle_event(Event::ConfigUpdated(Box::new(another)));
    apps.simulate_until_quiet(&mut reactor);
    reactor.handle_test_layout_command(LayoutCommand::CyclePresetColumnWidth);
    apps.simulate_until_quiet(&mut reactor);
    assert_ne!(
        width_of(&mut reactor, window()),
        given,
        "the trial's width command gave another width"
    );
    reactor.handle_event(Event::ConfigUpdated(Box::new(users)));
    apps.simulate_until_quiet(&mut reactor);
    assert_eq!(next_window_s_width(&mut apps, &mut reactor), given);
}

/// The app that reports first after the restart, as Slack did at 21:28:12 on 2026-11-04.
const FIRST_TO_REPORT: rini_core::ids::pid_t = 2;

/// `PID`'s two windows tiled and saved, `on` with a full width on record that its column does not
/// carry, as a width it was given on another display is, and rini started again on that file with
/// both still alive and their app not yet reported.
fn restarted_with_a_width_on_record(on: WindowId) -> Reactor {
    let (_, mut saved) = two_windows();
    let display = rini_core::ids::DisplayUuid::new("test-display-0");
    saved.display_affinity_mut().set_window_width(
        &display,
        on,
        crate::layout::ColumnWidth::FullWidth,
    );
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("layout.ron");
    saved.save_layout_before_exit(path.clone()).expect("the file is written");

    let (mut layout, memory) =
        crate::workspaces::LayoutEngine::load_for_startup_restore(path, |_, _| true, |_| true)
            .expect("the file loads");
    layout.finish_loading(
        &crate::app::config::VirtualWorkspaceSettings::default(),
        &crate::app::config::LayoutSettings::default(),
    );
    assert_eq!(memory.affinity.window_home(on), Some(&display));
    Reactor::new_for_test_remembering(layout, memory)
}

/// After rini restarts, the topology settles and another app reports before a live window's own app
/// does: that window is in no store yet, and its home and width on record are kept for when its app
/// reports it, where the settled-topology pass erased them as gone (the 7 `why=Gone` lines of
/// 2026-11-04 21:28:12 in the service log). Once its app reports it, a removal and re-add reads the
/// record back.
#[test]
fn a_live_window_s_records_survive_a_settled_topology_before_its_app_reports_it() {
    let mut reactor = restarted_with_a_width_on_record(window());
    let mut apps = Apps::new();
    reactor.handle_event(space_state_event(vec![SCREEN], vec![Some(space())]));
    apps.make_app_and_settle(&mut reactor, FIRST_TO_REPORT, vec![make_window(3)]);
    assert_eq!(
        recorded_width(&reactor, window()),
        Some(crate::layout::ColumnWidth::FullWidth)
    );
    assert!(reactor.display_affinity().window_home(window()).is_some());

    apps.make_app_and_settle(&mut reactor, PID, make_windows(2));
    let reported = width_of(&mut reactor, window());
    let others = [
        (server_id(sibling()), Some(space())),
        (WindowServerId::new(3), Some(space())),
    ];
    reactor.refresh_windows_after_mission_control_with_active_windows(others.to_vec());
    apps.simulate_until_quiet(&mut reactor);
    assert_eq!(
        laid_out_frame(&mut reactor, space(), SCREEN, window()),
        None,
        "the snapshot that leaves the window out takes it out"
    );
    let mut all = others.to_vec();
    all.push((server_id(window()), Some(space())));
    reactor.refresh_windows_after_mission_control_with_active_windows(all);
    apps.simulate_until_quiet(&mut reactor);
    reconcile(&mut reactor);
    let readded = width_of(&mut reactor, window());
    assert!(
        readded > SCREEN.size.width * 0.9,
        "laid at the full width on record: {reported} as reported, {readded} re-added"
    );
}

/// A window the window server's list of every window holds, as `pid`'s window `idx`.
fn listed(pid: rini_core::ids::pid_t, idx: u32) -> crate::windows::platform::window_server::Listed {
    crate::windows::platform::window_server::Listed {
        id: WindowServerId::new(idx),
        level: 0,
        pid,
        bounds: SCREEN,
        title: None,
    }
}

/// `restarted_with_a_width_on_record`'s app reporting one window, not `sibling()`, the window
/// server listing `every` window, and the width `sibling()` keeps on record after.
fn width_on_record_after_its_app_reports_without_it(
    every: Vec<crate::windows::platform::window_server::Listed>,
) -> Option<crate::layout::ColumnWidth> {
    let mut reactor = restarted_with_a_width_on_record(sibling());
    reactor.window_server().set_every_window(every);
    let mut apps = Apps::new();
    reactor.handle_event(space_state_event(vec![SCREEN], vec![Some(space())]));
    apps.make_app_and_settle(&mut reactor, PID, make_windows(1));
    recorded_width(&reactor, sibling())
}

/// A window from the file that its app reports without, and that the window server lacks now,
/// closed after rini started and before its app reported: its records go then. One the window
/// server still has, on another space or minimized, keeps them, as one does when the window server
/// gives no list.
#[test]
fn a_window_from_the_file_its_app_reports_without_is_forgotten_once_the_window_server_lacks_it() {
    let (window_idx, sibling_idx) = (window().idx.get(), sibling().idx.get());
    assert_eq!(
        width_on_record_after_its_app_reports_without_it(vec![listed(PID, window_idx)]),
        None
    );
    let full = Some(crate::layout::ColumnWidth::FullWidth);
    assert_eq!(
        width_on_record_after_its_app_reports_without_it(vec![
            listed(PID, window_idx),
            listed(PID, sibling_idx),
        ]),
        full,
        "the window server still has it"
    );
    assert_eq!(
        width_on_record_after_its_app_reports_without_it(Vec::new()),
        full,
        "no list is no answer"
    );
}
