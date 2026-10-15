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
use crate::pictures::domain::coverage::fits_frame;
use crate::pictures::domain::route::Route;
use crate::transitions::domain::lifecycle::frame_zero_pictures;
use crate::transitions::domain::request::AnimationRequest;

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
    /// A shrink whose tile starts with a picture covering its start: watched until it has repainted
    /// where it ends, never held for itself, and its tile keeps that picture, which covers every
    /// frame, to the lift (`Flight::keeps`).
    Kept,
    /// Watched until it has repainted where it ends, never held for, and pictured there only once it
    /// has: an entrance flying from where it opened, and a shrink whose tile starts with no picture
    /// covering its start.
    Repainting,
    /// Captured once it is where it ends.
    Lands,
}

/// What a pass's window flies on: the picture it has, taken by a `Route` and `covered` big, or,
/// having none, its entrance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::transitions) enum FliesOn {
    Picture { source: Route, covered: ScreenSize },
    Entrance(EntranceDecision),
}

impl FliesOn {
    /// How big the tile of a window flying so ends, its path ending it `end` big, which a pass
    /// merged into the flight running judges it at (`Landing::to`): a picture at `end`, a travelling
    /// entrance at its slot, and a reserved entrance, which has no tile, at none.
    pub(in crate::transitions) fn ends_at(self, end: ScreenSize) -> Option<ScreenSize> {
        match self {
            Self::Picture { .. } => Some(end),
            Self::Entrance(EntranceDecision::Travel { to, .. }) => Some(to.size),
            Self::Entrance(EntranceDecision::Reserve(_)) => None,
        }
    }
}

/// The tile of the flight running that a window of a pass merged into it lands on: how big the
/// picture it carries is, where the pass ends it, and whether a pass held before this one grows its
/// window. See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::transitions) struct Landing {
    pub(in crate::transitions) carried: ScreenSize,
    pub(in crate::transitions) to: ScreenSize,
    pub(in crate::transitions) held: bool,
}

impl EndPicture {
    /// Every sort.
    const ALL: [Self; 5] = [
        Self::Awaited,
        Self::Held,
        Self::Kept,
        Self::Repainting,
        Self::Lands,
    ];

    /// Which of `Waits`' lists a window so sorted is in: `awaiting`, `repainting` and `kept`.
    const fn lists(self) -> (bool, bool, bool) {
        match self {
            Self::Awaited => (true, false, false),
            Self::Held => (true, true, false),
            Self::Kept => (false, true, true),
            Self::Repainting => (false, true, false),
            Self::Lands => (false, false, false),
        }
    }

    /// The window `request` places, its tile starting at `start` on `display`, flying on `flies`:
    /// sorted on its request's own sizes, and, in a pass merged into the flight running, judged
    /// against the tile it lands on (`landing`, none for a first pass or a window joining). A shrink
    /// is kept only when its tile starts with a picture covering its start (`covers_start`).
    pub(in crate::transitions) fn of_request(
        request: &AnimationRequest,
        flies: FliesOn,
        landing: Option<Landing>,
        start: ScreenRect,
        display: ScreenRect,
    ) -> Self {
        let (own, composed) = match flies {
            FliesOn::Picture { source, covered } => (
                Self::of_window(source, request.from.size, request.to.size),
                Some(covered),
            ),
            FliesOn::Entrance(entrance) => (Self::of_entrance(entrance), None),
        };
        let carried = landing.map(|landing| landing.carried).or(composed);
        match own.landing_on(landing) {
            Self::Kept if !covers_start(carried, start, request.floating, display) => {
                Self::Repainting
            }
            sorted => sorted,
        }
    }

    /// A window the strip surface carries, which the pass neither resizes nor holds for, a stand-in
    /// included: it lands, unless judged against the tile it lands on in a flight running
    /// (`landing`).
    pub(in crate::transitions) fn of_strip(landing: Option<Landing>) -> Self {
        Self::Lands.landing_on(landing)
    }

    fn landing_on(self, landing: Option<Landing>) -> Self {
        landing.map_or(self, |Landing { carried, to, held }| {
            self.against_tile(carried, to, held)
        })
    }

    /// A window going from `from` to `to`, drawn from a picture taken `source`: a stand-in, which is
    /// no picture of the window, is awaited; a resize growing on either axis is held for its repaint;
    /// one that only shrinks is watched, never held for itself; a move lands.
    fn of_window(source: Route, from: ScreenSize, to: ScreenSize) -> Self {
        if source == Route::StandIn {
            Self::Awaited
        } else if grows(from, to) {
            Self::Held
        } else if is_a_resize(from, to) {
            Self::Kept
        } else {
            Self::Lands
        }
    }

    /// A merged pass's sort of a window, judged against the running tile it lands on, whose picture is
    /// `carried` big: a shrink, or a move of a window a pass held before it grows (`held`), ending at
    /// `to` wider or taller than that picture is a grow; every other sort stands. A first pass is
    /// judged on its own sizes alone, and so is a move of a window nothing held grows, however small
    /// its cached picture. See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
    fn against_tile(self, carried: ScreenSize, to: ScreenSize, held: bool) -> Self {
        let judged = self == Self::Kept || held && self == Self::Lands;
        if judged && grows(carried, to) {
            Self::Held
        } else {
            self
        }
    }

    /// A window that just opened: one flying from where it opened repaints where it ends, one already
    /// at its slot lands there, and a reservation is awaited.
    fn of_entrance(entrance: EntranceDecision) -> Self {
        match entrance {
            EntranceDecision::Travel { from, to } if from.same_as(to) => Self::Lands,
            EntranceDecision::Travel { .. } => Self::Repainting,
            EntranceDecision::Reserve(_) => Self::Awaited,
        }
    }
}

/// Whether a tile starting at `start` starts with a picture covering it: frame zero's, taken there
/// when it starts wholly on `display`, else the one it carries, `carried` big, when that fits it.
fn covers_start(
    carried: Option<ScreenSize>,
    start: ScreenRect,
    floating: bool,
    display: ScreenRect,
) -> bool {
    frame_zero_pictures(start, floating, display)
        || carried.is_some_and(|carried| fits_frame(carried, start.size))
}

/// A pass's windows as their `EndPicture`s sort them: `StageSummary::awaiting`, `::repainting` and
/// `::kept`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(in crate::transitions) struct Waits {
    /// Held for, overlay up, and watched until they repaint: grows, stand-ins and reserved entrances.
    pub(in crate::transitions) awaiting: Vec<WindowId>,
    /// Watched until they repaint, and pictured where they end only once they have: windows resized,
    /// held for if they grow and kept if they shrink inside the picture they start with, and windows
    /// flying from where they opened.
    pub(in crate::transitions) repainting: Vec<WindowId>,
    /// Of `repainting`, the shrinks whose tiles start with a picture covering their start, which they
    /// keep to the lift (`Flight::keeps`).
    pub(in crate::transitions) kept: Vec<WindowId>,
}

impl Waits {
    /// The grows: held for and pictured where they end only once repainted, what a pass merged while
    /// the motion runs is held for, a merged pass's judged against its tiles (`Landing`).
    pub(in crate::transitions) fn grows(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.awaiting
            .iter()
            .copied()
            .filter(|window| self.of(*window) == EndPicture::Held)
    }

    /// How `push` sorted `window`: a window in none of the lists lands.
    fn of(&self, window: WindowId) -> EndPicture {
        let lists = (
            self.awaiting.contains(&window),
            self.repainting.contains(&window),
            self.kept.contains(&window),
        );
        EndPicture::ALL
            .into_iter()
            .find(|end| end.lists() == lists)
            .unwrap_or(EndPicture::Lands)
    }

    /// Sorts the window `request` places, its tile starting at `start` on `display`, flying on `flies`
    /// and landing on `landing` (`EndPicture::of_request`).
    pub(in crate::transitions) fn push_request(
        &mut self,
        request: &AnimationRequest,
        flies: FliesOn,
        landing: Option<Landing>,
        start: ScreenRect,
        display: ScreenRect,
    ) {
        self.push(
            request.window,
            EndPicture::of_request(request, flies, landing, start, display),
        );
    }

    /// Sorts `window`, carried by the strip surface and landing on `landing` (`EndPicture::of_strip`).
    pub(in crate::transitions) fn push_strip(
        &mut self,
        window: WindowId,
        landing: Option<Landing>,
    ) {
        self.push(window, EndPicture::of_strip(landing));
    }

    pub(in crate::transitions) fn push(&mut self, window: WindowId, end: EndPicture) {
        let (holds, repaints, keeps) = end.lists();
        for (sorted, into) in [
            (holds, &mut self.awaiting),
            (repaints, &mut self.repainting),
            (keeps, &mut self.kept),
        ] {
            if sorted {
                into.push(window);
            }
        }
    }
}

impl FromIterator<(WindowId, EndPicture)> for Waits {
    fn from_iter<I: IntoIterator<Item = (WindowId, EndPicture)>>(ends: I) -> Self {
        let mut waits = Self::default();
        for (window, end) in ends {
            waits.push(window, end);
        }
        waits
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

    fn window(idx: u32) -> WindowId { WindowId::new(1, idx) }

    /// The built-in display, the menu bar's band included.
    const DISPLAY: ScreenRect =
        ScreenRect::new(ScreenPoint::new(0.0, 0.0), ScreenSize::new(1728.0, 1117.0));

    /// A SkyLight picture `covered` big.
    fn skylight(covered: ScreenSize) -> FliesOn {
        FliesOn::Picture {
            source: Route::SkyLight,
            covered,
        }
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

    fn end(from: ScreenSize, to: ScreenSize) -> EndPicture {
        EndPicture::of_window(Route::SkyLight, from, to)
    }

    /// A grow is held for its repaint, a fold that widens a window as it shortens it included.
    #[test]
    fn a_grow_is_held_for_its_repaint() {
        assert_eq!(
            end(COLUMN, ScreenSize::new(1720.0, 1081.0)),
            EndPicture::Held,
            "a grow"
        );
        assert_eq!(
            end(COLUMN, ScreenSize::new(1148.0, 538.0)),
            EndPicture::Held,
            "a fold"
        );
    }

    #[test]
    fn a_shrink_is_kept_unheld() {
        assert_eq!(end(COLUMN, ScreenSize::new(859.0, 535.0)), EndPicture::Kept);
        assert_eq!(end(ScreenSize::new(1720.0, 1081.0), COLUMN), EndPicture::Kept);
    }

    #[test]
    fn a_move_lands_a_point_of_rounding_included() {
        assert_eq!(end(COLUMN, COLUMN), EndPicture::Lands, "a move");
        assert_eq!(
            end(COLUMN, ScreenSize::new(860.0, 1081.0)),
            EndPicture::Lands,
            "rounding"
        );
        assert_eq!(
            EndPicture::of_window(Route::Baked, COLUMN, COLUMN),
            EndPicture::Lands
        );
    }

    #[test]
    fn a_stand_in_is_awaited_whatever_it_does() {
        for to in [
            COLUMN,
            ScreenSize::new(1720.0, 1081.0),
            ScreenSize::new(859.0, 535.0),
        ] {
            assert_eq!(
                EndPicture::of_window(Route::StandIn, COLUMN, to),
                EndPicture::Awaited
            );
        }
    }

    /// Held for: a window with no picture and a grow. Watched until repainted: a grow, a shrink and a
    /// travelling entrance. Kept: a shrink alone. A move, landing, is in none.
    #[test]
    fn a_grow_is_held_for_a_shrink_kept_and_both_watched_until_repainted() {
        let window = |idx| WindowId {
            pid: 1,
            idx: std::num::NonZeroU32::new(idx).unwrap(),
        };
        let waits: Waits = (1..).map(window).zip(EndPicture::ALL).collect();
        assert_eq!(waits, Waits {
            awaiting: vec![window(1), window(2)],
            repainting: vec![window(2), window(3), window(4)],
            kept: vec![window(3)],
        });
        assert_eq!(waits.grows().collect::<Vec<_>>(), [window(2)], "the grow alone");
    }

    /// Every sort pushed reads back as itself, so each is in its own lists.
    #[test]
    fn each_sort_reads_back_as_it_was_pushed() {
        let window = |idx| WindowId {
            pid: 1,
            idx: std::num::NonZeroU32::new(idx).unwrap(),
        };
        let waits: Waits = (1..).map(window).zip(EndPicture::ALL).collect();
        for (idx, end) in (1..).zip(EndPicture::ALL) {
            assert_eq!(waits.of(window(idx)), end);
        }
    }

    /// Kai's rapid Alt-R: a press shrinking the column on its own sizes, 1148 to 860, superseding a
    /// held grow whose repaint has not come, so its tile still carries the 572 picture, grows past
    /// that picture and is held; one ending within it stays kept.
    #[test]
    fn a_shrink_ending_wider_than_its_tiles_picture_is_held() {
        let (carried, to) = (ScreenSize::new(572.0, 1079.0), ScreenSize::new(860.0, 1079.0));
        assert_eq!(
            EndPicture::Kept.against_tile(carried, to, false),
            EndPicture::Held
        );
        assert_eq!(
            EndPicture::Kept.against_tile(ScreenSize::new(1148.0, 1079.0), to, false),
            EndPicture::Kept
        );
        assert_eq!(
            EndPicture::Kept.against_tile(ScreenSize::new(859.0, 1079.0), to, false),
            EndPicture::Kept,
            "rounding"
        );
    }

    /// A move of a window a held pass grows ends where that grow's repaint is still to come: past
    /// the picture its tile carries it is held too. A move of any other window stays a navigation,
    /// however small its cached picture, and a grow, a stand-in and an entrance keep their sorts.
    #[test]
    fn only_a_move_of_a_window_a_held_pass_grows_is_judged_against_its_tiles_picture() {
        let (carried, to) = (ScreenSize::new(572.0, 1079.0), ScreenSize::new(1148.0, 1079.0));
        assert_eq!(
            EndPicture::Lands.against_tile(carried, to, true),
            EndPicture::Held
        );
        assert_eq!(
            EndPicture::Lands.against_tile(to, to, true),
            EndPicture::Lands,
            "its picture covers it"
        );
        assert_eq!(
            EndPicture::Lands.against_tile(carried, to, false),
            EndPicture::Lands
        );
        for end in [
            EndPicture::Held,
            EndPicture::Awaited,
            EndPicture::Repainting,
        ] {
            assert_eq!(end.against_tile(carried, to, true), end);
        }
    }

    /// Sorted with the landing its tile gives, a window outgrowing its tile's picture is in a grow's
    /// lists, `awaiting` and `repainting`, not `kept`. A window joining has no tile to judge against
    /// and keeps its own sort, as does every window whose sort stands.
    #[test]
    fn a_merged_pass_sorts_only_the_windows_outgrowing_their_tiles_pictures_as_grows() {
        use rini_core::ids::WindowServerId;
        let to = ScreenSize::new(860.0, 1079.0);
        let request = |idx, from: ScreenSize| AnimationRequest {
            window: window(idx),
            server_id: WindowServerId::new(idx),
            from: ScreenRect::new(ScreenPoint::new(3.0, 35.0), from),
            to: ScreenRect::new(ScreenPoint::new(3.0, 35.0), to),
            floating: false,
            worn: Default::default(),
            strip_before: None,
            strip_after: None,
        };
        let landing = |carried: f64, held| {
            Some(Landing {
                carried: ScreenSize::new(carried, 1079.0),
                to,
                held,
            })
        };
        let wider = ScreenSize::new(1148.0, 1079.0);
        let pass = [
            (request(1, wider), landing(572.0, false)),
            (request(2, wider), landing(1148.0, false)),
            (request(3, to), landing(572.0, true)),
            (request(4, wider), None),
        ];
        let mut waits = Waits::default();
        for (request, landing) in &pass {
            waits.push_request(
                request,
                skylight(request.from.size),
                *landing,
                request.from,
                DISPLAY,
            );
        }
        assert_eq!(waits, Waits {
            awaiting: vec![window(1), window(3)],
            repainting: vec![window(1), window(2), window(3), window(4)],
            kept: vec![window(2), window(4)],
        });
        assert_eq!(waits.grows().collect::<Vec<_>>(), [window(1), window(3)]);
    }

    /// A window the strip surface carries lands, a stand-in's too, unless a pass held before grows it
    /// and it ends wider than the picture its tile carries.
    #[test]
    fn a_window_the_strip_carries_lands_unless_a_held_pass_grows_it_past_its_tiles_picture() {
        let (carried, to) = (ScreenSize::new(572.0, 1079.0), ScreenSize::new(1148.0, 1079.0));
        assert_eq!(EndPicture::of_strip(None), EndPicture::Lands);
        assert_eq!(
            EndPicture::of_strip(Some(Landing { carried, to, held: false })),
            EndPicture::Lands
        );
        assert_eq!(
            EndPicture::of_strip(Some(Landing { carried, to, held: true })),
            EndPicture::Held
        );
        assert_eq!(
            EndPicture::of_strip(Some(Landing { carried: to, to, held: true })),
            EndPicture::Lands
        );
    }

    /// A window that opened flies from where it opened and repaints at its slot, never held for; one
    /// already at its slot, a cold cache rather than an open, lands; a reservation is held for.
    #[test]
    fn an_entrance_that_travels_repaints_one_standing_lands_and_a_reservation_is_awaited() {
        let slot: ScreenRect = rect(867.0, 32.0, 859.0, 1081.0);
        let spawn = rect(400.0, 200.0, 700.0, 500.0);
        assert_eq!(
            EndPicture::of_entrance(EntranceDecision::Travel { from: spawn, to: slot }),
            EndPicture::Repainting
        );
        assert_eq!(
            EndPicture::of_entrance(EntranceDecision::Travel { from: slot, to: slot }),
            EndPicture::Lands
        );
        assert_eq!(
            EndPicture::of_entrance(EntranceDecision::Reserve("capture budget")),
            EndPicture::Awaited
        );
    }

    /// A merged pass judges a picture's tile where its path ends it and a travelling entrance's at
    /// its slot, so one standing there that a held pass grows past its tile's picture is held; a
    /// reservation has no tile to judge.
    #[test]
    fn a_merged_pass_judges_a_tile_where_it_ends_an_entrance_at_its_slot() {
        use rini_core::ids::WindowServerId;
        let slot: ScreenRect = rect(867.0, 32.0, 1148.0, 1081.0);
        let (end, carried) = (ScreenSize::new(860.0, 1081.0), ScreenSize::new(572.0, 1081.0));
        let standing = FliesOn::Entrance(EntranceDecision::Travel { from: slot, to: slot });
        assert_eq!(skylight(carried).ends_at(end), Some(end));
        assert_eq!(standing.ends_at(end), Some(slot.size));
        assert_eq!(
            FliesOn::Entrance(EntranceDecision::Reserve("capture budget")).ends_at(end),
            None
        );
        let request = AnimationRequest {
            window: window(1),
            server_id: WindowServerId::new(1),
            from: slot,
            to: slot,
            floating: false,
            worn: Default::default(),
            strip_before: None,
            strip_after: None,
        };
        let landing = standing.ends_at(end).map(|to| Landing { carried, to, held: true });
        assert_eq!(
            EndPicture::of_request(&request, standing, landing, slot, DISPLAY),
            EndPicture::Held
        );
        assert_eq!(
            EndPicture::of_request(&request, standing, None, slot, DISPLAY),
            EndPicture::Lands
        );
    }

    /// A pass's windows sorted from their requests, each by the sizes it goes from and to: a grow and
    /// a fold held for and watched, a shrink watched and kept, a move landing, a stand-in held for, an
    /// entrance flying from where it opened watched, one standing at its slot landing and a
    /// reservation held for.
    #[test]
    fn a_pass_s_windows_are_sorted_by_the_sizes_their_requests_go_from_and_to() {
        use rini_core::ids::WindowServerId;
        let request = |idx, from, to| AnimationRequest {
            window: window(idx),
            server_id: WindowServerId::new(idx),
            from,
            to,
            floating: false,
            worn: Default::default(),
            strip_before: None,
            strip_after: None,
        };
        let column: ScreenRect = rect(4.0, 32.0, 859.0, 1081.0);
        let next = rect(867.0, 32.0, 859.0, 1081.0);
        let picture = skylight(column.size);
        let pass = [
            (request(1, column, rect(4.0, 32.0, 1720.0, 1081.0)), picture),
            (request(2, column, rect(4.0, 32.0, 1148.0, 538.0)), picture),
            (request(3, column, rect(4.0, 32.0, 859.0, 535.0)), picture),
            (request(4, column, next), picture),
            (request(5, column, next), FliesOn::Picture {
                source: Route::StandIn,
                covered: next.size,
            }),
            (
                request(6, column, next),
                FliesOn::Entrance(EntranceDecision::Travel {
                    from: rect(400.0, 200.0, 700.0, 500.0),
                    to: next,
                }),
            ),
            (
                request(7, next, next),
                FliesOn::Entrance(EntranceDecision::Travel { from: next, to: next }),
            ),
            (
                request(8, column, next),
                FliesOn::Entrance(EntranceDecision::Reserve("capture budget")),
            ),
        ];
        let mut waits = Waits::default();
        for (request, flies) in &pass {
            waits.push_request(request, *flies, None, request.from, DISPLAY);
        }
        let windows = |idxs: &[u32]| idxs.iter().copied().map(window).collect::<Vec<_>>();
        assert_eq!(waits, Waits {
            awaiting: windows(&[1, 2, 5, 8]),
            repainting: windows(&[1, 2, 3, 6]),
            kept: windows(&[3])
        });
    }

    /// fold1's g17 (`docs/data/2026-10-10-fold`): a shrink starting part off the display, where frame
    /// zero does not picture it, is kept only when the picture its tile starts with fits its start,
    /// the cache's composed for a first pass and the one its tile carries for a merged one; one
    /// starting wholly on the display is kept whatever the cache holds, frame zero picturing it, but
    /// not a floating one, which frame zero leaves out.
    #[test]
    fn a_shrink_is_kept_only_when_the_picture_its_tile_starts_with_covers_its_start() {
        use rini_core::ids::WindowServerId;
        let request = |from: ScreenRect, floating| AnimationRequest {
            window: window(1),
            server_id: WindowServerId::new(1),
            from,
            to: rect(3.0, 35.0, 1148.0, 538.0),
            floating,
            worn: Default::default(),
            strip_before: None,
            strip_after: None,
        };
        let (fits, short) = (ScreenSize::new(1148.0, 1079.0), ScreenSize::new(1148.0, 900.0));
        let sort = |from: ScreenRect, floating, covered, carried: Option<ScreenSize>| {
            let request = request(from, floating);
            let landing = carried.map(|carried| Landing {
                carried,
                to: request.to.size,
                held: false,
            });
            EndPicture::of_request(&request, skylight(covered), landing, from, DISPLAY)
        };
        let (off, on) = (
            rect(-285.0, 35.0, 1148.0, 1079.0),
            rect(3.0, 35.0, 1148.0, 1079.0),
        );
        assert_eq!(
            sort(off, false, fits, None),
            EndPicture::Kept,
            "the cache's picture fits its start"
        );
        assert_eq!(
            sort(off, false, short, None),
            EndPicture::Repainting,
            "it does not"
        );
        assert_eq!(
            sort(off, false, short, Some(fits)),
            EndPicture::Kept,
            "its tile carries one that does"
        );
        assert_eq!(
            sort(off, false, fits, Some(short)),
            EndPicture::Repainting,
            "its tile carries one that does not"
        );
        assert_eq!(
            sort(on, false, short, None),
            EndPicture::Kept,
            "frame zero pictures it"
        );
        assert_eq!(sort(on, true, short, None), EndPicture::Repainting, "floating");
        assert_eq!(
            sort(on, true, fits, None),
            EndPicture::Kept,
            "floating, its cache's picture fitting"
        );
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
