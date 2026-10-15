use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::ScreenId;

use super::*;

fn screen(id: u32, display: &str, x: f64, width: f64, space: Option<u64>) -> ScreenInfo {
    let frame = CGRect::new(CGPoint::new(x, 0.0), CGSize::new(width, 800.0));
    ScreenInfo {
        is_builtin: false,
        id: ScreenId::new(id),
        frame,
        bounds: frame,
        display_uuid: display.to_string(),
        name: None,
        space: space.map(SpaceId::new),
        scale: 2.0,
    }
}

fn spaces(screens: &[ScreenInfo]) -> Vec<Option<u64>> {
    screens.iter().map(|screen| screen.space.map(|space| space.get())).collect()
}

fn wsid(n: u32) -> WindowServerId { WindowServerId::new(n) }

fn space(n: u64) -> SpaceId { SpaceId::new(n) }

const LOGIN: u64 = 7;

fn kinds_with_a_login_space() -> SpaceKinds {
    SpaceKinds {
        is_fullscreen: |space| space.get() >= TEST_FULLSCREEN_SPACE,
        is_user: |space| space.get() != LOGIN,
    }
}

#[test]
fn only_user_spaces_are_left_on_the_screens_and_the_fullscreen_ones_are_kept_aside() {
    let fullscreen = TEST_FULLSCREEN_SPACE + 1;
    let screens = vec![
        screen(1, "a", 0.0, 1000.0, Some(LOGIN)),
        screen(2, "b", 1000.0, 1000.0, Some(fullscreen)),
        screen(3, "c", 2000.0, 1000.0, Some(8)),
    ];

    let (screens, fullscreen_spaces) = user_screens(&[], screens, kinds_with_a_login_space());

    assert_eq!(spaces(&screens), vec![None, None, Some(8)]);
    assert_eq!(fullscreen_spaces, HashSet::from_iter([space(fullscreen)]));
}

/// While display b enters fullscreen, macOS reports display a showing b's old user space. Display a
/// keeps its own.
#[test]
fn a_display_keeps_its_user_space_while_another_enters_fullscreen_and_reports_it_there() {
    let previous = [
        screen(1, "a", 0.0, 1000.0, Some(11)),
        screen(2, "b", 1000.0, 1000.0, Some(21)),
    ];
    let screens = vec![
        screen(1, "a", 0.0, 1000.0, Some(21)),
        screen(2, "b", 1000.0, 1000.0, Some(TEST_FULLSCREEN_SPACE + 21)),
    ];

    let (screens, _) = user_screens(&previous, screens, SpaceKinds::for_tests());

    assert_eq!(spaces(&screens), vec![Some(11), None]);
}

/// Outside a fullscreen entry the same report is taken as it is.
#[test]
fn without_a_fullscreen_entry_a_display_takes_the_space_reported_for_it() {
    let previous = [
        screen(1, "a", 0.0, 1000.0, Some(11)),
        screen(2, "b", 1000.0, 1000.0, Some(21)),
    ];
    let screens = vec![
        screen(1, "a", 0.0, 1000.0, Some(21)),
        screen(2, "b", 1000.0, 1000.0, Some(22)),
    ];

    let (screens, _) = user_screens(&previous, screens, SpaceKinds::for_tests());

    assert_eq!(spaces(&screens), vec![Some(21), Some(22)]);
}

#[test]
fn the_active_display_names_the_command_space_over_the_active_space() {
    let screens = [
        screen(1, "left", 0.0, 1000.0, Some(1)),
        screen(2, "right", 1000.0, 1000.0, Some(2)),
    ];

    assert_eq!(
        active_display_space(&screens, Some("right"), Some(space(1))),
        Some(space(2))
    );
}

#[test]
fn without_the_active_display_the_active_space_then_the_first_screen_names_it() {
    let screens = [
        screen(1, "left", 0.0, 1000.0, Some(1)),
        screen(2, "right", 1000.0, 1000.0, Some(2)),
    ];

    assert_eq!(
        active_display_space(&screens, Some("missing"), Some(space(2))),
        Some(space(2))
    );
    assert_eq!(
        active_display_space(&screens, Some("missing"), None),
        Some(space(1))
    );
    assert_eq!(
        active_display_space(&screens, None, Some(space(9))),
        Some(space(1))
    );
}

#[test]
fn a_window_two_active_spaces_list_is_on_the_one_it_calls_home() {
    let listed = vec![
        (space(511), vec![(wsid(92), None)]),
        (space(512), vec![(wsid(92), Some(space(512)))]),
    ];

    let visible = visible_window_spaces(&listed, &HashMap::default(), false);

    assert_eq!(visible.get(&wsid(92)), Some(&space(512)));
}

/// When the home-space read does not answer, the window stays on the active space it was on.
#[test]
fn a_window_two_active_spaces_list_keeps_its_previous_space_when_its_home_is_unknown() {
    let listed = vec![
        (space(502), vec![(wsid(91), None)]),
        (space(501), vec![(wsid(91), None)]),
    ];
    let previous = HashMap::from_iter([(wsid(91), space(501))]);

    let visible = visible_window_spaces(&listed, &previous, false);

    assert_eq!(visible.get(&wsid(91)), Some(&space(501)));
}

#[test]
fn an_empty_listing_is_the_truth_unless_the_previous_windows_are_kept() {
    let listed = vec![(space(1), vec![])];
    let previous = HashMap::from_iter([(wsid(5), space(1)), (wsid(6), space(2))]);

    assert!(visible_window_spaces(&listed, &previous, false).is_empty());
    assert_eq!(
        visible_window_spaces(&listed, &previous, true),
        HashMap::from_iter([(wsid(5), space(1))]),
        "only the windows on a space still active"
    );
}

#[test]
fn the_active_spaces_are_each_screen_s_space_once_in_screen_order() {
    let screens = [
        screen(1, "a", 0.0, 1000.0, Some(3)),
        screen(2, "b", 1000.0, 1000.0, None),
        screen(3, "c", 2000.0, 1000.0, Some(1)),
        screen(4, "d", 3000.0, 1000.0, Some(3)),
    ];

    assert_eq!(active_spaces(&screens), vec![space(3), space(1)]);
}

#[test]
fn a_window_that_changed_space_in_a_churn_disappears_from_one_and_appears_on_the_other() {
    let previous = HashMap::from_iter([(wsid(1), space(10)), (wsid(2), space(10))]);
    let current = HashMap::from_iter([(wsid(1), space(11)), (wsid(3), space(11))]);

    let delta = window_delta(4, DisplayReconfigFlags::MOVED, &previous, &current);

    let mut appeared = delta.appeared.clone();
    appeared.sort_by_key(|(w, _)| w.as_u32());
    let mut disappeared = delta.disappeared.clone();
    disappeared.sort_by_key(|(w, _)| w.as_u32());
    assert_eq!(appeared, vec![(wsid(1), space(11)), (wsid(3), space(11))]);
    assert_eq!(disappeared, vec![(wsid(1), space(10)), (wsid(2), space(10))]);
    assert_eq!((delta.epoch, delta.flags), (4, DisplayReconfigFlags::MOVED));
}

fn taken(screens: Vec<ScreenInfo>) -> Taken { Taken { screens, ..Taken::default() } }

#[test]
fn a_second_display_is_a_topology_change_that_remaps_the_first_display_s_new_space() {
    let mut memory = Forwarded::default();
    forward(&mut memory, taken(vec![screen(1, "a", 0.0, 1000.0, Some(31))]));

    let state = forward(
        &mut memory,
        taken(vec![
            screen(1, "a", 0.0, 1000.0, Some(41)),
            screen(2, "b", 1000.0, 1000.0, Some(51)),
        ]),
    );

    assert!(state.display_set_changed && state.topology_changed);
    assert!(state.allow_space_remap && state.should_force_refresh_layout);
    assert_eq!(state.space_remaps, vec![(space(31), space(41))]);
    assert_eq!(state.resized_spaces, vec![(
        space(51),
        CGSize::new(1000.0, 800.0)
    )]);
    assert_eq!(memory.last_user_space_by_display.get("a"), Some(&space(41)));
}

/// macOS can report the remaining display with the space of one not back from sleep yet. The
/// display does not take that space into its history, and nothing is remapped onto it.
#[test]
fn a_display_showing_another_display_s_space_takes_none_of_its_history() {
    let mut memory = Forwarded::default();
    forward(
        &mut memory,
        taken(vec![
            screen(1, "builtin", 0.0, 1000.0, Some(3)),
            screen(2, "external", 1000.0, 1000.0, Some(506)),
        ]),
    );

    let state = forward(
        &mut memory,
        taken(vec![screen(2, "external", 0.0, 1000.0, Some(3))]),
    );

    assert!(state.space_remaps.is_empty());
    assert_eq!(
        state.last_user_space_by_display.get("external"),
        Some(&space(506))
    );
}

#[test]
fn a_space_on_two_screens_allows_no_remap() {
    let mut memory = Forwarded::default();
    forward(&mut memory, taken(vec![screen(1, "a", 0.0, 1000.0, Some(91))]));

    let state = forward(
        &mut memory,
        taken(vec![
            screen(1, "a", 0.0, 1000.0, Some(92)),
            screen(2, "b", 1000.0, 1000.0, Some(92)),
        ]),
    );

    assert!(state.topology_changed && !state.allow_space_remap);
    assert!(state.space_remaps.is_empty());
}

#[test]
fn the_window_server_s_space_list_stands_and_the_screens_stand_in_for_a_silent_one() {
    let screens = || vec![screen(1, "a", 0.0, 1000.0, Some(5))];
    let listed = HashMap::from_iter([("a".to_string(), vec![space(5), space(6)])]);
    let heard = Heard {
        spaces_per_display: listed.clone(),
        ..Heard::default()
    };

    let answered = forward(&mut Forwarded::default(), Taken { heard, ..taken(screens()) });
    let silent = forward(&mut Forwarded::default(), taken(screens()));

    assert_eq!(
        (
            answered.display_space_ids,
            answered.display_space_ids_from_screens
        ),
        (listed, false)
    );
    assert_eq!(silent.display_space_ids.get("a"), Some(&vec![space(5)]));
    assert!(silent.display_space_ids_from_screens);
}

#[test]
fn the_menu_bar_space_is_the_active_space_only_while_a_screen_shows_it() {
    let screens = || {
        vec![
            screen(1, "a", 0.0, 1000.0, Some(1)),
            screen(2, "b", 1000.0, 1000.0, Some(2)),
        ]
    };
    let heard = |active: u64| Heard {
        active_space: Some(space(active)),
        ..Heard::default()
    };

    let shown = forward(&mut Forwarded::default(), Taken {
        heard: heard(2),
        ..taken(screens())
    });
    let elsewhere = forward(&mut Forwarded::default(), Taken {
        heard: heard(9),
        ..taken(screens())
    });

    assert_eq!(shown.menu_bar_space, Some(space(2)));
    assert_eq!(elsewhere.menu_bar_space, Some(space(1)));
}

/// A snapshot with no space at all keeps the last one's for the menu bar and commands.
#[test]
fn a_snapshot_naming_no_space_keeps_the_last_one_s() {
    let mut memory = Forwarded::default();
    forward(&mut memory, taken(vec![screen(1, "a", 0.0, 1000.0, Some(4))]));

    let state = forward(&mut memory, taken(vec![screen(1, "a", 0.0, 1000.0, None)]));

    assert_eq!(
        (state.menu_bar_space, state.command_space),
        (Some(space(4)), Some(space(4)))
    );
}

#[test]
fn the_active_display_is_the_one_heard_while_attached_else_the_command_space_s() {
    let screens = || {
        vec![
            screen(1, "a", 0.0, 1000.0, Some(1)),
            screen(2, "b", 1000.0, 1000.0, Some(2)),
        ]
    };
    let heard = |display: &str| Heard {
        active_display: Some(display.to_string()),
        ..Heard::default()
    };
    let mut memory = Forwarded::default();

    forward(&mut memory, Taken {
        heard: heard("b"),
        ..taken(screens())
    });
    assert_eq!(memory.active_display_uuid.as_deref(), Some("b"));
    forward(&mut memory, Taken {
        heard: heard("gone"),
        ..taken(screens())
    });
    assert_eq!(memory.active_display_uuid.as_deref(), Some("a"));
}
