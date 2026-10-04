//! What happens when a request arrives while a flight is already running.
//!
//! The layout does not wait for an animation to land, so a second pass can arrive mid-flight with a
//! different destination for a window already moving, or with a window the first pass had not placed.
//! Admitting one into the other without a visible jump is what these decisions are for. A move's
//! nudge and a push against an end arrive the same way: the nudge waits for its flight to move, and a
//! push while a bounce plays starts the next from where that one is drawn, as long as it is drawn.

use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::WindowId;
use rini_geometry::{SameAs, to_overlay_space};
use tracing::debug;

use crate::animation::domain::motion::accents::{NudgeAdmission, NudgePlaying, nudge_admission};
use crate::animation::domain::motion::bounce::Bounce;
use crate::animation::domain::motion::nudge::NUDGE_STRETCH;
use crate::pictures::domain::coverage::outgrows;

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

/// How a newly opened window enters a flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::transitions) enum EntranceDecision {
    /// Its tile travels from the frame macOS showed it at to its slot.
    Travel { from: CGRect, to: CGRect },
    /// No usable picture at spawn: a reservation held for the first picture, with the reason.
    Reserve(&'static str),
}

/// How a window's picture where it ends is waited for. See "A grow holds, then reveals" and "A window
/// that opens" in `src/transitions/docs/coordination.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::transitions) enum EndPicture {
    /// Held for, overlay up, and watched until it has repainted where it ends.
    Awaited,
    /// Watched until it has repainted where it ends, and never held for.
    Repainting,
    /// Captured once it is where it ends.
    Lands,
}

impl EndPicture {
    /// A window drawn from a picture covering `covered`: a grow its picture cannot cover at `to`, or a
    /// stand-in, which covers nothing, is awaited.
    pub(in crate::transitions) fn of_picture(covered: (f64, f64), to: CGSize) -> Self {
        if outgrows(covered, to) { Self::Awaited } else { Self::Lands }
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

/// The edge bounce the containers draw, and when it began: what a push arriving while it plays
/// starts the next from. Core Animation drops the bounce at the lift and whenever frame zero is
/// installed again, and the record goes with it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(in crate::transitions) struct BounceRecord(Option<(Instant, Bounce)>);

impl BounceRecord {
    /// The bounce a push at `now` plays, from rest with none drawn, else from where the one drawn is;
    /// and the overshoot of the one it replaces, which a container that did not carry starts from
    /// rest.
    pub(in crate::transitions) fn push(
        &mut self,
        now: Instant,
        overshoot: CGPoint,
        duration: Duration,
    ) -> (Bounce, Option<CGPoint>) {
        let next = match self.0 {
            Some((began, bounce)) => {
                bounce.pushed(now.saturating_duration_since(began), overshoot, duration)
            }
            None => Bounce::from_rest(overshoot, duration),
        };
        let before = self.0.map(|(_, drawn)| drawn.overshoot);
        self.0 = Some((now, next));
        (next, before)
    }

    /// The containers no longer draw it: the overlay lifted, or frame zero was installed again.
    pub(in crate::transitions) fn gone(&mut self) {
        self.0 = None;
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

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







    /// A grow its picture cannot cover and a stand-in are held for; a picture that covers lands.
    #[test]
    fn a_grow_and_a_stand_in_are_awaited_and_a_picture_that_covers_lands() {
        let column = CGSize::new(859.0, 1081.0);
        assert_eq!(EndPicture::of_picture((859.0, 1081.0), CGSize::new(1720.0, 1081.0)), EndPicture::Awaited, "a grow");
        assert_eq!(EndPicture::of_picture((0.0, 0.0), column), EndPicture::Awaited, "a stand-in covers nothing");
        assert_eq!(EndPicture::of_picture((859.0, 1081.0), column), EndPicture::Lands);
        assert_eq!(EndPicture::of_picture((1720.0, 1081.0), column), EndPicture::Lands, "a shrink crops");
    }

    /// A window that opened flies from where it opened and repaints at its slot, never held for; one
    /// already at its slot, a cold cache rather than an open, lands; a reservation is held for.
    #[test]
    fn an_entrance_that_travels_repaints_one_standing_lands_and_a_reservation_is_awaited() {
        let slot = rect(867.0, 32.0, 859.0, 1081.0);
        let spawn = rect(400.0, 200.0, 700.0, 500.0);
        assert_eq!(EndPicture::of_entrance(EntranceDecision::Travel { from: spawn, to: slot }), EndPicture::Repainting);
        assert_eq!(EndPicture::of_entrance(EntranceDecision::Travel { from: slot, to: slot }), EndPicture::Lands);
        assert_eq!(EndPicture::of_entrance(EntranceDecision::Reserve("capture budget")), EndPicture::Awaited);
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
            let newcomer = PendingEntrance {
                window: window(51462),
                to: to_overlay_space(slot, EXTERNAL),
                floating: false,
            };
            let untouched = PendingEntrance {
                window: window(51463),
                to: to_overlay_space(rect(2000.0, 32.0, 400.0, 1081.0), EXTERNAL),
                floating: false,
            };
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

        const PLAYS: Duration = Duration::from_millis(350);
        const WALL: CGPoint = CGPoint { x: -72.0, y: 0.0 };

        /// A push while a bounce plays is not dropped: it starts a bounce from where the last is
        /// drawn. One after it came home, or with none playing, starts from rest.
        #[test]
        fn every_push_at_an_end_starts_a_bounce_from_where_the_last_is_drawn() {
            let began = Instant::now();
            let mut record = BounceRecord::default();
            let (first, before) = record.push(began, WALL, PLAYS);
            assert_eq!((first, before), (Bounce::from_rest(WALL, PLAYS), None));
            let later = began + Duration::from_millis(60);
            let (second, before) = record.push(later, WALL, PLAYS);
            assert_eq!(second.from, first.at(Duration::from_millis(60)));
            assert_ne!(second.from, CGPoint::new(0.0, 0.0), "not over from rest");
            assert_eq!(before, Some(WALL), "it replaces the one drawn");
            let home = later + second.duration;
            assert_eq!(record.push(home, WALL, PLAYS).0, Bounce::from_rest(WALL, PLAYS));
        }

        /// The lift and a frame zero installed again remove the bounce's animations, so a push
        /// while the one gone would still have played starts from rest, replacing nothing: from
        /// where the record says it was, the surface would jump.
        #[test]
        fn a_push_after_the_bounce_is_gone_starts_from_rest() {
            let began = Instant::now();
            let mut record = BounceRecord::default();
            record.push(began, WALL, PLAYS);
            record.gone();
            let (next, before) = record.push(began + Duration::from_millis(60), WALL, PLAYS);
            assert_eq!((next, before), (Bounce::from_rest(WALL, PLAYS), None));
        }
    }
}
