//! What happens when a request arrives while a flight is already running.
//!
//! The layout does not wait for an animation to land, so a second pass can arrive mid-flight with a
//! different destination for a window already moving, or with a window the first pass had not placed.
//! Admitting one into the other without a visible jump is what these decisions are for.

use rini_core::ids::WindowId;
use rini_geometry::space::{OverlayRect, ScreenRect, ScreenSize};
use rini_geometry::stretch::{grows, is_a_resize};
use rini_geometry::{SameAs, to_overlay_space};

use crate::decorations::domain::style::Worn;
use crate::pictures::domain::route::Route;

/// A window that joins the animation as soon as it has a picture. The flight holds at frame zero
/// for it. See "The reservation fallback" in `src/transitions/docs/coordination.md`.
#[derive(Debug, Clone)]
pub(in crate::transitions) struct PendingEntrance {
    pub(in crate::transitions) window: WindowId,
    /// Destination.
    pub(in crate::transitions) to: OverlayRect,
    pub(in crate::transitions) floating: bool,
    pub(in crate::transitions) worn: Worn,
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
    current_to: Option<OverlayRect>,
    incoming_to: OverlayRect,
) -> Admitted {
    match current_to {
        Some(to) if to.same_as(incoming_to) => Admitted::Redundant,
        Some(_) => Admitted::Retargeted,
        None => Admitted::Joined,
    }
}

/// Points a flight's reserved entrances at a later pass's destinations; an entrance has no tile
/// for `merge_pass` to retarget. See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn retarget_entrances(
    entrances: &mut [PendingEntrance],
    final_frames: &[(WindowId, ScreenRect)],
    display: ScreenRect,
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

/// How a newly opened window enters a flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::transitions) enum EntranceDecision {
    /// Its tile travels from the frame macOS showed it at to its slot.
    Travel { from: ScreenRect, to: ScreenRect },
    /// No usable picture at spawn: a reservation held for the first picture, with the reason.
    Reserve(&'static str),
}

/// How a window's picture where it ends is waited for. See "A grow waits for its repaint" and "A
/// window that opens" in `src/transitions/docs/coordination.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::transitions) enum EndPicture {
    /// No picture of the window: held for, overlay up, and watched until it has repainted where it
    /// ends.
    Awaited,
    /// A grow: held for, overlay up, watched until it has repainted where it ends, and pictured
    /// there only once it has.
    Held,
    /// Watched until it has repainted where it ends, never held for, and pictured there only once it
    /// has.
    Repainting,
    /// Captured once it is where it ends.
    Lands,
}

impl EndPicture {
    /// A window going from `from` to `to`, drawn from a picture taken `source`: a stand-in, which is
    /// no picture of the window, is awaited; a resize growing on either axis is held for its repaint;
    /// one that only shrinks flies the picture it has at once and repaints where it ends; a move
    /// lands.
    pub(in crate::transitions) fn of_window(source: Route, from: ScreenSize, to: ScreenSize) -> Self {
        if source == Route::StandIn {
            Self::Awaited
        } else if grows(from, to) {
            Self::Held
        } else if is_a_resize(from, to) {
            Self::Repainting
        } else {
            Self::Lands
        }
    }

    /// Whether the overlay holds the motion for it.
    pub(in crate::transitions) fn holds(self) -> bool {
        matches!(self, Self::Awaited | Self::Held)
    }

    /// Whether it is pictured where it ends only once it has repainted there.
    pub(in crate::transitions) fn repaints(self) -> bool {
        matches!(self, Self::Held | Self::Repainting)
    }

    /// A window that just opened: one flying from where it opened repaints where it ends, one already
    /// at its slot lands there, and a reservation is awaited.
    pub(in crate::transitions) fn of_entrance(entrance: EntranceDecision) -> Self {
        match entrance {
            EntranceDecision::Travel { from, to } if from.same_as(to) => Self::Lands,
            EntranceDecision::Travel { .. } => Self::Repainting,
            EntranceDecision::Reserve(_) => Self::Awaited,
        }
    }
}

/// `Travel` iff the server reports a sized frame, the spawn capture is usable and budget remains.
pub(in crate::transitions) fn entrance_plan(
    spawn: Option<ScreenRect>,
    slot: ScreenRect,
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

#[cfg(test)]
mod tests {
    use rini_geometry::space::{ScreenPoint, ScreenVector};

    use super::*;
    use crate::animation::domain::motion::testing::{Gen, RUNS, rect};

    fn window(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    /// A pass that repeats a destination is not a new flight. This is what stops a held key from
    /// restarting the animation on every repeat, which would mean it never lands.
    #[test]
    fn the_same_destination_again_is_redundant() {
        let to: OverlayRect = rect(0., 0., 100., 100.);
        assert_eq!(merge_action(Some(to), to), Admitted::Redundant);
    }

    #[test]
    fn a_new_destination_for_a_moving_window_retargets_it() {
        let flying_to: OverlayRect = rect(0., 0., 100., 100.);
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
        let to: OverlayRect = rect(10., 20., 100., 100.);
        let nudged = rect(10.000000001, 20., 100., 100.);
        assert_eq!(merge_action(Some(to), nudged), Admitted::Redundant);
    }

    /// An entrance has no tile yet, so `merge_pass` cannot retarget it; this is the path that does.
    /// The destination is converted into the overlay's space, which is what the display origin is for.
    #[test]
    fn a_reserved_entrance_is_retargeted_into_overlay_space() {
        let display = rect(1000., 500., 1000., 800.);
        let mut entrances = vec![PendingEntrance {
            worn: Default::default(),
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
        let to: OverlayRect = rect(10., 10., 100., 100.);
        let mut entrances = vec![PendingEntrance {
            worn: Default::default(),
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
            worn: Default::default(),
            window: window(1),
            to: to_overlay_space(frame, display),
            floating: false,
        }];

        assert_eq!(
            retarget_entrances(&mut entrances, &[(window(1), frame)], display),
            0
        );
    }






    #[test]
    fn a_window_with_a_usable_picture_and_a_real_frame_travels() {
        let spawn: ScreenRect = rect(0., 0., 200., 200.);
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
        let slot: ScreenRect = rect(500., 0., 400., 600.);
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
        let slot: ScreenRect = rect(500., 0., 400., 600.);
        assert_eq!(
            entrance_plan(Some(rect(0., 0., 200., 0.)), slot, true, true),
            EntranceDecision::Reserve("zero server frame")
        );
    }

    /// The budget is checked BEFORE the picture, so a flight that has spent its captures says so
    /// rather than blaming the picture it never took.
    #[test]
    fn a_spent_capture_budget_is_reported_ahead_of_an_unusable_picture() {
        let slot: ScreenRect = rect(500., 0., 400., 600.);
        let spawn = rect(0., 0., 200., 200.);
        assert_eq!(
            entrance_plan(Some(spawn), slot, false, false),
            EntranceDecision::Reserve("capture budget")
        );
    }






    /// The 0.1pt tolerance is `same_as`'s: the layout recomputes destinations bit-for-bit only most
    /// of the time.
    #[test]
    fn a_pass_confirming_the_destination_is_redundant() {
        let to: OverlayRect = rect(4.0, 32.0, 859.0, 1081.0);
        let confirming = rect(4.05, 32.0, 859.0, 1081.0);
        assert_eq!(merge_action(Some(to), confirming), Admitted::Redundant);
    }

    #[test]
    fn a_new_destination_retargets_and_a_new_window_joins() {
        let to: OverlayRect = rect(4.0, 32.0, 859.0, 1081.0);
        assert_eq!(
            merge_action(Some(to), rect(865.0, 32.0, 859.0, 1081.0)),
            Admitted::Retargeted
        );
        assert_eq!(merge_action(None, to), Admitted::Joined);
    }







    const COLUMN: ScreenSize = ScreenSize::new(859.0, 1081.0);

    fn end(from: ScreenSize, to: ScreenSize) -> EndPicture { EndPicture::of_window(Route::SkyLight, from, to) }

    /// A grow is held for its repaint, a fold that widens a window as it shortens it included.
    #[test]
    fn a_grow_is_held_for_its_repaint() {
        assert_eq!(end(COLUMN, ScreenSize::new(1720.0, 1081.0)), EndPicture::Held, "a grow");
        assert_eq!(end(COLUMN, ScreenSize::new(1148.0, 538.0)), EndPicture::Held, "a fold");
    }

    #[test]
    fn a_shrink_repaints_unheld() {
        assert_eq!(end(COLUMN, ScreenSize::new(859.0, 535.0)), EndPicture::Repainting);
        assert_eq!(end(ScreenSize::new(1720.0, 1081.0), COLUMN), EndPicture::Repainting);
    }

    #[test]
    fn a_move_lands_a_point_of_rounding_included() {
        assert_eq!(end(COLUMN, COLUMN), EndPicture::Lands, "a move");
        assert_eq!(end(COLUMN, ScreenSize::new(860.0, 1081.0)), EndPicture::Lands, "rounding");
        assert_eq!(EndPicture::of_window(Route::Baked, COLUMN, COLUMN), EndPicture::Lands);
    }

    #[test]
    fn a_stand_in_is_awaited_whatever_it_does() {
        for to in [COLUMN, ScreenSize::new(1720.0, 1081.0), ScreenSize::new(859.0, 535.0)] {
            assert_eq!(EndPicture::of_window(Route::StandIn, COLUMN, to), EndPicture::Awaited);
        }
    }

    /// Held for: a window with no picture and a grow. Pictured only once repainted: a grow and a
    /// window that repaints unheld.
    #[test]
    fn a_grow_is_both_held_for_and_pictured_only_once_repainted() {
        use EndPicture::*;
        let held: Vec<_> = [Awaited, Held, Repainting, Lands].into_iter().filter(|e| e.holds()).collect();
        let repaints: Vec<_> = [Awaited, Held, Repainting, Lands].into_iter().filter(|e| e.repaints()).collect();
        assert_eq!(held, [Awaited, Held]);
        assert_eq!(repaints, [Held, Repainting]);
    }

    /// A window that opened flies from where it opened and repaints at its slot, never held for; one
    /// already at its slot, a cold cache rather than an open, lands; a reservation is held for.
    #[test]
    fn an_entrance_that_travels_repaints_one_standing_lands_and_a_reservation_is_awaited() {
        let slot: ScreenRect = rect(867.0, 32.0, 859.0, 1081.0);
        let spawn = rect(400.0, 200.0, 700.0, 500.0);
        assert_eq!(EndPicture::of_entrance(EntranceDecision::Travel { from: spawn, to: slot }), EndPicture::Repainting);
        assert_eq!(EndPicture::of_entrance(EntranceDecision::Travel { from: slot, to: slot }), EndPicture::Lands);
        assert_eq!(EndPicture::of_entrance(EntranceDecision::Reserve("capture budget")), EndPicture::Awaited);
    }

    #[test]
    fn entrance_plan_travels_from_spawn_or_reserves_with_a_reason() {
        let slot: ScreenRect = rect(867.0, 32.0, 859.0, 1081.0);
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
    fn entrance_plan_travels_exactly_when_it_can() {
        let mut rng = Gen(157);
        for run in 0..RUNS {
            let slot: ScreenRect = rng.on_screen();
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
        use rini_geometry::space::Size;

        use super::*;

        /// An external display at a non-zero origin, so a retarget that forgot the conversion shows.
        const EXTERNAL: ScreenRect =
            ScreenRect::new(ScreenPoint::new(1728.0, -300.0), Size::new(3008.0, 1692.0));

        #[test]
        fn a_later_pass_retargets_a_reserved_entrance() {
            let slot = rect(
                EXTERNAL.origin.x + 867.0,
                EXTERNAL.origin.y + 32.0,
                859.0,
                1081.0,
            );
            let pan = ScreenVector::new(-574.0, 0.0);
            let newcomer = PendingEntrance {
                worn: Default::default(),
                window: window(51462),
                to: to_overlay_space(slot, EXTERNAL),
                floating: false,
            };
            let untouched = PendingEntrance {
                worn: Default::default(),
                window: window(51463),
                to: to_overlay_space(rect(2000.0, 32.0, 400.0, 1081.0), EXTERNAL),
                floating: false,
            };
            let mut entrances = vec![newcomer, untouched.clone()];
            let frames = vec![
                (window(1), rect(4.0, 32.0, 859.0, 1081.0)),
                (window(51462), slot.translate(pan)),
            ];

            assert_eq!(retarget_entrances(&mut entrances, &frames, EXTERNAL), 1);
            assert_eq!(entrances[0].to, to_overlay_space(slot.translate(pan), EXTERNAL));
            assert_eq!(entrances[1].to, untouched.to, "no frame for it: left alone");
            assert_eq!(
                retarget_entrances(&mut entrances, &frames, EXTERNAL),
                0,
                "already there"
            );
        }
    }
}
