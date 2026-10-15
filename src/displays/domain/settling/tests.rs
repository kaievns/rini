use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::ScreenId;

use super::*;

fn screen(display: &str, x: f64, space: Option<u64>) -> ScreenInfo {
    let frame = CGRect::new(CGPoint::new(x, 0.0), CGSize::new(1000.0, 800.0));
    ScreenInfo {
        is_builtin: false,
        id: ScreenId::new(1),
        frame,
        bounds: frame,
        display_uuid: display.to_string(),
        name: None,
        space: space.map(SpaceId::new),
        scale: 2.0,
    }
}

const QUIET: fn() -> bool = || true;
const BUSY: fn() -> bool = || false;

#[test]
fn a_refresh_retries_ten_times() {
    assert_eq!(REFRESH_MAX_RETRIES, 10);
    assert_eq!(refresh_retry(0), Some(1));
    assert_eq!(refresh_retry(9), Some(10));
    assert_eq!(refresh_retry(10), None);
}

#[test]
fn a_churn_is_checked_again_ten_times_and_settles_at_two_identical_samples() {
    assert_eq!(DISPLAY_STABILIZE_MAX_ATTEMPTS, 10);
    assert_eq!(DISPLAY_STABLE_REQUIRED_HITS, 2);
}

#[test]
fn only_a_reconfiguration_of_the_displays_themselves_begins_a_churn() {
    assert!(!begins_display_churn(DisplayReconfigFlags::BEGIN_CONFIGURATION));
    assert!(!begins_display_churn(DisplayReconfigFlags::empty()));
    for flag in [
        DisplayReconfigFlags::ADD,
        DisplayReconfigFlags::REMOVE,
        DisplayReconfigFlags::MOVED,
        DisplayReconfigFlags::SET_MAIN,
        DisplayReconfigFlags::SET_MODE,
        DisplayReconfigFlags::ENABLED,
        DisplayReconfigFlags::DISABLED,
        DisplayReconfigFlags::MIRROR,
        DisplayReconfigFlags::UNMIRROR,
        DisplayReconfigFlags::DESKTOP_SHAPE_CHANGED,
    ] {
        assert!(begins_display_churn(
            flag | DisplayReconfigFlags::BEGIN_CONFIGURATION
        ));
    }
}

#[test]
fn the_second_identical_sample_of_a_coherent_topology_over_a_quiet_window_server_is_forwarded() {
    let mut stability = Stability::default();
    let screens = [screen("a", 0.0, Some(1)), screen("b", 1000.0, Some(2))];
    let kinds = SpaceKinds::for_tests();

    assert_eq!(
        settle(&mut stability, &screens, kinds, QUIET, 0),
        Settle::Again(1)
    );
    assert_eq!(
        settle(&mut stability, &screens, kinds, QUIET, 1),
        Settle::Forward
    );
}

#[test]
fn a_topology_that_moves_between_samples_starts_its_count_over() {
    let mut stability = Stability::default();
    let kinds = SpaceKinds::for_tests();
    let before = [screen("a", 0.0, Some(1))];
    let moved = [screen("a", 200.0, Some(1))];

    settle(&mut stability, &before, kinds, QUIET, 0);
    assert_eq!(settle(&mut stability, &moved, kinds, QUIET, 1), Settle::Again(2));
    assert_eq!(settle(&mut stability, &moved, kinds, QUIET, 2), Settle::Forward);
}

/// A new topology is checked again whatever the attempt: the cap counts only checks of a topology
/// already seen.
#[test]
fn a_new_topology_is_checked_again_even_at_the_cap() {
    let mut stability = Stability::default();
    let screens = [screen("a", 0.0, Some(1))];
    let at_cap = DISPLAY_STABILIZE_MAX_ATTEMPTS;

    assert_eq!(
        settle(&mut stability, &screens, SpaceKinds::for_tests(), QUIET, at_cap),
        Settle::Again(at_cap + 1)
    );
}

#[test]
fn a_busy_window_server_holds_a_settled_topology_until_the_cap_then_gives_up() {
    let mut stability = Stability::default();
    let screens = [screen("a", 0.0, Some(1))];
    let kinds = SpaceKinds::for_tests();
    settle(&mut stability, &screens, kinds, BUSY, 0);

    assert_eq!(
        settle(&mut stability, &screens, kinds, BUSY, 1),
        Settle::Again(2)
    );
    let at_cap = DISPLAY_STABILIZE_MAX_ATTEMPTS;
    assert_eq!(
        settle(&mut stability, &screens, kinds, BUSY, at_cap),
        Settle::GiveUp
    );
}

#[test]
fn no_screens_are_checked_again_until_the_cap_then_give_up() {
    let mut stability = Stability::default();
    let kinds = SpaceKinds::for_tests();
    let at_cap = DISPLAY_STABILIZE_MAX_ATTEMPTS;

    assert_eq!(
        settle(&mut stability, &[], kinds, QUIET, at_cap - 1),
        Settle::Again(at_cap)
    );
    assert_eq!(settle(&mut stability, &[], kinds, QUIET, at_cap), Settle::GiveUp);
}

/// A user space on two screens at once is never forwarded, and the count starts over each time.
#[test]
fn an_incoherent_topology_is_never_forwarded_and_starts_the_count_over() {
    let mut stability = Stability::default();
    let kinds = SpaceKinds::for_tests();
    let twice = [screen("a", 0.0, Some(1)), screen("b", 1000.0, Some(1))];
    settle(&mut stability, &twice, kinds, QUIET, 0);

    assert_eq!(settle(&mut stability, &twice, kinds, QUIET, 1), Settle::Again(2));
    assert_eq!(settle(&mut stability, &twice, kinds, QUIET, 2), Settle::Again(3));
    assert_eq!(settle(&mut stability, &twice, kinds, QUIET, 3), Settle::Again(4));
}

/// Whether the window server is quiet is asked only of a coherent topology sampled twice.
#[test]
fn the_window_server_is_asked_whether_it_is_quiet_only_of_a_topology_ready_to_forward() {
    let mut stability = Stability::default();
    let screens = [screen("a", 0.0, Some(1))];
    let never = || -> bool { panic!("asked before the topology was ready") };

    assert_eq!(
        settle(&mut stability, &screens, SpaceKinds::for_tests(), never, 0),
        Settle::Again(1)
    );
}
