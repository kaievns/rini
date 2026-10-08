//! What the rest windows are sent after a batch of events: each managed window's ring and border,
//! with the key window holding focus, and what holds its bands.
use objc2_core_foundation::{CGPoint, CGSize};
use rini_core::ids::WindowServerId;
use rini_geometry::space::ScreenRect;
use test_log::test;

use super::fixtures::*;
use crate::app::reactor::testing::*;
use crate::app::reactor::*;
use crate::decorations::domain::paint::{Colour, Paint};
use crate::decorations::domain::rest::{RestModel, RestWindow};
use crate::decorations::domain::rules::{BandRule, DecorationRule};
use crate::decorations::domain::style::Ring;
use crate::decorations::platform::rest::{Event as RestEvent, Receiver};
use crate::windows::domain::attention::Attention;
use crate::windows::domain::catalogue::NativeFullscreenTransition;
use crate::windows::domain::rules::RuleMatch;
use crate::windows::platform::window_server::set_space_window_list_for_space_override;
use crate::workspaces::{Direction, LayoutCommand, LayoutEvent};

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

/// (focused, ring, parked) of the window `wsid` names.
fn look(model: &RestModel, wsid: u32) -> (bool, Ring, bool) {
    let window = sent(model, wsid);
    (window.focused, window.ring, window.held.parked)
}

#[test]
fn a_focus_change_sends_each_displays_ring_to_the_rest_actor() {
    let mut desk = desk();
    let left = desk.left;
    let model = focus(&mut desk, left_ghostty(), left);
    assert_eq!(model.displays.len(), 2);
    assert_eq!(look(&model, 901), (true, Ring::Active, false), "the focused window: the active ring");
    assert_eq!(look(&model, 902), (false, Ring::None, false), "not its workspace's active window");
    assert_eq!(look(&model, 903), (false, Ring::Inactive, false), "the other display's: the inactive ring");
    assert_eq!(look(&model, 904), (false, Ring::Inactive, true), "its workspace's, which its display does not show");
    assert!(sent(&model, 901).decoration.ring.is_some());
}

/// niri paints every workspace's active window on the focused monitor active: a workspace a switch
/// leaves keeps its window's ring active as it slides away, and the other display's turn inactive.
#[test]
fn every_ring_on_the_display_holding_focus_is_sent_active() {
    let mut desk = desk();
    let (left, right) = (desk.left, desk.right);
    focus(&mut desk, left_ghostty(), left);
    let model = focus(&mut desk, WindowId::new(ZEN, 2), right);
    assert_eq!(look(&model, 903), (true, Ring::Active, false));
    assert_eq!(look(&model, 904), (false, Ring::Active, true), "another workspace on the display holding focus");
    assert_eq!(look(&model, 901), (false, Ring::Inactive, false), "the display focus left");
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
    assert_eq!(look(&model, 902), (true, Ring::Active, false), "turned with the window server's report");
    assert_eq!(look(&model, 901), (false, Ring::None, false));
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

#[test]
fn no_band_is_sent_to_show_while_mission_control_is_up() {
    let mut desk = desk();
    let left = desk.left;
    focus(&mut desk, left_ghostty(), left);
    desk.reactor.handle_test_batch(vec![Event::MissionControlNativeEntered]);
    let model = models(&mut desk.rest).pop().expect("sent");
    assert!(!model.windows.is_empty() && model.windows.iter().all(|window| window.held.mission_control));
    desk.reactor.handle_test_batch(vec![Event::MissionControlNativeExited]);
    let model = models(&mut desk.rest).pop().expect("sent");
    assert!(model.windows.iter().all(|window| !window.held.mission_control));
}

#[test]
fn a_window_macos_takes_into_native_fullscreen_is_sent_held() {
    let mut desk = desk();
    let left = desk.left;
    focus(&mut desk, left_ghostty(), left);
    let _ = desk.reactor.state.windows.suspend_window_to_native_fullscreen(
        left_ghostty(),
        Some(WindowServerId::new(901)),
        Some(left),
        SpaceId::new(0x400000001),
        NativeFullscreenTransition::Suspended,
    );
    desk.reactor.handle_test_batch(Vec::new());
    let model = models(&mut desk.rest).pop().expect("sent");
    assert!(sent(&model, 901).held.fullscreen);
    assert!(!sent(&model, 902).held.fullscreen && !sent(&model, 903).held.fullscreen, "only that window");
}

/// A Space a display stops showing is no longer active, so its windows are sent no band at all.
#[test]
fn a_window_on_a_space_its_display_is_not_showing_is_sent_no_band() {
    let mut desk = desk();
    let left = desk.left;
    focus(&mut desk, left_ghostty(), left);
    let left_frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let right_frame = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let other = SpaceId::new(3);
    set_space_membership(&[(left, &[901, 902]), (other, &[])]);
    desk.reactor.handle_test_batch(vec![space_state_event(vec![left_frame, right_frame], vec![Some(left), Some(other)])]);
    let model = models(&mut desk.rest).pop().expect("sent");
    let sent_ids: Vec<u32> = model.windows.iter().map(|window| window.window.as_u32()).collect();
    assert!(sent_ids.contains(&901) && !sent_ids.contains(&903) && !sent_ids.contains(&904), "{sent_ids:?}");
}

/// A tiled window gives its border out of its slot as its own display draws it, in whole device
/// pixels: 1.5pt is 3 of them at 2x and rounds to 2pt at 1x.
#[test]
fn each_window_gives_its_border_out_of_its_slot_at_its_own_displays_scale() {
    let mut desk = desk();
    let decorations = &mut desk.reactor.config.settings.decorations;
    decorations.border.on = true;
    decorations.border.band.width = 1.5;
    let left_frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1440., 900.));
    let right_frame = CGRect::new(CGPoint::new(1440., 0.), CGSize::new(1440., 900.));
    let mut screens = make_screen_snapshots(vec![left_frame, right_frame], vec![Some(desk.left), Some(desk.right)]);
    screens[1].scale = 1.0;
    desk.reactor.handle_test_batch(vec![space_state_event_from_screens(screens)]);
    let borders = desk.reactor.borders();
    assert_eq!((borders.of(left_ghostty()), borders.of(WindowId::new(ZEN, 2))), (1.5, 2.0));
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

/// macOS asks per application: while Zen asks, each of its windows is sent urgent, on both displays
/// and ringed or not, until one of its windows holds focus, before LaunchServices says it stopped.
#[test]
fn an_application_asking_for_attention_is_sent_urgent_until_one_of_its_windows_holds_focus() {
    let mut desk = desk();
    let left = desk.left;
    focus(&mut desk, left_ghostty(), left);
    desk.reactor.handle_test_batch(vec![Event::AttentionChanged(Attention::of([ZEN]))]);
    let model = models(&mut desk.rest).pop().expect("the ask is sent");
    let urgent = |model: &RestModel| [901, 902, 903, 904].map(|wsid| sent(model, wsid).decoration.urgent);
    assert_eq!(urgent(&model), [false, true, true, false]);
    let ring = desk.reactor.config.settings.decorations.focus_ring.band;
    assert_eq!(sent(&model, 903).style().ring, Some(ring.urgent), "the other display's ring");
    let model = focus(&mut desk, left_zen(), left);
    assert_eq!(urgent(&model), [false; 4]);
    assert_eq!(sent(&model, 902).style().ring, Some(ring.active));
    desk.reactor.handle_test_batch(vec![Event::AttentionChanged(Attention::default())]);
    assert!(models(&mut desk.rest).is_empty(), "nothing left to turn");
}

/// A pass ends with its focus, so a pass that focuses a window of an asking application carries
/// every window of it calm, as the rest windows will have them once macOS makes it key.
#[test]
fn a_pass_focusing_an_asking_application_carries_its_windows_calm() {
    let mut desk = desk();
    let left = desk.left;
    focus(&mut desk, left_ghostty(), left);
    desk.reactor.handle_test_batch(vec![Event::AttentionChanged(Attention::of([ZEN]))]);
    let rings = desk.reactor.layout_rings();
    assert!(desk.reactor.worn(left_zen(), &rings).decoration.urgent);
    desk.reactor.send_layout_event(LayoutEvent::WindowFocused(left, left_zen()));
    let rings = desk.reactor.layout_rings();
    for window in [left_zen(), WindowId::new(ZEN, 2)] {
        assert!(!desk.reactor.worn(window, &rings).decoration.urgent, "{window:?}");
    }
}

/// Every word the rest windows have been sent since the last look: a model, or `None` for off.
fn words(rest: &mut Receiver) -> Vec<Option<RestModel>> {
    let mut out = Vec::new();
    while let Ok((_, event)) = rest.try_recv() {
        match event {
            RestEvent::Model(model) => out.push(Some(model)),
            RestEvent::Off => out.push(None),
            _ => {}
        }
    }
    out
}

/// Reloads the config with the decorations `change` leaves.
fn reload(desk: &mut Desk, change: impl FnOnce(&mut crate::decorations::settings::DecorationSettings)) {
    reload_config(&mut desk.reactor, change);
}

fn reload_config(reactor: &mut Reactor, change: impl FnOnce(&mut crate::decorations::settings::DecorationSettings)) {
    let mut config = reactor.config.clone();
    change(&mut config.settings.decorations);
    reactor.handle_test_batch(vec![Event::ConfigUpdated(config)]);
}

/// Kai, 2026-10-08: "not make it transparrent, i mean disable the feature". With the ring and the
/// border off the rest windows are turned off once, the window server's frames and orders go
/// unfollowed, and focus moving sends nothing; turning the ring back on starts it all again.
#[test]
fn with_the_ring_and_the_border_off_the_rest_windows_are_turned_off_and_nothing_is_followed() {
    let mut desk = desk();
    assert!(desk.reactor.rest.following.on(), "the ring on by default");
    reload(&mut desk, |decorations| decorations.focus_ring.on = false);
    assert_eq!(words(&mut desk.rest), vec![None], "turned off, once");
    assert!(!desk.reactor.rest.following.on());
    let left = desk.left;
    desk.reactor.handle_test_batch(vec![
        Event::ApplicationGloballyActivated(left_zen().pid),
        Event::WindowServerFocusChanged(left_zen(), left),
    ]);
    reload(&mut desk, |decorations| decorations.focus_ring.band.width = 6.0);
    assert!(words(&mut desk.rest).is_empty(), "focus and a reload that turns nothing on send nothing");
    assert!(desk.reactor.layout_rings() == crate::decorations::domain::style::Rings::default(), "no ring read");
    reload(&mut desk, |decorations| decorations.focus_ring.on = true);
    let [Some(model)] = &words(&mut desk.rest)[..] else { panic!("one model, turning it on") };
    assert_eq!(sent(model, 902).decoration.ring.map(|band| band.width), Some(6.0));
    assert!(desk.reactor.rest.following.on());
    reload(&mut desk, |decorations| {
        decorations.focus_ring.on = false;
        decorations.border.on = true;
    });
    assert!(matches!(&words(&mut desk.rest)[..], [Some(_)]) && desk.reactor.rest.following.on(), "the border alone keeps it on");
}

/// A window rule turning the ring on for one application keeps the rest windows on with both off in
/// `layout`, and only that application's windows wear it.
#[test]
fn a_rule_turning_one_windows_ring_on_keeps_the_rest_windows_on() {
    let mut desk = desk();
    reload(&mut desk, |decorations| {
        decorations.focus_ring.on = false;
        decorations.rules = ruled(DecorationRule {
            focus_ring: BandRule { on: Some(true), ..BandRule::default() },
            ..rule_for(RuleMatch::app("com.mitchellh.ghostty"))
        });
    });
    let [Some(model)] = &words(&mut desk.rest)[..] else { panic!("still on, with the rule") };
    assert!(desk.reactor.rest.following.on());
    let ringed = |wsid| sent(model, wsid).decoration.ring.is_some();
    assert_eq!([901, 902, 903, 904].map(ringed), [true, false, false, true]);
    reload(&mut desk, |decorations| decorations.rules.clear());
    assert_eq!(words(&mut desk.rest), vec![None], "the rule gone, nothing is decorated");
}

fn frame_of(layout: &[(WindowId, CGRect)], window: WindowId) -> CGRect {
    layout.iter().find(|(w, _)| *w == window).map(|(_, frame)| *frame).expect("laid out")
}

/// What the rest windows have been told since the last look, by kind, with a flight's display.
fn told(rest: &mut Receiver) -> Vec<(&'static str, Option<ScreenRect>)> {
    let mut out = Vec::new();
    while let Ok((_, event)) = rest.try_recv() {
        out.push(match event {
            RestEvent::Flight(display) => ("flight", Some(display)),
            RestEvent::Model(_) => ("model", None),
            RestEvent::Off => ("off", None),
            _ => continue,
        });
    }
    out
}

/// Kai, 2026-10-08: "all the animations are _significantly_ slower with the rings". A press that
/// flies tells the rest windows the display its overlay covers before the model the press changed,
/// so that model waits for the lift instead of being drawn while the show waits; off, nothing.
#[test]
fn a_press_that_flies_tells_the_rest_windows_before_its_model() {
    use crate::transitions::platform::engine::Event as Engine;
    let (mut apps, mut reactor) = test_context();
    let mut rest = reactor.connect_test_rest();
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1728., 1117.));
    let space = SpaceId::new(1);
    reactor.handle_event(space_state_event(vec![screen], vec![Some(space)]));
    apps.make_app_and_settle(&mut reactor, 1, make_windows(4));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, WindowId::new(1, 1)));
    apps.simulate_until_quiet(&mut reactor);
    reactor.config.settings.animate = true;
    let (transitions_tx, mut transitions_rx) = channels::channel();
    reactor.communication_manager.transitions_tx = Some(transitions_tx);
    reactor.handle_test_batch(Vec::new());
    told(&mut rest);
    let press = |reactor: &mut Reactor, direction| {
        let command = crate::app::reactor::state::Command::Layout(LayoutCommand::MoveFocus(direction));
        reactor.handle_test_batch(vec![Event::Command(command)]);
    };
    let flown = |rx: &mut channels::Receiver<Engine>| {
        let mut display = None;
        let mut flights = 0;
        while let Ok((_, event)) = rx.try_recv() {
            match event {
                Engine::SetDisplay { frame, .. } => display = Some(frame),
                Engine::Animate { .. } | Engine::AnimateSurface { .. } => flights += 1,
                _ => {}
            }
        }
        (flights, display)
    };

    press(&mut reactor, Direction::Right);
    let (flights, display) = flown(&mut transitions_rx);
    assert!(flights > 0, "the press flies");
    assert_eq!(told(&mut rest), vec![("flight", display), ("model", None)], "the flight first, at the overlay's display");

    reload_config(&mut reactor, |decorations| decorations.focus_ring.on = false);
    assert_eq!(told(&mut rest), vec![("off", None)]);
    press(&mut reactor, Direction::Left);
    assert!(flown(&mut transitions_rx).0 > 0, "the press still flies");
    assert!(told(&mut rest).is_empty(), "off, the rest windows are told nothing");
}
