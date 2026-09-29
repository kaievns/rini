//! The bar: what each display's bar is sent after a batch of events, and what its clicks do.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::WindowServerId;
use test_log::test;

use super::fixtures::*;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::bar::domain::model::{Action, BarModel, FocusLabel, Row};
use crate::bar::platform::actor::{Event as BarEvent, Receiver};
use crate::windows::platform::window_server::set_space_window_list_for_space_override;
use crate::workspaces::LayoutCommand;

const LEFT: &str = "test-display-0";
const RIGHT: &str = "test-display-1";

const GHOSTTY: i32 = 1;
const ZEN: i32 = 2;

/// Two displays, four workspaces. The left shows workspace 0 with Ghostty and Zen on it. The right
/// shows workspace 0 with a Zen window, and holds a Ghostty window on workspace 2 that it does not
/// show. Nothing is focused.
struct Desk {
    reactor: Reactor,
    bar: Receiver,
    left: SpaceId,
    right: SpaceId,
}

impl Drop for Desk {
    fn drop(&mut self) {
        for space in [self.left, self.right] {
            set_space_window_list_for_space_override(space.get(), None);
        }
    }
}

fn left_ghostty() -> WindowId {
    WindowId::new(GHOSTTY, 1)
}

fn left_zen() -> WindowId {
    WindowId::new(ZEN, 1)
}

fn right_zen() -> WindowId {
    WindowId::new(ZEN, 2)
}

fn right_ghostty() -> WindowId {
    WindowId::new(GHOSTTY, 2)
}

fn desk() -> Desk {
    let mut reactor = test_reactor();
    let bar = reactor.connect_test_bar();
    let left = SpaceId::new(1);
    let right = SpaceId::new(2);
    let left_frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let right_frame = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    set_space_membership(&[(left, &[901, 902]), (right, &[903, 904])]);
    reactor.handle_test_batch(vec![space_state_event(
        vec![left_frame, right_frame],
        vec![Some(left), Some(right)],
    )]);
    reactor.add_test_app_with_info(GHOSTTY, "com.mitchellh.ghostty", "Ghostty");
    reactor.add_test_app_with_info(ZEN, "app.zen-browser.zen", "Zen");
    for (window, wsid, space, frame, workspace) in [
        (left_ghostty(), 901, left, left_frame, 0),
        (left_zen(), 902, left, left_frame, 0),
        (right_zen(), 903, right, right_frame, 0),
        (right_ghostty(), 904, right, right_frame, 2),
    ] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), frame);
        let workspace = reactor.test_workspace(space, workspace);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspace));
    }
    let mut desk = Desk { reactor, bar, left, right };
    desk.reactor.handle_test_batch(Vec::new());
    let _ = models(&mut desk.bar);
    desk
}

/// Every model the bar has been sent since the last look.
fn models(bar: &mut Receiver) -> Vec<BarModel> {
    let mut out = Vec::new();
    while let Ok((_, event)) = bar.try_recv() {
        if let BarEvent::Model(model) = event {
            out.push(model);
        }
    }
    out
}

/// Focus `window` the way macOS reports it: its application comes to the front, then the window
/// server names the window.
fn focus(desk: &mut Desk, window: WindowId, space: SpaceId) -> BarModel {
    desk.reactor.handle_test_batch(vec![
        Event::ApplicationGloballyActivated(window.pid),
        Event::WindowServerFocusChanged(window, space),
    ]);
    models(&mut desk.bar).pop().expect("a change of focus is sent")
}

#[test]
fn a_workspace_change_sends_a_model() {
    let mut desk = desk();
    desk.reactor.handle_test_batch(vec![Event::Command(Command::Layout(
        LayoutCommand::SwitchToWorkspace(1),
    ))]);
    let sent = models(&mut desk.bar);
    assert_eq!(sent.len(), 1, "one batch, one model");
    assert_eq!(
        sent[0].displays[0].rows,
        vec![Row::Occupied, Row::Shown, Row::Empty, Row::Empty]
    );
}

/// A title the bar does not draw is a change the bar never hears about. The focused window's is.
#[test]
fn an_event_that_changes_nothing_on_the_bar_sends_nothing() {
    let mut desk = desk();
    let left = desk.left;
    focus(&mut desk, left_ghostty(), left);

    desk.reactor.handle_test_batch(vec![
        Event::WindowTitleChanged(left_zen(), "a tab".into()),
        Event::WindowTitleChanged(right_zen(), "another tab".into()),
    ]);
    assert!(models(&mut desk.bar).is_empty());

    desk.reactor.handle_test_batch(vec![Event::WindowTitleChanged(
        left_ghostty(),
        "~/p/rini".into(),
    )]);
    let sent = models(&mut desk.bar);
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].displays[0].focus,
        Some(FocusLabel {
            app: "Ghostty".into(),
            title: "~/p/rini".into()
        })
    );
}

/// The numeral is on the right display's bar, so the right display switches, although the left one
/// has focus and is where a keyboard switch would land.
#[test]
fn a_numeral_click_switches_the_display_it_is_on_and_leaves_the_focused_one_alone() {
    let mut desk = desk();
    let (left, right) = (desk.left, desk.right);
    focus(&mut desk, left_ghostty(), left);
    let left_shown = desk.reactor.test_active_workspace(left);

    desk.reactor.handle_test_batch(vec![Event::BarAction(Action::ShowWorkspace {
        display: RIGHT.into(),
        index: 2,
    })]);

    assert_eq!(
        desk.reactor.test_active_workspace(right),
        Some(desk.reactor.test_workspace(right, 2))
    );
    assert_eq!(desk.reactor.test_active_workspace(left), left_shown);
    let sent = models(&mut desk.bar).pop().expect("the switch is sent");
    assert_eq!(sent.displays[0].rows[0], Row::Shown);
    assert_eq!(sent.displays[1].rows[2], Row::Shown);
}

#[test]
fn a_numeral_click_naming_no_known_display_does_nothing() {
    let mut desk = desk();
    let (left, right) = (desk.left, desk.right);
    let shown = (
        desk.reactor.test_active_workspace(left),
        desk.reactor.test_active_workspace(right),
    );
    desk.reactor.handle_test_batch(vec![Event::BarAction(Action::ShowWorkspace {
        display: "unplugged".into(),
        index: 2,
    })]);
    assert_eq!(
        (
            desk.reactor.test_active_workspace(left),
            desk.reactor.test_active_workspace(right),
        ),
        shown
    );
    assert!(models(&mut desk.bar).is_empty());
}

#[test]
fn a_glyph_click_focuses_its_window() {
    let mut desk = desk();
    let left = desk.left;
    focus(&mut desk, left_ghostty(), left);
    let outcome = desk.reactor.dispatch_test_bar_action(Action::Focus(left_zen()));
    let raised = outcome.raise_requests.iter().find_map(|request| match request {
        crate::windows::domain::raise::Event::RaiseRequest(request) => {
            request.focus_window.map(|(window, _)| window)
        }
        _ => None,
    });
    assert_eq!(raised, Some(left_zen()));
}

/// Turned off, the bars are sent a model with no displays in it, which takes them down, and nothing
/// after that until it is turned back on.
#[test]
fn a_bar_turned_off_is_sent_an_empty_model() {
    let mut desk = desk();
    let mut config = desk.reactor.config.clone();
    config.settings.bar.enabled = false;
    desk.reactor.handle_test_batch(vec![Event::ConfigUpdated(config)]);
    assert_eq!(models(&mut desk.bar), vec![BarModel::default()]);

    desk.reactor.handle_test_batch(vec![Event::Command(Command::Layout(
        LayoutCommand::SwitchToWorkspace(1),
    ))]);
    assert!(models(&mut desk.bar).is_empty());
}

/// The application and title go on the display the focused window is on, and the other stays silent.
#[test]
fn the_focus_label_moves_with_focus() {
    let mut desk = desk();
    let (left, right) = (desk.left, desk.right);

    let sent = focus(&mut desk, left_ghostty(), left);
    assert_eq!(
        sent.displays[0].focus.as_ref().map(|label| label.app.as_str()),
        Some("Ghostty")
    );
    assert_eq!(sent.displays[1].focus, None);
    assert!(sent.displays[0].glyphs[0].lit);

    let sent = focus(&mut desk, right_zen(), right);
    assert_eq!(sent.displays[0].focus, None);
    assert_eq!(
        sent.displays[1].focus.as_ref().map(|label| label.app.as_str()),
        Some("Zen")
    );
    assert!(sent.displays[1].glyphs[0].lit);
    assert!(sent.displays[0].glyphs.iter().all(|glyph| !glyph.lit));
}

/// A workspace is bright on a display only if it holds windows ON THAT DISPLAY. Workspace 2 holds a
/// window on the right and none on the left; workspace 0 holds windows on both.
#[test]
fn occupied_rows_are_per_display() {
    let mut desk = desk();
    let input = desk.reactor.bar_input();
    assert_eq!(input.displays[0].occupied, vec![true, false, false, false]);
    assert_eq!(input.displays[1].occupied, vec![true, false, true, false]);

    desk.reactor.handle_test_batch(vec![Event::Command(Command::Layout(
        LayoutCommand::SwitchToWorkspace(1),
    ))]);
    let sent = models(&mut desk.bar).pop().expect("the switch is sent");
    assert_eq!(
        sent.displays[0].rows,
        vec![Row::Occupied, Row::Shown, Row::Empty, Row::Empty]
    );
    assert_eq!(
        sent.displays[1].rows,
        vec![Row::Shown, Row::Empty, Row::Occupied, Row::Empty]
    );
}

/// The shown workspace's windows, in the order the workspace holds them, each with its
/// application's name, and each display with its own.
#[test]
fn each_display_is_told_its_own_shown_windows() {
    let desk = desk();
    let input = desk.reactor.bar_input();
    let windows = |display: usize| -> Vec<(WindowId, String)> {
        input.displays[display]
            .windows
            .iter()
            .map(|window| (window.window, window.app.clone()))
            .collect()
    };
    assert_eq!(input.displays[0].uuid, LEFT);
    assert_eq!(
        windows(0),
        vec![
            (left_ghostty(), "Ghostty".to_string()),
            (left_zen(), "Zen".to_string())
        ]
    );
    assert_eq!(input.displays[1].uuid, RIGHT);
    assert_eq!(windows(1), vec![(right_zen(), "Zen".to_string())]);
    assert_eq!(input.displays[1].shown, Some(0));
}

/// A display on a native fullscreen space has no user space in the snapshot, and no bar.
#[test]
fn a_display_without_a_user_space_has_no_bar() {
    let mut desk = desk();
    let left_frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let right_frame = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let left = desk.left;
    desk.reactor.handle_test_batch(vec![space_state_event(
        vec![left_frame, right_frame],
        vec![Some(left), None],
    )]);
    let sent = models(&mut desk.bar).pop().expect("the lost bar is sent");
    let uuids: Vec<&str> = sent.displays.iter().map(|display| display.uuid.as_str()).collect();
    assert_eq!(uuids, vec![LEFT]);
}

#[test]
fn waking_has_the_bars_read_the_clock() {
    let mut desk = desk();
    desk.reactor.handle_test_batch(vec![Event::SystemWoke]);
    let mut clock_changed = false;
    while let Ok((_, event)) = desk.bar.try_recv() {
        clock_changed |= matches!(event, BarEvent::ClockChanged);
    }
    assert!(clock_changed);
}
