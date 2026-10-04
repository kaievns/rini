//! What happens when a request arrives while a flight is already running.
//!
//! The layout does not wait for an animation to land, so a second pass can arrive mid-flight with a
//! different destination for a window already moving, or with a window the first pass had not placed.
//! Admitting one into the other without a visible jump is what these decisions are for. A move's
//! nudge and a push against an end arrive the same way: the nudge waits for its flight to move, and a
//! push while a bounce plays starts the next from where that one is drawn.

use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::WindowId;
use rini_geometry::{SameAs, to_overlay_space};
use tracing::debug;

use crate::animation::domain::motion::accents::{NudgeAdmission, NudgePlaying, nudge_admission};
use crate::animation::domain::motion::bounce::Bounce;
use crate::animation::domain::motion::nudge::NUDGE_STRETCH;

/// A window that joins the animation as soon as it has a picture. The flight holds at frame zero
/// for it. See "The reservation fallback" in `src/transitions/docs/coordination.md`.
#[derive(Debug, Clone)]
pub(in crate::transitions) struct PendingEntrance {
    pub(in crate::transitions) window: WindowId,
    /// Destination, in the overlay's coordinate space.
    pub(in crate::transitions) to: CGRect,
    pub(in crate::transitions) floating: bool,
}

/// What became of a tile offered to an animation in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::transitions) enum Admitted {
    /// Same window, same destination: a redundant pass. Nothing restarts, so rapid presses
    /// neither restart nor extend the flight.
    Redundant,
    /// Same window, new destination: the tile bends toward it mid-flight.
    Retargeted,
    /// A window this animation had not seen yet.
    Joined,
}

/// The merge decision for one tile.
pub(in crate::transitions) fn merge_action(
    current_to: Option<CGRect>,
    incoming_to: CGRect,
) -> Admitted {
    match current_to {
        Some(to) if to.same_as(incoming_to) => Admitted::Redundant,
        Some(_) => Admitted::Retargeted,
        None => Admitted::Joined,
    }
}

/// Folds a later pass's destinations into the flight's; latest frame per window wins.
pub(in crate::transitions) fn merge_final_frames(
    existing: &mut Vec<(WindowId, CGRect)>,
    incoming: Vec<(WindowId, CGRect)>,
) -> bool {
    let mut changed = false;
    for (window, frame) in incoming {
        if let Some(current) = existing.iter_mut().find(|(w, _)| *w == window) {
            if !current.1.same_as(frame) {
                changed = true;
            }
            current.1 = frame;
        } else {
            existing.push((window, frame));
            changed = true;
        }
    }
    changed
}

/// Points a flight's reserved entrances at a later pass's destinations; an entrance has no tile
/// for `merge_pass` to retarget. See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn retarget_entrances(
    entrances: &mut [PendingEntrance],
    final_frames: &[(WindowId, CGRect)],
    display: CGRect,
) -> usize {
    let mut moved = 0;
    for entrance in entrances.iter_mut() {
        let Some((_, frame)) = final_frames.iter().find(|(w, _)| *w == entrance.window) else {
            continue;
        };
        // A slot is never a park, so the frame is aimed at directly.
        let to = to_overlay_space(*frame, display);
        if !entrance.to.same_as(to) {
            entrance.to = to;
            moved += 1;
        }
    }
    moved
}

/// The frames a coalescing merge must send again: `step` will not place frame-zero frames twice.
/// See "The reservation fallback" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn reapply_set(
    frames_applied: bool,
    in_flight: bool,
    changed: bool,
    final_frames: &[(WindowId, CGRect)],
) -> Option<Vec<(WindowId, CGRect)>> {
    (frames_applied && !in_flight && changed).then(|| final_frames.to_vec())
}

/// A newly opened window's reservation, and the hold entry it adds to `awaiting`.
pub(in crate::transitions) fn entrance_reservation(
    window: WindowId,
    to: CGRect,
    floating: bool,
) -> (PendingEntrance, (WindowId, CGSize)) {
    (PendingEntrance { window, to, floating }, (window, to.size))
}

/// Whether a flight that does not hold chases the real picture of a window flying as the stand-in:
/// when the window lands on this display, where a framed capture sees it once its frame is applied.
/// One parked at both ends has nothing on screen to capture.
pub(in crate::transitions) fn chases_stand_in(destination: CGRect, display: CGRect) -> bool {
    !rini_geometry::is_off_screen(display, destination)
}

/// How a newly opened window enters a flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::transitions) enum EntranceDecision {
    /// Its tile travels from the frame macOS showed it at to its slot.
    Travel { from: CGRect, to: CGRect },
    /// No usable picture at spawn: a reservation held for the first picture, with the reason.
    Reserve(&'static str),
}

/// `Travel` iff the server reports a sized frame, the spawn capture is usable and budget remains.
pub(in crate::transitions) fn entrance_plan(
    spawn: Option<CGRect>,
    slot: CGRect,
    picture_usable: bool,
    budget_left: bool,
) -> EntranceDecision {
    let Some(from) = spawn else {
        return EntranceDecision::Reserve("no server frame");
    };
    if from.size.width <= 0.0 || from.size.height <= 0.0 {
        return EntranceDecision::Reserve("zero server frame");
    }
    if !budget_left {
        return EntranceDecision::Reserve("capture budget");
    }
    if !picture_usable {
        return EntranceDecision::Reserve("capture unusable");
    }
    EntranceDecision::Travel { from, to: slot }
}

/// What a fresh flight does at frame zero: whether it holds, which windows the chase follows, and
/// which frames go out now (all when holding, else the newcomers' slots so the chase can capture).
pub(in crate::transitions) fn frame_zero_work(
    awaiting: &[(WindowId, CGSize)],
    chase: &[(WindowId, CGSize)],
    final_frames: &[(WindowId, CGRect)],
    entrance_frames: &[(WindowId, CGRect)],
) -> (bool, Vec<(WindowId, CGSize)>, Vec<(WindowId, CGRect)>) {
    let holding = !awaiting.is_empty();
    let mut chase_set = awaiting.to_vec();
    for entry in chase {
        if !chase_set.iter().any(|(w, _)| *w == entry.0) {
            chase_set.push(*entry);
        }
    }
    let now_frames = if holding {
        final_frames.to_vec()
    } else {
        entrance_frames.to_vec()
    };
    (holding, chase_set, now_frames)
}

/// A move's nudge arriving at a flight that does not move yet, kept with the move's plain duration
/// until it does (`NudgeAdmission::Wait`); `None` when one still playing skips it.
pub(in crate::transitions) fn nudge_waiting(
    nudge: Option<CGPoint>,
    plain: Duration,
    playing: &NudgePlaying,
    now: Instant,
) -> Option<(CGPoint, Duration)> {
    let nudge = nudge?;
    if nudge_admission(playing, now, false) == NudgeAdmission::Wait {
        Some((nudge, plain))
    } else {
        debug!("move nudge skipped: one is still playing");
        None
    }
}

/// A move's nudge reaching a flight that moves, with the stretched duration it plays over; `None`
/// when one still playing skips it.
pub(in crate::transitions) fn starting_nudge(
    nudge: Option<(CGPoint, Duration)>,
    playing: &NudgePlaying,
    now: Instant,
) -> Option<(CGPoint, Duration)> {
    let (nudge, plain) = nudge?;
    if nudge_admission(playing, now, true) == NudgeAdmission::Start {
        Some((nudge, plain.mul_f64(NUDGE_STRETCH)))
    } else {
        debug!("move nudge skipped: one is still playing");
        None
    }
}

/// The bounce a push at `now` plays: from rest with none playing, else pushed from where the one
/// playing is drawn.
pub(in crate::transitions) fn next_bounce(
    playing: Option<(Instant, Bounce)>,
    now: Instant,
    overshoot: CGPoint,
    duration: Duration,
) -> Bounce {
    match playing {
        Some((began, bounce)) => {
            bounce.pushed(now.saturating_duration_since(began), overshoot, duration)
        }
        None => Bounce::from_rest(overshoot, duration),
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGPoint;

    use super::*;
    use crate::animation::domain::motion::testing::{Gen, RUNS};

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn window(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    /// A pass that repeats a destination is not a new flight. This is what stops a held key from
    /// restarting the animation on every repeat, which would mean it never lands.
    #[test]
    fn the_same_destination_again_is_redundant() {
        let to = rect(0., 0., 100., 100.);
        assert_eq!(merge_action(Some(to), to), Admitted::Redundant);
    }

    #[test]
    fn a_new_destination_for_a_moving_window_retargets_it() {
        let flying_to = rect(0., 0., 100., 100.);
        let now_to = rect(500., 0., 100., 100.);
        assert_eq!(merge_action(Some(flying_to), now_to), Admitted::Retargeted);
    }

    #[test]
    fn a_window_the_flight_has_not_seen_joins_it() {
        assert_eq!(merge_action(None, rect(0., 0., 100., 100.)), Admitted::Joined);
    }

    /// `same_as` rather than `==`: a destination that differs only by floating-point noise is the
    /// same destination, and treating it as a retarget would bend a tile toward where it already is.
    #[test]
    fn a_rounding_difference_is_still_the_same_destination() {
        let to = rect(10., 20., 100., 100.);
        let nudged = rect(10.000000001, 20., 100., 100.);
        assert_eq!(merge_action(Some(to), nudged), Admitted::Redundant);
    }

    #[test]
    fn a_later_pass_overwrites_a_windows_destination_and_reports_the_change() {
        let mut frames = vec![(window(1), rect(0., 0., 100., 100.))];
        let changed =
            merge_final_frames(&mut frames, vec![(window(1), rect(500., 0., 100., 100.))]);
        assert!(changed);
        assert_eq!(frames, [(window(1), rect(500., 0., 100., 100.))]);
    }

    /// Latest frame per window wins, and a repeat is not a change. The caller uses the flag to decide
    /// whether to resend frames, so reporting a change that did not happen costs a round of writes.
    #[test]
    fn repeating_a_destination_reports_no_change() {
        let to = rect(0., 0., 100., 100.);
        let mut frames = vec![(window(1), to)];
        assert!(!merge_final_frames(&mut frames, vec![(window(1), to)]));
        assert_eq!(frames, [(window(1), to)]);
    }

    #[test]
    fn a_window_not_in_the_flight_is_appended_as_a_change() {
        let mut frames = vec![(window(1), rect(0., 0., 100., 100.))];
        let changed =
            merge_final_frames(&mut frames, vec![(window(2), rect(200., 0., 100., 100.))]);
        assert!(changed);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1], (window(2), rect(200., 0., 100., 100.)));
    }

    #[test]
    fn nothing_incoming_changes_nothing() {
        let mut frames = vec![(window(1), rect(0., 0., 100., 100.))];
        assert!(!merge_final_frames(&mut frames, vec![]));
    }

    /// An entrance has no tile yet, so `merge_pass` cannot retarget it; this is the path that does.
    /// The destination is converted into the overlay's space, which is what the display origin is for.
    #[test]
    fn a_reserved_entrance_is_retargeted_into_overlay_space() {
        let display = rect(1000., 500., 1000., 800.);
        let mut entrances = vec![PendingEntrance {
            window: window(1),
            to: rect(0., 0., 100., 100.),
            floating: false,
        }];

        let moved = retarget_entrances(
            &mut entrances,
            &[(window(1), rect(1200., 600., 50., 50.))],
            display,
        );

        assert_eq!(moved, 1);
        assert_eq!(
            entrances[0].to,
            rect(200., 100., 50., 50.),
            "the display origin is subtracted, so the overlay draws at its own coordinates"
        );
    }

    #[test]
    fn an_entrance_the_pass_does_not_mention_is_left_alone() {
        let display = rect(0., 0., 1000., 800.);
        let to = rect(10., 10., 100., 100.);
        let mut entrances = vec![PendingEntrance {
            window: window(1),
            to,
            floating: false,
        }];

        let moved =
            retarget_entrances(&mut entrances, &[(window(2), rect(500., 0., 50., 50.))], display);

        assert_eq!(moved, 0);
        assert_eq!(entrances[0].to, to);
    }

    #[test]
    fn retargeting_an_entrance_to_where_it_already_points_moves_nothing() {
        let display = rect(0., 0., 1000., 800.);
        let frame = rect(10., 10., 100., 100.);
        let mut entrances = vec![PendingEntrance {
            window: window(1),
            to: frame,
            floating: false,
        }];

        assert_eq!(
            retarget_entrances(&mut entrances, &[(window(1), frame)], display),
            0
        );
    }

    /// The reapply set exists for one case: a coalescing merge after frame zero has already gone out.
    /// `step` will not place frame-zero frames twice, so a merge that changed something has to resend.
    #[test]
    fn frames_are_resent_only_after_they_went_out_and_something_changed() {
        let frames = [(window(1), rect(0., 0., 100., 100.))];
        assert_eq!(reapply_set(true, false, true, &frames), Some(frames.to_vec()));
    }

    #[test]
    fn frames_that_never_went_out_are_not_resent() {
        let frames = [(window(1), rect(0., 0., 100., 100.))];
        assert_eq!(reapply_set(false, false, true, &frames), None);
    }

    /// A flight still in the air will place them itself.
    #[test]
    fn frames_are_not_resent_while_the_flight_is_running() {
        let frames = [(window(1), rect(0., 0., 100., 100.))];
        assert_eq!(reapply_set(true, true, true, &frames), None);
    }

    #[test]
    fn nothing_changing_resends_nothing() {
        let frames = [(window(1), rect(0., 0., 100., 100.))];
        assert_eq!(reapply_set(true, false, false, &frames), None);
    }

    #[test]
    fn a_reservation_holds_the_flight_for_the_window_it_reserves() {
        let to = rect(100., 0., 300., 400.);
        let (entrance, hold) = entrance_reservation(window(9), to, true);
        assert_eq!(entrance.window, window(9));
        assert_eq!(entrance.to, to);
        assert!(entrance.floating);
        assert_eq!(
            hold,
            (window(9), to.size),
            "the hold carries the size to wait for"
        );
    }

    #[test]
    fn a_window_with_a_usable_picture_and_a_real_frame_travels() {
        let spawn = rect(0., 0., 200., 200.);
        let slot = rect(500., 0., 400., 600.);
        assert_eq!(
            entrance_plan(Some(spawn), slot, true, true),
            EntranceDecision::Travel { from: spawn, to: slot }
        );
    }

    /// Each refusal names itself, because "the window appeared without animating" has four causes and
    /// the log is the only way to tell them apart afterwards.
    #[test]
    fn every_reason_to_reserve_says_which_one_it_was() {
        let slot = rect(500., 0., 400., 600.);
        let spawn = rect(0., 0., 200., 200.);
        assert_eq!(
            entrance_plan(None, slot, true, true),
            EntranceDecision::Reserve("no server frame")
        );
        assert_eq!(
            entrance_plan(Some(rect(0., 0., 0., 200.)), slot, true, true),
            EntranceDecision::Reserve("zero server frame")
        );
        assert_eq!(
            entrance_plan(Some(spawn), slot, true, false),
            EntranceDecision::Reserve("capture budget")
        );
        assert_eq!(
            entrance_plan(Some(spawn), slot, false, true),
            EntranceDecision::Reserve("capture unusable")
        );
    }

    /// A zero-height frame reserves as surely as a zero-width one: macOS reports both for a window it
    /// has not laid out yet, and a tile scaled from either is a division by zero.
    #[test]
    fn a_frame_with_no_height_reserves_too() {
        let slot = rect(500., 0., 400., 600.);
        assert_eq!(
            entrance_plan(Some(rect(0., 0., 200., 0.)), slot, true, true),
            EntranceDecision::Reserve("zero server frame")
        );
    }

    /// The budget is checked BEFORE the picture, so a flight that has spent its captures says so
    /// rather than blaming the picture it never took.
    #[test]
    fn a_spent_capture_budget_is_reported_ahead_of_an_unusable_picture() {
        let slot = rect(500., 0., 400., 600.);
        let spawn = rect(0., 0., 200., 200.);
        assert_eq!(
            entrance_plan(Some(spawn), slot, false, false),
            EntranceDecision::Reserve("capture budget")
        );
    }

    /// Holding at frame zero means every frame goes out now, because the flight is not going to place
    /// them: it is waiting for a picture.
    #[test]
    fn a_flight_holding_for_an_entrance_sends_every_frame_immediately() {
        let awaiting = [(window(1), CGSize::new(100., 100.))];
        let finals = [
            (window(1), rect(0., 0., 100., 100.)),
            (window(2), rect(200., 0., 100., 100.)),
        ];
        let entrances = [(window(1), rect(0., 0., 100., 100.))];

        let (holding, chase, now) = frame_zero_work(&awaiting, &[], &finals, &entrances);

        assert!(holding);
        assert_eq!(chase, awaiting.to_vec());
        assert_eq!(now, finals.to_vec(), "all of them, not just the newcomers");
    }

    /// Not holding means only the newcomers' slots go out, so the chase has something to capture
    /// while the rest of the flight animates normally.
    #[test]
    fn a_flight_with_nothing_to_wait_for_sends_only_the_newcomers() {
        let finals = [
            (window(1), rect(0., 0., 100., 100.)),
            (window(2), rect(200., 0., 100., 100.)),
        ];
        let entrances = [(window(2), rect(200., 0., 100., 100.))];

        let (holding, _, now) =
            frame_zero_work(&[], &[(window(2), CGSize::new(100., 100.))], &finals, &entrances);

        assert!(!holding);
        assert_eq!(now, entrances.to_vec());
    }

    /// The chase set is the awaited windows plus the chased ones, without repeating a window that is
    /// both. A duplicate would have the capture service chase one window twice.
    #[test]
    fn the_chase_set_does_not_repeat_a_window_that_is_both_awaited_and_chased() {
        let awaiting = [(window(1), CGSize::new(100., 100.))];
        let chase = [
            (window(1), CGSize::new(100., 100.)),
            (window(2), CGSize::new(50., 50.)),
        ];

        let (_, chase_set, _) = frame_zero_work(&awaiting, &chase, &[], &[]);

        assert_eq!(
            chase_set,
            [
                (window(1), CGSize::new(100., 100.)),
                (window(2), CGSize::new(50., 50.))
            ]
        );
    }

    #[test]
    fn a_flight_with_no_entrances_at_all_holds_for_nothing() {
        let (holding, chase, now) = frame_zero_work(&[], &[], &[], &[]);
        assert!(!holding);
        assert!(chase.is_empty());
        assert!(now.is_empty());
    }

    /// The column a move at the edge passes comes in from its park with no picture: its real
    /// picture is chased once it is on screen. One parked at both ends is not, nor one on another
    /// display.
    #[test]
    fn a_stand_in_landing_on_this_display_is_chased() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        assert!(chases_stand_in(rect(868.0, 32.0, 856.0, 1081.0), display));
        assert!(
            chases_stand_in(rect(-400.0, 32.0, 856.0, 1081.0), display),
            "partly on"
        );
        assert!(!chases_stand_in(rect(1727.0, 1085.0, 1720.0, 1081.0), display));
        assert!(!chases_stand_in(rect(-3446.0, 32.0, 1720.0, 1081.0), display));
    }

    /// The 0.1pt tolerance is `same_as`'s: the layout recomputes destinations bit-for-bit only most
    /// of the time.
    #[test]
    fn a_pass_confirming_the_destination_is_redundant() {
        let to = rect(4.0, 32.0, 859.0, 1081.0);
        let confirming = rect(4.05, 32.0, 859.0, 1081.0);
        assert_eq!(merge_action(Some(to), confirming), Admitted::Redundant);
    }

    #[test]
    fn a_new_destination_retargets_and_a_new_window_joins() {
        let to = rect(4.0, 32.0, 859.0, 1081.0);
        assert_eq!(
            merge_action(Some(to), rect(865.0, 32.0, 859.0, 1081.0)),
            Admitted::Retargeted
        );
        assert_eq!(merge_action(None, to), Admitted::Joined);
    }

    #[test]
    fn a_coalescing_merge_re_requests_frames_it_already_applied() {
        let moved = window(1);
        let a = rect(1727.0, 1116.0, 859.0, 1081.0);
        let b = rect(867.0, 32.0, 859.0, 1081.0);
        let mut final_frames = vec![(moved, a)];

        let changed = merge_final_frames(&mut final_frames, vec![(moved, b)]);
        let reapply = reapply_set(true, false, changed, &final_frames);

        assert!(changed, "B differs from A");
        assert_eq!(final_frames, vec![(moved, b)], "latest frame wins");
        assert_eq!(
            reapply,
            Some(vec![(moved, b)]),
            "frames applied early and then merged must be re-requested; got {reapply:?}"
        );
    }

    #[test]
    fn a_merge_before_frames_were_applied_re_requests_nothing() {
        let mut rng = Gen(35);
        for _ in 0..RUNS {
            let current = rng.on_screen();
            let mut final_frames = vec![(window(1), current)];
            let incoming = if rng.coin() { rng.on_screen() } else { current };
            let action = merge_action(Some(current), incoming);
            let expected = if current.same_as(incoming) {
                Admitted::Redundant
            } else {
                Admitted::Retargeted
            };
            assert_eq!(action, expected);
            let changed =
                merge_final_frames(&mut final_frames, vec![(window(1), incoming)]);
            assert_eq!(changed, action == Admitted::Retargeted);
            assert_eq!(
                final_frames,
                vec![(window(1), incoming)],
                "latest frame wins"
            );
            for in_flight in [false, true] {
                assert_eq!(
                    reapply_set(false, in_flight, changed, &final_frames),
                    None
                );
            }
        }
        assert_eq!(
            merge_action(None, rect(4.0, 32.0, 859.0, 1081.0)),
            Admitted::Joined
        );
    }

    #[test]
    fn a_coalescing_merge_re_requests_exactly_the_merged_frames() {
        let mut rng = Gen(63);
        let mut re_requested = 0;
        for _ in 0..RUNS {
            let count = rng.below(4) as u32 + 1;
            let mut final_frames: Vec<(WindowId, CGRect)> =
                (1..=count).map(|i| (window(i), rng.on_screen())).collect();
            let passes = rng.below(3) + 1;
            let mut expected: Vec<(WindowId, CGRect)> = final_frames.clone();
            for _ in 0..passes {
                let mut incoming: Vec<(WindowId, CGRect)> = Vec::new();
                for i in 1..=count + 1 {
                    match rng.below(3) {
                        0 => continue,
                        1 => incoming.push((window(i), rng.on_screen())),
                        _ => {
                            if let Some(&(_, current)) =
                                expected.iter().find(|(w, _)| *w == window(i))
                            {
                                incoming.push((window(i), current));
                            }
                        }
                    }
                }
                let mut expected_changed = false;
                for (window, frame) in &incoming {
                    match expected.iter_mut().find(|(w, _)| w == window) {
                        Some(current) => {
                            expected_changed |= !current.1.same_as(*frame);
                            current.1 = *frame;
                        }
                        None => {
                            expected.push((*window, *frame));
                            expected_changed = true;
                        }
                    }
                }
                let changed = merge_final_frames(&mut final_frames, incoming);
                assert_eq!(changed, expected_changed, "seed 63");
                let reapply = reapply_set(true, false, changed, &final_frames);
                if changed {
                    re_requested += 1;
                    assert_eq!(
                        reapply,
                        Some(expected.clone()),
                        "seed 63: merged set, latest wins"
                    );
                } else {
                    assert_eq!(reapply, None, "seed 63: nothing changed, nothing re-requested");
                }
            }
        }
        assert!(
            re_requested > RUNS / 2,
            "generator sanity: {re_requested} re-requests"
        );
    }

    #[test]
    fn reapply_set_truth_table() {
        let frames = vec![(window(1), rect(4.0, 32.0, 859.0, 1081.0))];
        for frames_applied in [false, true] {
            for in_flight in [false, true] {
                for changed in [false, true] {
                    let expected =
                        (frames_applied && !in_flight && changed).then(|| frames.clone());
                    assert_eq!(
                        reapply_set(frames_applied, in_flight, changed, &frames),
                        expected,
                        "frames_applied={frames_applied} in_flight={in_flight} changed={changed}"
                    );
                }
            }
        }
    }

    /// The reservation always brings a hold entry of the destination's size.
    #[test]
    fn an_entrance_always_holds_at_its_destination_size() {
        let mut rng = Gen(61);
        for _ in 0..RUNS {
            let to = rng.on_screen();
            let floating = rng.coin();
            let (entrance, waiting) = entrance_reservation(window(2), to, floating);
            assert_eq!(waiting, (window(2), to.size), "seed 61: {to:?}");
            assert_eq!(entrance.to, to);
            assert_eq!(entrance.floating, floating);
        }
    }

    #[test]
    fn a_fresh_flight_with_an_entrance_applies_and_chases_at_frame_zero() {
        let mut rng = Gen(311);
        for _ in 0..RUNS {
            let to = rng.on_screen();
            let (entrance, waiting) = entrance_reservation(window(3), to, false);
            assert_eq!(entrance.window, window(3));
            assert_eq!(entrance.to, to);
            let awaiting: Vec<(WindowId, CGSize)> = vec![waiting];
            let (apply_now, chase, _) = frame_zero_work(&awaiting, &[], &[], &[]);
            assert!(apply_now);
            assert!(
                chase.contains(&(window(3), to.size)),
                "the entrance is chased: {chase:?}"
            );

            let awaiting = vec![(window(1), rng.on_screen().size)];
            let (apply_now, chase, _) = frame_zero_work(&awaiting, &[], &[], &[]);
            assert!(apply_now);
            assert_eq!(chase, awaiting);
        }
        assert_eq!(
            frame_zero_work(&[], &[], &[], &[]),
            (false, Vec::new(), Vec::new())
        );
    }

    #[test]
    fn entrance_plan_travels_from_spawn_or_reserves_with_a_reason() {
        let slot = rect(867.0, 32.0, 859.0, 1081.0);
        let spawn = rect(300.0, 200.0, 640.0, 480.0);
        assert_eq!(
            entrance_plan(Some(spawn), slot, true, true),
            EntranceDecision::Travel { from: spawn, to: slot }
        );
        assert_eq!(
            entrance_plan(None, slot, true, true),
            EntranceDecision::Reserve("no server frame")
        );
        assert_eq!(
            entrance_plan(Some(rect(300.0, 200.0, 0.0, 0.0)), slot, true, true),
            EntranceDecision::Reserve("zero server frame")
        );
        assert_eq!(
            entrance_plan(Some(spawn), slot, false, true),
            EntranceDecision::Reserve("capture unusable")
        );
        assert_eq!(
            entrance_plan(Some(spawn), slot, true, false),
            EntranceDecision::Reserve("capture budget")
        );
    }

    #[test]
    fn frame_zero_work_sends_all_frames_when_holding_and_only_slots_otherwise() {
        let slot = rect(867.0, 32.0, 859.0, 1081.0);
        let grown = rect(4.0, 32.0, 1200.0, 1081.0);
        let final_frames = vec![(window(1), grown), (window(2), slot)];
        let (holding, chase_set, now) = frame_zero_work(
            &[(window(1), grown.size)],
            &[(window(2), slot.size)],
            &final_frames,
            &[(window(2), slot)],
        );
        assert!(holding);
        assert_eq!(
            now, final_frames,
            "holding: every frame goes out under the overlay"
        );
        assert_eq!(chase_set, vec![(window(1), grown.size), (window(2), slot.size)]);

        let (holding, chase_set, now) =
            frame_zero_work(&[], &[(window(2), slot.size)], &final_frames, &[(window(2), slot)]);
        assert!(!holding);
        assert_eq!(
            now,
            vec![(window(2), slot)],
            "not holding: the newcomer's slot alone"
        );
        assert_eq!(chase_set, vec![(window(2), slot.size)]);

        let (holding, chase_set, now) = frame_zero_work(&[], &[], &final_frames, &[]);
        assert!(!holding && chase_set.is_empty() && now.is_empty());
    }

    #[test]
    fn entrance_plan_travels_exactly_when_it_can() {
        let mut rng = Gen(157);
        for run in 0..RUNS {
            let slot = rng.on_screen();
            let spawn = match rng.below(4) {
                0 => None,
                1 => Some(rect(
                    rng.pt(0.0, 1000.0),
                    rng.pt(0.0, 800.0),
                    0.0,
                    rng.pt(0.0, 500.0),
                )),
                _ => Some(rng.on_screen()),
            };
            let usable = rng.coin();
            let budget = rng.coin();
            let decision = entrance_plan(spawn, slot, usable, budget);
            let can = spawn.is_some_and(|f| f.size.width > 0.0 && f.size.height > 0.0)
                && usable
                && budget;
            match decision {
                EntranceDecision::Travel { from, to } => {
                    assert!(can, "seed 157 run {run}");
                    assert_eq!(Some(from), spawn, "seed 157 run {run}");
                    assert_eq!(to, slot, "seed 157 run {run}");
                    assert!(from.size.width > 0.0, "seed 157 run {run}");
                }
                EntranceDecision::Reserve(_) => assert!(!can, "seed 157 run {run}"),
            }
        }
    }

    mod on_another_display {
        use objc2_core_foundation::CGPoint;

        use super::*;

        /// An external display at a non-zero origin, so a retarget that forgot the conversion shows.
        const EXTERNAL: CGRect = CGRect {
            origin: CGPoint { x: 1728.0, y: -300.0 },
            size: CGSize { width: 3008.0, height: 1692.0 },
        };

        fn shifted(frame: CGRect, delta: CGPoint) -> CGRect {
            CGRect::new(
                CGPoint::new(frame.origin.x + delta.x, frame.origin.y + delta.y),
                frame.size,
            )
        }

        #[test]
        fn a_later_pass_retargets_a_reserved_entrance() {
            let slot = rect(
                EXTERNAL.origin.x + 867.0,
                EXTERNAL.origin.y + 32.0,
                859.0,
                1081.0,
            );
            let pan = CGPoint::new(-574.0, 0.0);
            let (newcomer, _) =
                entrance_reservation(window(51462), to_overlay_space(slot, EXTERNAL), false);
            let (untouched, _) = entrance_reservation(
                window(51463),
                to_overlay_space(rect(2000.0, 32.0, 400.0, 1081.0), EXTERNAL),
                false,
            );
            let mut entrances = vec![newcomer, untouched.clone()];
            let frames = vec![
                (window(1), rect(4.0, 32.0, 859.0, 1081.0)),
                (window(51462), shifted(slot, pan)),
            ];

            assert_eq!(retarget_entrances(&mut entrances, &frames, EXTERNAL), 1);
            assert_eq!(entrances[0].to, to_overlay_space(shifted(slot, pan), EXTERNAL));
            assert_eq!(entrances[1].to, untouched.to, "no frame for it: left alone");
            assert_eq!(
                retarget_entrances(&mut entrances, &frames, EXTERNAL),
                0,
                "already there"
            );
        }
    }

    /// A move's nudge waits for its flight to move, then plays stretched.
    mod nudge {
        use super::*;

        /// The reactor's step waits with the move's plain duration until the flight moves, then
        /// plays stretched. A press while one plays is skipped.
        #[test]
        fn a_nudge_waits_for_its_flight_then_plays_stretched() {
            let step = CGPoint::new(-576.0, 0.0);
            let plain = Duration::from_millis(350);
            let now = Instant::now();
            let idle = NudgePlaying::default();
            let waiting = nudge_waiting(Some(step), plain, &idle, now);
            assert_eq!(waiting, Some((step, plain)));
            let started = starting_nudge(waiting, &idle, now);
            assert_eq!(started, Some((step, plain.mul_f64(NUDGE_STRETCH))));

            let mut playing = NudgePlaying::default();
            playing.start(now, plain.mul_f64(NUDGE_STRETCH));
            let soon = now + Duration::from_millis(100);
            assert_eq!(nudge_waiting(Some(step), plain, &playing, soon), None);
            assert_eq!(starting_nudge(Some((step, plain)), &playing, soon), None);
            assert_eq!(nudge_waiting(None, plain, &idle, now), None);
        }
    }

    /// The edge bounce: every push against an end shows, re-kicked from where the last is drawn.
    mod bounce {
        use super::*;

        /// A push while a bounce plays is not dropped: it starts a bounce from where the last is
        /// drawn. One after it came home, or with none playing, starts from rest.
        #[test]
        fn every_push_at_an_end_starts_a_bounce_from_where_the_last_is_drawn() {
            let plays = Duration::from_millis(350);
            let wall = CGPoint::new(-72.0, 0.0);
            let began = Instant::now();
            let first = next_bounce(None, began, wall, plays);
            assert_eq!(first, Bounce::from_rest(wall, plays));
            let later = began + Duration::from_millis(60);
            let second = next_bounce(Some((began, first)), later, wall, plays);
            assert_eq!(second.from, first.at(Duration::from_millis(60)));
            assert_ne!(second.from, CGPoint::new(0.0, 0.0), "not over from rest");
            let home = began + plays;
            assert_eq!(
                next_bounce(Some((began, first)), home, wall, plays),
                Bounce::from_rest(wall, plays)
            );
        }
    }
}
