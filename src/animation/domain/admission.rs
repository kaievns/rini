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
