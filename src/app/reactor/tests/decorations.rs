//! What the rest windows are sent after a batch of events: each managed window's ring and border,
//! with the key window holding focus, and what holds its bands.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::WindowServerId;
use test_log::test;

use super::fixtures::*;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::decorations::domain::paint::{Colour, Paint};
use crate::decorations::domain::rest::{RestModel, RestWindow};
use crate::decorations::domain::rules::{BandRule, DecorationRule};
use crate::decorations::platform::rest::{Event as RestEvent, Receiver};
use crate::windows::domain::rules::RuleMatch;
use crate::windows::platform::window_server::set_space_window_list_for_space_override;
use crate::workspaces::{LayoutCommand, LayoutEvent};

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


/// With the ring off, focus still decides the border's paint, and every managed window wears it.
#[test]
fn with_only_borders_on_every_managed_window_is_sent_its_border_and_whether_it_holds_focus() {
    let mut desk = desk();
    let decorations = &mut desk.reactor.config.settings.decorations;
    decorations.focus_ring.on = false;
    decorations.border.on = true;
    let left = desk.left;
    let model = focus(&mut desk, left_ghostty(), left);
    for wsid in [901, 902, 903, 904] {
        let window = sent(&model, wsid);
        assert_eq!(window.decoration.border, Some(desk.reactor.config.settings.decorations.border.band), "{wsid}");
        assert_eq!(window.decoration.ring, None, "{wsid}");
        assert_eq!(window.focused, wsid == 901, "{wsid}");
    }
}

fn ruled(rule: DecorationRule) -> Vec<DecorationRule> { vec![rule] }

fn border(on: Option<bool>, width: Option<f64>) -> BandRule { BandRule { on, width, ..BandRule::default() } }

/// niri's border rules follow focus as it moves: the window focus moves to takes the colours a rule
/// gives the focused window, and the one it left those a rule gives the others.
#[test]
fn focus_moving_recolours_the_two_windows_it_moved_between() {
    let mut desk = desk();
    let (red, green) = (Paint::Solid(Colour::rgb(0xff0000)), Paint::Solid(Colour::rgb(0x00ff00)));
    let decorations = &mut desk.reactor.config.settings.decorations;
    decorations.border.on = true;
    decorations.rules = vec![
        DecorationRule {
            border: BandRule { active: Some(red), ..BandRule::default() },
            ..rule_for(RuleMatch { is_focused: Some(true), ..RuleMatch::default() })
        },
        DecorationRule {
            border: BandRule { inactive: Some(green), ..BandRule::default() },
            ..rule_for(RuleMatch { is_active: Some(false), ..RuleMatch::default() })
        },
    ];
    let left = desk.left;
    let paint = |model: &RestModel, wsid| sent(model, wsid).style().border;
    let model = focus(&mut desk, left_ghostty(), left);
    assert_eq!((paint(&model, 901), paint(&model, 902)), (Some(red), Some(green)));
    let model = focus(&mut desk, left_zen(), left);
    assert_eq!((paint(&model, 901), paint(&model, 902)), (Some(green), Some(red)));
}

fn rule_for(line: RuleMatch) -> DecorationRule { DecorationRule { matches: vec![line], ..DecorationRule::default() } }

fn worn_border(model: &RestModel, reactor: &Reactor, window: WindowId) -> Option<f64> {
    sent(model, reactor.test_window_server_id(window).as_u32()).decoration.border.map(|band| band.width)
}

/// A rule for tiled windows gives each a border out of its slot, the strip laid out again as soon as
/// it applies; a window floated wears what the rules give a floating window, and laid back in the
/// strip, its slot's border again.
#[test]
fn toggling_floating_refolds_and_relays_the_window_out() {
    let (mut apps, mut reactor) = test_context();
    let mut rest = reactor.connect_test_rest();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    apps.requests();
    let (window, other) = (WindowId::new(1, 1), WindowId::new(1, 2));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, window));
    reactor.handle_test_batch(Vec::new());
    let slots = test_layout(&mut reactor, space, screen);
    reactor.config.settings.decorations.rules = ruled(DecorationRule {
        border: border(Some(true), Some(6.0)),
        ..rule_for(RuleMatch { is_floating: Some(false), ..RuleMatch::default() })
    });
    reactor.handle_test_batch(Vec::new());
    let inset = test_layout(&mut reactor, space, screen);
    for (laid, slot) in slots.iter().map(|(w, slot)| (frame_of(&inset, *w), *slot)) {
        assert_eq!(laid, crate::layout::domain::tile::window_in(slot, 6.0), "laid out again in its slot");
    }
    let model = models(&mut rest).pop().expect("sent");
    assert_eq!((worn_border(&model, &reactor, window), worn_border(&model, &reactor, other)), (Some(6.0), Some(6.0)));

    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    reactor.handle_test_batch(Vec::new());
    let model = models(&mut rest).pop().expect("sent");
    assert!(reactor.layout_manager.layout_engine.is_window_floating(window));
    assert_eq!((worn_border(&model, &reactor, window), worn_border(&model, &reactor, other)), (None, Some(6.0)));

    reactor.handle_test_layout_command(LayoutCommand::ToggleWindowFloating);
    reactor.handle_test_batch(Vec::new());
    let model = models(&mut rest).pop().expect("sent");
    assert_eq!(worn_border(&model, &reactor, window), Some(6.0));
    let back = frame_of(&test_layout(&mut reactor, space, screen), window);
    let slot = crate::layout::domain::tile::slot_of(back, 6.0);
    assert!(slots.iter().any(|(_, laid)| laid.size == slot.size), "inset in a slot the strip lays out: {back:?}");
}

/// A title a rule matches on is read afresh: the window it now names is laid out inside its slot by
/// the rule's border, with no other event asking for a pass.
#[test]
fn a_title_a_border_rule_names_lays_the_strip_out_again() {
    let (mut apps, mut reactor) = test_context();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(2));
    apps.requests();
    reactor.config.settings.decorations.rules = ruled(DecorationRule {
        border: border(Some(true), Some(6.0)),
        ..rule_for(RuleMatch::title("^Wide$"))
    });
    reactor.handle_test_batch(Vec::new());
    let slots = test_layout(&mut reactor, space, screen);
    let window = WindowId::new(1, 1);
    reactor.handle_test_batch(vec![Event::WindowTitleChanged(window, "Wide".into())]);
    let laid = test_layout(&mut reactor, space, screen);
    assert_eq!(frame_of(&laid, window), crate::layout::domain::tile::window_in(frame_of(&slots, window), 6.0));
    let other = WindowId::new(1, 2);
    assert_eq!(frame_of(&laid, other), frame_of(&slots, other), "the rule names one window");
}

/// One `geometry-corner-radius` rule reaches the window's bands on its tile and at rest alike, ahead
/// of what the window server reports.
#[test]
fn a_rule_radius_reaches_the_tile_and_the_rest_panel() {
    let mut desk = desk();
    desk.reactor.config.settings.decorations.rules =
        ruled(DecorationRule { corner_radius: Some(9.0), ..rule_for(RuleMatch::app("com.mitchellh.ghostty")) });
    let left = desk.left;
    let model = focus(&mut desk, left_ghostty(), left);
    assert_eq!((sent(&model, 901).decoration.radius, sent(&model, 902).decoration.radius), (Some(9.0), None));
    let rings = desk.reactor.layout_rings();
    assert_eq!(desk.reactor.worn(left_ghostty(), &rings).decoration.radius, Some(9.0), "carried to its tile");
    assert_eq!(desk.reactor.worn(left_zen(), &rings).decoration.radius, None);
}

fn frame_of(layout: &[(WindowId, CGRect)], window: WindowId) -> CGRect {
    layout.iter().find(|(w, _)| *w == window).map(|(_, frame)| *frame).expect("laid out")
}
