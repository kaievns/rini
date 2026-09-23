//! What happens when a request arrives while a flight is already running.
//!
//! The layout does not wait for an animation to land, so a second pass can arrive mid-flight with a
//! different destination for a window already moving, or with a window the first pass had not placed.
//! Admitting one into the other without a visible jump is what these decisions are for.

use objc2_core_foundation::{CGRect, CGSize};

use rini_core::ids::WindowId;
use rini_geometry::SameAs;

use crate::animation::domain::motion::surface::to_overlay_space;

/// A window that joins the animation as soon as it has a picture. The flight holds at frame zero
/// for it. See "The reservation fallback" in `src/animation/docs/animation-smoothness.md`.
#[derive(Debug, Clone)]
pub(in crate::animation) struct PendingEntrance {
    pub(in crate::animation) window: WindowId,
    /// Destination, in the overlay's coordinate space.
    pub(in crate::animation) to: CGRect,
    pub(in crate::animation) floating: bool,
}

/// Where an entering window grows in from: zero width at its own left edge, full height.
pub(in crate::animation) fn entrance_from(to: CGRect) -> CGRect {
    CGRect::new(to.origin, CGSize::new(0.0, to.size.height))
}

/// What became of a tile offered to an animation in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::animation) enum Admitted {
    /// Same window, same destination: a redundant pass. Nothing restarts, so rapid presses
    /// neither restart nor extend the flight.
    Redundant,
    /// Same window, new destination: the tile bends toward it mid-flight.
    Retargeted,
    /// A window this animation had not seen yet.
    Joined,
}

/// The merge decision for one tile.
pub(in crate::animation) fn merge_action(
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
pub(in crate::animation) fn merge_final_frames(
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
/// for `merge_pass` to retarget. See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) fn retarget_entrances(
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
/// See "The reservation fallback" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) fn reapply_set(
    frames_applied: bool,
    in_flight: bool,
    changed: bool,
    final_frames: &[(WindowId, CGRect)],
) -> Option<Vec<(WindowId, CGRect)>> {
    (frames_applied && !in_flight && changed).then(|| final_frames.to_vec())
}

/// A newly opened window's reservation, and the hold entry it adds to `awaiting`.
pub(in crate::animation) fn entrance_reservation(
    window: WindowId,
    to: CGRect,
    floating: bool,
) -> (PendingEntrance, Option<(WindowId, CGSize)>) {
    (PendingEntrance { window, to, floating }, Some((window, to.size)))
}

/// How a newly opened window enters a flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::animation) enum EntranceDecision {
    /// Its tile travels from the frame macOS showed it at to its slot.
    Travel { from: CGRect, to: CGRect },
    /// No usable picture at spawn: a reservation held for the first picture, with the reason.
    Reserve(&'static str),
}

/// `Travel` iff the server reports a sized frame, the spawn capture is usable and budget remains.
pub(in crate::animation) fn entrance_plan(
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
pub(in crate::animation) fn frame_zero_work(
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

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGPoint;

    use super::*;

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

    /// An entering window grows from zero width at its own left edge, so it unfurls rightward from
    /// where it will sit rather than sliding in from somewhere else.
    #[test]
    fn an_entrance_grows_from_no_width_at_its_own_left_edge() {
        let to = rect(300., 50., 400., 600.);
        let from = entrance_from(to);
        assert_eq!(from.origin, to.origin);
        assert_eq!(from.size.width, 0.0);
        assert_eq!(from.size.height, to.size.height, "full height from the start");
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
            Some((window(9), to.size)),
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
}
