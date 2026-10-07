//! What the rest windows are sent after a batch of events: each managed window's ring, with the key
//! window holding focus, and what holds its band.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::WindowServerId;
use test_log::test;

use super::fixtures::*;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::decorations::domain::rest::{RestModel, RestWindow};
use crate::decorations::platform::rest::{Event as RestEvent, Receiver};
use crate::windows::platform::window_server::set_space_window_list_for_space_override;
use crate::workspaces::LayoutEvent;

const GHOSTTY: i32 = 1;
const ZEN: i32 = 2;

/// Two displays. The left shows workspace 0 with Ghostty (901) and Zen (902). The right shows
/// workspace 0 with a Zen window (903), and holds a Ghostty window (904) on workspace 2.
struct Desk {
    reactor: Reactor,
    rest: Receiver,
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

fn left_ghostty() -> WindowId { WindowId::new(GHOSTTY, 1) }

fn left_zen() -> WindowId { WindowId::new(ZEN, 1) }

fn desk() -> Desk {
    let mut reactor = test_reactor();
    let rest = reactor.connect_test_rest();
    let (left, right) = (SpaceId::new(1), SpaceId::new(2));
    let left_frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let right_frame = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    set_space_membership(&[(left, &[901, 902]), (right, &[903, 904])]);
    reactor.handle_test_batch(vec![space_state_event(vec![left_frame, right_frame], vec![Some(left), Some(right)])]);
    reactor.add_test_app_with_info(GHOSTTY, "com.mitchellh.ghostty", "Ghostty");
    reactor.add_test_app_with_info(ZEN, "app.zen-browser.zen", "Zen");
    for (window, wsid, space, frame, workspace) in [
        (left_ghostty(), 901, left, left_frame, 0),
        (left_zen(), 902, left, left_frame, 0),
        (WindowId::new(ZEN, 2), 903, right, right_frame, 0),
        (WindowId::new(GHOSTTY, 2), 904, right, right_frame, 2),
    ] {
        reactor.add_test_window(window, WindowServerId::new(wsid), Some(space), frame);
        let workspace = reactor.test_workspace(space, workspace);
        assert!(reactor.assign_test_window_to_workspace(space, window, workspace));
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, window));
    }
    let mut desk = Desk { reactor, rest, left, right };
    desk.reactor.handle_test_batch(Vec::new());
    let _ = models(&mut desk.rest);
    desk
}

/// Every model the rest windows have been sent since the last look.
fn models(rest: &mut Receiver) -> Vec<RestModel> {
    let mut out = Vec::new();
    while let Ok((_, event)) = rest.try_recv() {
        if let RestEvent::Model(model) = event {
            out.push(model);
        }
    }
    out
}

/// Focus `window` the way macOS reports it: its application comes to the front, then the window
/// server names the window.
fn focus(desk: &mut Desk, window: WindowId, space: SpaceId) -> RestModel {
    desk.reactor.handle_test_batch(vec![
        Event::ApplicationGloballyActivated(window.pid),
        Event::WindowServerFocusChanged(window, space),
    ]);
    models(&mut desk.rest).pop().expect("a change of focus is sent")
}

fn sent(model: &RestModel, wsid: u32) -> RestWindow {
    *model.windows.iter().find(|window| window.window == WindowServerId::new(wsid)).expect("sent")
}

/// (focused, ringed, parked) of the window `wsid` names.
fn look(model: &RestModel, wsid: u32) -> (bool, bool, bool) {
    let window = sent(model, wsid);
    (window.focused, window.ringed, window.held.parked)
}

#[test]
fn a_focus_change_sends_each_displays_ring_to_the_rest_actor() {
    let mut desk = desk();
    let left = desk.left;
    let model = focus(&mut desk, left_ghostty(), left);
    assert_eq!(model.displays.len(), 2);
    assert_eq!(look(&model, 901), (true, true, false), "the focused window: the active ring");
    assert_eq!(look(&model, 902), (false, false, false), "not its workspace's active window");
    assert_eq!(look(&model, 903), (false, true, false), "the other display's: the inactive ring");
    assert_eq!(look(&model, 904), (false, true, true), "its workspace's, which its display does not show");
    assert!(sent(&model, 901).decoration.ring.is_some());
}

/// rini's decision moves the layout's focus at once; macOS repaints when its key window changes,
/// and the band at rest turns then.
#[test]
fn the_rest_look_turns_when_the_window_server_reports_the_focus() {
    let mut desk = desk();
    let left = desk.left;
    focus(&mut desk, left_ghostty(), left);
    desk.reactor.send_layout_event(LayoutEvent::WindowFocused(left, left_zen()));
    desk.reactor.handle_test_batch(Vec::new());
    assert!(models(&mut desk.rest).is_empty(), "macOS has not moved its key window: nothing turns");
    let model = focus(&mut desk, left_zen(), left);
    assert_eq!(look(&model, 902), (true, true, false), "turned with the window server's report");
    assert_eq!(look(&model, 901), (false, false, false));
}

#[test]
fn a_column_the_strip_parks_is_sent_to_the_rest_actor_as_parked() {
    let (mut apps, mut reactor) = test_context();
    let mut rest = reactor.connect_test_rest();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(4));
    apps.requests();
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(1, 1)));
    reactor.handle_test_batch(Vec::new());
    let model = models(&mut rest).pop().expect("sent");
    let strip = reactor.test_strip_layout(space);
    for (window, frame) in strip {
        let server_id = reactor.test_window_server_id(window);
        let off = rini_geometry::is_off_screen(
            rini_geometry::space::ScreenRect::from_cg(screen),
            rini_geometry::space::ScreenRect::from_cg(frame),
        );
        assert_eq!(sent(&model, server_id.as_u32()).held.parked, off, "{window:?} at {frame:?}");
    }
    assert!(model.windows.iter().any(|window| window.held.parked), "four 0.7-wide columns: the last is off screen");
    assert!(model.windows.iter().any(|window| !window.held.parked));
}

