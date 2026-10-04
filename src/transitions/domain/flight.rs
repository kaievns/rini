//! What a flight is.
//!
//! One pass over the layout becomes a flight: how a fresh group begins moving, whether a picture
//! landing in flight is swapped in and which tiles the focus refresh recaptures, what a pass asks for
//! the windows it has no picture of and whose hairline is harvested, which unmanaged window traces a
//! tile as its border, whether a pass is worth an overlay, how far the real windows are at the
//! handover, and what the flight reports. Decided over ids, frames and progress fractions, with
//! nothing of Core Animation in it. When the pictures service makes the captures a flight asks for is
//! `crate::pictures::domain::schedule`. Design in `src/transitions/docs/coordination.md`.

use objc2_core_foundation::CGRect;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use rini_core::ids::{WindowId, WindowServerId};

use crate::pictures::domain::schedule::Priority;

use super::timing::{
    COMPANION_CENTER_SLACK, COMPANION_EXPANSION, HANDOVER_THRESHOLD_PT, REFRESH_APPLY_BEFORE,
    REFRESH_PASSES_AT, REVEAL_APPLY_BEFORE,
};

/// Which of `tiles` to recapture mid-flight: the two ends of a focus change, and nothing else.
/// See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn refresh_targets(
    previous: Option<WindowId>,
    current: Option<WindowId>,
    tiles: &[WindowId],
) -> Vec<WindowId> {
    let Some(current) = current else { return Vec::new() };
    if previous == Some(current) {
        return Vec::new();
    }
    [Some(current), previous]
        .into_iter()
        .flatten()
        .filter(|window| tiles.contains(window))
        .collect()
}

/// A flight's recapture of the two ends of its focus change, in the passes at `REFRESH_PASSES_AT`.
/// See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
#[derive(Debug, Default)]
pub(in crate::transitions) struct FocusRefresh {
    /// How many of the passes have been taken.
    passes: usize,
    /// When the first pass asked for its pictures.
    asked: Option<Instant>,
    /// The windows recaptured: the only moving tiles that may take a picture mid-flight.
    targets: Vec<WindowId>,
    /// When the picture the refresh last cut onto each target was taken.
    shown: Vec<(WindowId, Instant)>,
}

impl FocusRefresh {
    /// Whether a pass is due at `progress`; takes it when it is. A first look that comes late skips
    /// the passes it overslept rather than taking them back to back.
    pub(in crate::transitions) fn take_pass(&mut self, progress: f64, allowed: bool) -> bool {
        let due = REFRESH_PASSES_AT.iter().filter(|&&at| progress >= at).count();
        if !allowed || due <= self.passes {
            return false;
        }
        self.passes = due;
        true
    }

    /// A pass asked for pictures of `targets` at `now`.
    pub(in crate::transitions) fn ask(&mut self, targets: Vec<WindowId>, now: Instant) {
        self.targets = targets;
        self.asked.get_or_insert(now);
    }

    pub(in crate::transitions) fn is_target(&self, window: WindowId) -> bool {
        self.targets.contains(&window)
    }

    /// Whether a picture of `window` taken at `taken` was taken for this refresh, and is newer than
    /// the one the refresh last cut in.
    ///
    /// A picture requested before the flight, or coalesced into a capture already running when the
    /// refresh asked, shows the focus as it was; landing after the refresh's own, it would cut the tile
    /// back. Both are older than the ask: a capture's `taken` is never earlier than the capture began,
    /// on either route.
    pub(in crate::transitions) fn is_fresh(&self, window: WindowId, taken: Instant) -> bool {
        let shown = self.shown.iter().find(|(w, _)| *w == window).map(|(_, at)| *at);
        self.asked.is_some_and(|asked| taken >= asked) && shown.is_none_or(|shown| taken > shown)
    }

    /// The refresh cut a picture of `window` taken at `taken` onto its tile.
    pub(in crate::transitions) fn cut(&mut self, window: WindowId, taken: Instant) {
        match self.shown.iter_mut().find(|(w, _)| *w == window) {
            Some(entry) => entry.1 = taken,
            None => self.shown.push((window, taken)),
        }
    }
}

/// How a fresh group of tiles begins moving.
pub(in crate::transitions) enum GroupStart {
    /// Wait one `COALESCE_WINDOW` for the reactor's layout passes to settle. For layout changes.
    Coalesced,
    /// Move now. For strip movements, which arrive once per keystroke.
    Immediate,
}

/// The unmanaged window tracing `frame` as its border, if any. A parked window never traces and is
/// never traced. See "Border companions" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn companion_of(
    frame: CGRect,
    candidates: &[(WindowServerId, CGRect)],
    display: CGRect,
) -> Option<(WindowServerId, CGRect)> {
    if rini_geometry::is_off_screen(display, frame) {
        return None;
    }
    let center = |r: CGRect| (r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0);
    let (cx, cy) = center(frame);
    candidates
        .iter()
        .filter(|(_, candidate)| !rini_geometry::is_off_screen(display, *candidate))
        .find(|(_, candidate)| {
            let dw = candidate.size.width - frame.size.width;
            let dh = candidate.size.height - frame.size.height;
            let (kx, ky) = center(*candidate);
            (-1.0..=COMPANION_EXPANSION).contains(&dw)
                && (-1.0..=COMPANION_EXPANSION).contains(&dh)
                && (kx - cx).abs() <= COMPANION_CENTER_SLACK
                && (ky - cy).abs() <= COMPANION_CENTER_SLACK
        })
        .copied()
}

/// What a tile is doing when a picture of its window lands mid-flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::transitions) enum TileState {
    /// No tile for this window in the flight.
    NotTiled,
    /// The flight holds for this window's first picture: an entrance reservation.
    Awaiting,
    /// A grow waiting for its reveal, held or flying the stand-in. `fits`: the landed picture
    /// covers the destination.
    Reveal { fits: bool },
    /// An ordinary moving tile. `fits`: the picture covers the destination; `resizing`: the tile
    /// changes size in flight.
    Moving { fits: bool, resizing: bool },
    /// A moving tile whose picture is the flight's own destination refresh.
    MovingRefreshTarget { fits: bool, resizing: bool },
}

/// What to do with a picture that landed while a flight is running, after it is cached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::transitions) enum SwapDecision {
    /// A held flight takes it at frame zero (`claim_reveal`).
    Claim,
    /// A moving flight adds the entrance late (`admit_entrance`).
    Admit,
    /// Hard-cut onto the moving tile, with the reason logged.
    Swap(&'static str),
    /// Cache only, for the next flight.
    CacheOnly,
}

/// Whether a picture landing mid-flight may change what a tile draws. `progress` is `None`
/// before the flight starts moving. See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn should_swap_mid_flight(
    state: TileState,
    settled: bool,
    fresh: bool,
    progress: Option<f64>,
) -> SwapDecision {
    match state {
        TileState::Awaiting | TileState::Reveal { .. } if progress.is_none() => SwapDecision::Claim,
        TileState::Awaiting => SwapDecision::Admit,
        TileState::Reveal { fits: true }
            if settled && progress.is_some_and(|p| p < REVEAL_APPLY_BEFORE) =>
        {
            SwapDecision::Swap("reveal")
        }
        TileState::Reveal { .. } => SwapDecision::CacheOnly,
        // No likeness test: a focus change is a traffic light and a dimmed title bar, which no
        // thumbprint small enough to be cheap can see, and a cut to an identical picture is invisible.
        TileState::MovingRefreshTarget { fits: true, resizing }
            if fresh
                && (!resizing || settled)
                && progress.is_some_and(|p| p < REFRESH_APPLY_BEFORE) =>
        {
            SwapDecision::Swap("refresh")
        }
        TileState::NotTiled | TileState::Moving { .. } | TileState::MovingRefreshTarget { .. } => {
            SwapDecision::CacheOnly
        }
    }
}

/// What a pass asks for the windows it has no picture of. Frame zero while its flight has not yet
/// begun holding or moving, since a capture landing then can still serve it; otherwise a warm, which
/// waits out the flight for the next one.
pub(in crate::transitions) fn missing_pictures_priority(started: bool, holding: bool) -> Priority {
    if started || holding {
        Priority::Warm
    } else {
        Priority::FrameZero
    }
}

/// Whether a background capture landing now has its window's hairline harvested: only at rest, since
/// in flight the quiet after the lift harvests instead, and only for a capture that covered its window.
pub(in crate::transitions) fn harvests_landing(flying: bool, covered: bool) -> bool {
    !flying && covered
}

/// Which animated windows `after_flight_captures` harvests a hairline for: each at most once per
/// flight.
pub(in crate::transitions) fn finish_harvest_set(
    animated: &[WindowId],
    harvested: &HashSet<WindowId>,
    requested: &[WindowId],
    dressed: &HashSet<WindowId>,
) -> Vec<WindowId> {
    let mut seen = HashSet::new();
    animated
        .iter()
        .copied()
        .filter(|w| {
            !harvested.contains(w)
                && !requested.contains(w)
                && !dressed.contains(w)
                && seen.insert(*w)
        })
        .collect()
}

/// Whether an in-flight merge leaves the already-applied frames stale. A parked window has no
/// tile, so `frames_changed` counts too. See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn mark_stale_on_untiled_change(
    changed: bool,
    frames_changed: bool,
) -> bool {
    changed || frames_changed
}

/// How far the real windows were from their tiles at lift. See `report_handover_error`.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::transitions) struct HandoverReport {
    /// Windows measured: tiled, answered by the server, intended on screen.
    pub(in crate::transitions) total: usize,
    /// Windows more than `HANDOVER_THRESHOLD_PT` off.
    pub(in crate::transitions) count_over: usize,
    /// The largest error among the windows the report counts.
    pub(in crate::transitions) worst_visible_pt: f64,
    pub(in crate::transitions) worst_wsid: u32,
}

/// Measures every tiled window's real frame against its intended one. Parks are excluded: macOS
/// clamps them. See "Real windows land before lift" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn handover_report(
    final_frames: &[(WindowId, CGRect)],
    tiled: &[WindowId],
    real: &HashMap<WindowId, CGRect>,
    display: CGRect,
) -> HandoverReport {
    let mut report = HandoverReport {
        total: 0,
        count_over: 0,
        worst_visible_pt: 0.0,
        worst_wsid: 0,
    };
    for (window, intended) in final_frames {
        if !tiled.contains(window) {
            continue;
        }
        if rini_geometry::is_off_screen(display, *intended) {
            continue;
        }
        let Some(actual) = real.get(window) else { continue };
        report.total += 1;
        let dx = (actual.origin.x - intended.origin.x).abs();
        let dy = (actual.origin.y - intended.origin.y).abs();
        let error = dx.max(dy);
        if error > HANDOVER_THRESHOLD_PT {
            report.count_over += 1;
        }
        if error > report.worst_visible_pt {
            report.worst_visible_pt = error;
            report.worst_wsid = window.idx.get();
        }
    }
    report
}

/// What a settled picture did for a flight holding at frame zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::transitions) enum Claimed {
    /// Taken; other holds remain.
    Held,
    /// Taken, and it was the last hold: the flight may start moving.
    Released,
    /// Taken by a composed tile standing at frame zero; no hold was involved.
    Refreshed,
}

/// Whether a composed pass is worth an overlay flight.
/// See "Layout changes" in `src/transitions/docs/coordination.md`.
pub(in crate::transitions) fn worth_flying(moving_drawable: bool, running: bool) -> bool {
    moving_drawable || running
}

/// The window server ids a border companion may never be: everything rini manages. Synthetic
/// (companion) ids carry pid 0 and are left out, or a border seen once could never match again.
pub(in crate::transitions) fn managed_server_ids(
    pass: &std::collections::HashSet<u32>,
    cached: impl Iterator<Item = WindowId>,
    owed: impl Iterator<Item = WindowId>,
) -> std::collections::HashSet<u32> {
    let mut ids = pass.clone();
    ids.extend(cached.chain(owed).filter(|w| w.pid != 0).map(|w| w.idx.get()));
    ids
}

/// What whoever installed `on_flight` is told: `true` when an overlay goes up, `false` once flying has
/// settled. Settled is the quiet period after a lift (`SETTLE_BEFORE_CAPTURES`), not the lift, so a
/// burst of flights reads as one: a flight beginning inside the quiet period says nothing, and the
/// `false` waits for its own settle.
#[derive(Debug, Default)]
pub(in crate::transitions) struct FlightReport {
    flying: bool,
}

impl FlightReport {
    /// An overlay went up. `Some(true)` unless a flight is already reported.
    pub(in crate::transitions) fn overlay_up(&mut self) -> Option<bool> {
        (!std::mem::replace(&mut self.flying, true)).then_some(true)
    }

    /// The quiet period after the last lift ran out. `Some(false)` unless no flight was reported.
    pub(in crate::transitions) fn settled(&mut self) -> Option<bool> {
        std::mem::replace(&mut self.flying, false).then_some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(idx: u32) -> WindowId {
        WindowId::new(7, idx)
    }

    fn after(base: Instant, millis: u64) -> Instant {
        base + std::time::Duration::from_millis(millis)
    }

    /// Nothing is fresh before the refresh asks: the flight has no picture of its own yet.
    #[test]
    fn nothing_is_fresh_before_the_refresh_asks() {
        let refresh = FocusRefresh::default();
        assert!(!refresh.is_fresh(window(1), Instant::now()));
    }

    /// The race behind the rule: a capture already running when the refresh asked is coalesced with
    /// it, lands later, and shows the focus as it was. It carries the older stamp and is refused.
    #[test]
    fn a_picture_begun_before_the_ask_is_not_fresh() {
        let base = Instant::now();
        let mut refresh = FocusRefresh::default();
        refresh.ask(vec![window(1), window(2)], after(base, 100));

        assert!(
            !refresh.is_fresh(window(1), after(base, 40)),
            "begun before the ask"
        );
        assert!(
            refresh.is_fresh(window(1), after(base, 100)),
            "begun as it asked"
        );
        assert!(refresh.is_fresh(window(2), after(base, 130)));
    }

    /// Once the refresh has cut a picture in, only a newer one may follow: the first pass's service
    /// capture landing after the second pass's framed one would cut the tile back a step.
    #[test]
    fn after_a_cut_only_a_newer_picture_is_fresh() {
        let base = Instant::now();
        let mut refresh = FocusRefresh::default();
        refresh.ask(vec![window(1)], after(base, 100));
        refresh.cut(window(1), after(base, 250));

        assert!(
            !refresh.is_fresh(window(1), after(base, 120)),
            "older than what is shown"
        );
        assert!(!refresh.is_fresh(window(1), after(base, 250)), "the one shown");
        assert!(refresh.is_fresh(window(1), after(base, 260)));
    }

    /// The second pass keeps the first pass's ask: its pictures are newer, and so still fresh, while
    /// anything from before the first pass stays refused.
    #[test]
    fn a_second_pass_keeps_the_first_ask() {
        let base = Instant::now();
        let mut refresh = FocusRefresh::default();
        refresh.ask(vec![window(1)], after(base, 100));
        refresh.ask(vec![window(1)], after(base, 220));

        assert!(
            refresh.is_fresh(window(1), after(base, 150)),
            "the first pass's own picture"
        );
        assert!(!refresh.is_fresh(window(1), after(base, 90)));
        assert!(refresh.is_target(window(1)));
        assert!(!refresh.is_target(window(2)));
    }

    /// At a fresh composition, or merging into a flight still coalescing, the missing pictures may still
    /// serve it; holding or moving, they are for the next flight.
    #[test]
    fn a_pass_asks_frame_zero_pictures_only_before_its_flight_holds_or_moves() {
        assert_eq!(missing_pictures_priority(false, false), Priority::FrameZero);
        assert_eq!(missing_pictures_priority(false, true), Priority::Warm, "holding");
        assert_eq!(missing_pictures_priority(true, false), Priority::Warm, "moving");
    }

    #[test]
    fn a_landing_is_harvested_only_at_rest_and_only_if_it_covered_its_window() {
        assert!(harvests_landing(false, true));
        assert!(!harvests_landing(true, true), "in flight");
        assert!(!harvests_landing(false, false), "a sliver");
    }

    #[test]
    fn the_passes_are_taken_once_each_and_only_when_allowed() {
        let mut refresh = FocusRefresh::default();
        assert!(!refresh.take_pass(0.1, true), "before the first");
        assert!(!refresh.take_pass(0.3, false), "not allowed now");
        assert!(refresh.take_pass(0.3, true), "and taken once it is");
        assert!(!refresh.take_pass(0.4, true));
        assert!(refresh.take_pass(0.55, true));
        assert!(!refresh.take_pass(1.0, true), "spent");
    }

    #[test]
    fn a_flight_is_reported_once_up_and_once_settled() {
        let mut report = FlightReport::default();
        assert_eq!(report.overlay_up(), Some(true));
        assert_eq!(report.settled(), Some(false));
        assert_eq!(report.settled(), None, "nothing is flying to settle");
    }

    /// A second flight lifting off before the first has settled cancels that settle, so the burst is
    /// one `true` and one `false`, the `false` at the last flight's settle.
    #[test]
    fn a_flight_inside_the_quiet_period_continues_the_one_reported() {
        let mut report = FlightReport::default();
        assert_eq!(report.overlay_up(), Some(true));
        assert_eq!(report.overlay_up(), None);
        assert_eq!(report.overlay_up(), None);
        assert_eq!(report.settled(), Some(false));
        assert_eq!(
            report.overlay_up(),
            Some(true),
            "a flight after the settle is a new one"
        );
    }

    #[test]
    fn a_settle_with_no_flight_says_nothing() {
        assert_eq!(FlightReport::default().settled(), None);
    }

    #[test]
    fn refresh_targets_are_the_two_ends_of_a_focus_change() {
        let tiles = [window(1), window(2), window(3)];
        // No change: a strip pan with focus where it was.
        assert!(refresh_targets(Some(window(1)), Some(window(1)), &tiles).is_empty());
        // No current focus: nothing to recapture towards.
        assert!(refresh_targets(Some(window(1)), None, &tiles).is_empty());
        assert!(refresh_targets(None, None, &tiles).is_empty());
        // A change between two tiles: both, destination first.
        assert_eq!(
            refresh_targets(Some(window(1)), Some(window(2)), &tiles),
            vec![window(2), window(1)]
        );
        // Only one end is in the flight: only that one.
        assert_eq!(refresh_targets(Some(window(9)), Some(window(2)), &tiles), vec![window(2)]);
        assert_eq!(refresh_targets(Some(window(1)), Some(window(9)), &tiles), vec![window(1)]);
        // First flight ever: the destination alone.
        assert_eq!(refresh_targets(None, Some(window(3)), &tiles), vec![window(3)]);
        // Neither end tiled: nothing, whatever else is flying.
        assert!(refresh_targets(Some(window(8)), Some(window(9)), &tiles).is_empty());
    }

    #[test]
    fn an_untiled_frame_change_in_flight_marks_frames_stale() {
        assert!(
            mark_stale_on_untiled_change(false, true),
            "frames_applied stays true after an untiled frame change"
        );
    }

    #[test]
    fn finish_harvests_each_animated_window_at_most_once() {
        let animated = [window(1), window(2), window(3), window(4), window(2)];
        let mut harvested = HashSet::new();
        assert!(harvested.insert(window(1)), "the chase's dressing");
        assert!(
            !harvested.insert(window(1)),
            "a second harvest for the same window is skipped"
        );
        let requested = [window(3)];
        let dressed: HashSet<WindowId> = [window(4)].into_iter().collect();
        assert_eq!(
            finish_harvest_set(&animated, &harvested, &requested, &dressed),
            vec![window(2)]
        );
        assert!(
            finish_harvest_set(&[], &harvested, &requested, &dressed).is_empty(),
            "nothing animated, nothing harvested"
        );
    }

    /// Applied frames go stale when a tile changed or any final frame did.
    #[test]
    fn frames_go_stale_on_a_tile_or_an_untiled_change() {
        assert!(!mark_stale_on_untiled_change(false, false));
        assert!(mark_stale_on_untiled_change(true, false));
        assert!(mark_stale_on_untiled_change(false, true));
        assert!(mark_stale_on_untiled_change(true, true));
    }

    mod companions {
        use super::*;
        use crate::animation::domain::motion::testing::{DISPLAY, rect};

        /// Border geometry from the user's bordersrc: a concentric sibling window a few points larger.
        #[test]
        fn a_border_window_tracing_a_window_is_its_companion() {
            let window = rect(4.0, 32.0, 859.0, 1081.0);
            let border = (WindowServerId::new(9001), rect(1.0, 29.0, 865.0, 1087.0));
            let neighbor = (WindowServerId::new(9002), rect(867.0, 32.0, 859.0, 1081.0));
            let zoom = (WindowServerId::new(9003), rect(224.0, 95.0, 1280.0, 960.0));
            let found = companion_of(window, &[neighbor, zoom, border], DISPLAY);
            assert_eq!(found.map(|(id, _)| id.as_u32()), Some(9001));
        }

        #[test]
        fn only_a_concentric_hug_counts_as_a_border() {
            let window = rect(4.0, 32.0, 859.0, 1081.0);
            let exact = (WindowServerId::new(1), rect(4.0, 32.0, 859.0, 1081.0));
            assert!(companion_of(window, &[exact], DISPLAY).is_some());
            let shifted = (WindowServerId::new(2), rect(24.0, 32.0, 859.0, 1081.0));
            let larger = (WindowServerId::new(3), rect(-16.0, 12.0, 899.0, 1121.0));
            let smaller = (WindowServerId::new(4), rect(6.0, 34.0, 855.0, 1077.0));
            assert!(companion_of(window, &[shifted, larger, smaller], DISPLAY).is_none());
        }

        /// Parked windows share a frame macOS clamps to 41pt visible, past `is_off_screen`; only the
        /// managed-id exclusion separates them. A synthetic companion id (pid 0) stays a candidate.
        #[test]
        fn a_managed_window_is_never_a_border_candidate() {
            let pass: std::collections::HashSet<u32> = [102698].into_iter().collect();
            let cached = [WindowId::new(82799, 102682), WindowId::new(0, 9001)];
            let owed = [WindowId::new(872, 51462)];
            let managed = managed_server_ids(&pass, cached.into_iter(), owed.into_iter());
            assert!(managed.contains(&102698) && managed.contains(&102682) && managed.contains(&51462));
            assert!(
                !managed.contains(&9001),
                "a border seen before is still a border"
            );
            // The clamped park is not judged off screen, so geometry alone cannot exclude it.
            let park = rect(1727.0, 1076.0, 1720.0, 1081.0);
            assert!(!rini_geometry::is_off_screen(DISPLAY, park));
            assert!(companion_of(park, &[(WindowServerId::new(102682), park)], DISPLAY).is_some());
        }

        #[test]
        fn a_parked_window_neither_traces_nor_is_traced() {
            let park = rect(
                DISPLAY.size.width - 1.0,
                DISPLAY.size.height - 1.0,
                859.0,
                1081.0,
            );
            assert!(rini_geometry::is_off_screen(DISPLAY, park));
            let twin = (WindowServerId::new(7), park);
            assert!(companion_of(park, &[twin], DISPLAY).is_none(), "a parked anchor");
            let on_screen = rect(4.0, 32.0, 859.0, 1081.0);
            let border = (WindowServerId::new(8), rect(1.0, 29.0, 865.0, 1087.0));
            assert!(
                companion_of(on_screen, &[twin, border], DISPLAY).map(|(id, _)| id.as_u32()) == Some(8)
            );
        }
    }

    mod swaps {
        use super::*;
        use crate::animation::domain::motion::testing::{Gen, RUNS};

        const STATES: [TileState; 12] = [
            TileState::NotTiled,
            TileState::Awaiting,
            TileState::Reveal { fits: false },
            TileState::Reveal { fits: true },
            TileState::Moving { fits: false, resizing: false },
            TileState::Moving { fits: false, resizing: true },
            TileState::Moving { fits: true, resizing: false },
            TileState::Moving { fits: true, resizing: true },
            TileState::MovingRefreshTarget { fits: false, resizing: false },
            TileState::MovingRefreshTarget { fits: false, resizing: true },
            TileState::MovingRefreshTarget { fits: true, resizing: false },
            TileState::MovingRefreshTarget { fits: true, resizing: true },
        ];

        /// The rule, spelled out independently of the implementation.
        fn expected(
            state: TileState,
            settled: bool,
            fresh: bool,
            progress: Option<f64>,
        ) -> SwapDecision {
            match state {
                TileState::Awaiting | TileState::Reveal { .. } if progress.is_none() => {
                    SwapDecision::Claim
                }
                TileState::Awaiting => SwapDecision::Admit,
                // 2.4: a settled reveal before 0.6 is swapped whatever route it came by.
                TileState::Reveal { fits } => {
                    if fits && settled && progress.is_some_and(|p| p < 0.6) {
                        SwapDecision::Swap("reveal")
                    } else {
                        SwapDecision::CacheOnly
                    }
                }
                TileState::MovingRefreshTarget { fits: true, resizing } => {
                    let early = progress.is_some_and(|p| p < 0.9);
                    if early && fresh && (!resizing || settled) {
                        SwapDecision::Swap("refresh")
                    } else {
                        SwapDecision::CacheOnly
                    }
                }
                _ => SwapDecision::CacheOnly,
            }
        }

        #[test]
        fn a_background_picture_never_swaps_onto_a_moving_tile() {
            let decision = should_swap_mid_flight(
                TileState::Moving { fits: true, resizing: false },
                false,
                true,
                Some(0.4),
            );
            assert_eq!(
                decision,
                SwapDecision::CacheOnly,
                "an unsolicited picture reached the moving tile: {decision:?}"
            );
        }

        #[test]
        fn a_refresh_landing_late_is_cached_only() {
            let decision = should_swap_mid_flight(
                TileState::MovingRefreshTarget { fits: true, resizing: false },
                false,
                true,
                Some(0.98),
            );
            assert_eq!(
                decision,
                SwapDecision::CacheOnly,
                "a refresh at 0.98 cut onto the tile: {decision:?}"
            );
        }

        #[test]
        fn should_swap_mid_flight_full_table() {
            let progresses = [
                None,
                Some(0.3),
                Some(0.59),
                Some(0.6),
                Some(0.89),
                Some(0.9),
            ];
            let mut swaps = 0usize;
            for state in STATES {
                for settled in [false, true] {
                    for fresh in [false, true] {
                        for progress in progresses {
                            let got = should_swap_mid_flight(state, settled, fresh, progress);
                            assert_eq!(
                                got,
                                expected(state, settled, fresh, progress),
                                "{state:?} settled={settled} fresh={fresh} progress={progress:?}"
                            );
                            if matches!(got, SwapDecision::Swap(_)) {
                                swaps += 1;
                            }
                        }
                    }
                }
            }
            // Refresh: fits, fresh, 4 progresses before 0.9 x 3 (settle x resizing) = 12;
            // reveal: fits, settled, 2 freshness x 2 progresses before 0.6 = 4.
            assert_eq!(
                swaps,
                12 + 4,
                "the table has exactly the refresh and reveal swaps in their windows"
            );
        }

        #[test]
        fn swap_only_for_the_early_refresh_target() {
            let mut rng = Gen(95);
            let mut swaps = 0usize;
            for _ in 0..RUNS {
                let state = STATES[rng.below(STATES.len() as u64) as usize];
                let settled = rng.coin();
                let fresh = rng.coin();
                let progress = rng.coin().then(|| rng.below(1001) as f64 / 1000.0);
                let decision = should_swap_mid_flight(state, settled, fresh, progress);
                if let SwapDecision::Swap(reason) = decision {
                    swaps += 1;
                    match state {
                        TileState::MovingRefreshTarget { fits: true, .. } => {
                            assert_eq!(reason, "refresh", "seed 95: {state:?}");
                            assert!(fresh, "seed 95: swapped a picture older than the refresh");
                            assert!(
                                progress.is_some_and(|p| p < 0.9),
                                "seed 95: refresh at {progress:?}"
                            );
                        }
                        TileState::Reveal { fits: true } => {
                            assert_eq!(reason, "reveal", "seed 95: {state:?}");
                            assert!(settled, "seed 95: an unsettled reveal swapped");
                            assert!(
                                progress.is_some_and(|p| p < 0.6),
                                "seed 95: reveal at {progress:?}"
                            );
                        }
                        other => panic!("seed 95: swapped onto {other:?}"),
                    }
                }
                if matches!(state, TileState::Moving { .. }) {
                    assert_eq!(decision, SwapDecision::CacheOnly, "seed 95: {state:?}");
                }
            }
            assert!(swaps > 0, "generator sanity: no Swap in {RUNS} runs");
        }

        /// A picture older than the refresh shows the focus as it was, so it never reaches the tile.
        #[test]
        fn a_picture_older_than_the_refresh_never_swaps_it() {
            for settled in [false, true] {
                for resizing in [false, true] {
                    let decision = should_swap_mid_flight(
                        TileState::MovingRefreshTarget { fits: true, resizing },
                        settled,
                        false,
                        Some(0.3),
                    );
                    assert_eq!(
                        decision,
                        SwapDecision::CacheOnly,
                        "settled={settled} resizing={resizing}: a stale picture swapped"
                    );
                }
            }
        }

        /// The reported flicker: the window being left kept its focused rendering until the lift. A
        /// fresh picture of it is cut in whatever it looks like, until late in the flight.
        #[test]
        fn a_fresh_refresh_picture_swaps_until_late_in_the_flight() {
            let at = |progress| {
                should_swap_mid_flight(
                    TileState::MovingRefreshTarget { fits: true, resizing: false },
                    false,
                    true,
                    Some(progress),
                )
            };
            assert_eq!(at(0.3), SwapDecision::Swap("refresh"));
            assert_eq!(
                at(0.82),
                SwapDecision::Swap("refresh"),
                "measured: landings at 0.82"
            );
            assert_eq!(at(0.9), SwapDecision::CacheOnly);
        }

        #[test]
        fn a_refresh_swap_implies_a_fresh_picture() {
            let mut rng = Gen(97);
            let mut swaps = 0usize;
            for _ in 0..RUNS {
                let state = STATES[rng.below(STATES.len() as u64) as usize];
                let fresh = rng.coin();
                let progress = rng.coin().then(|| rng.below(1001) as f64 / 1000.0);
                let decision = should_swap_mid_flight(state, rng.coin(), fresh, progress);
                if decision == SwapDecision::Swap("refresh") {
                    swaps += 1;
                    assert!(
                        fresh,
                        "seed 97: {state:?} at {progress:?} swapped a stale picture"
                    );
                }
            }
            assert!(swaps > 0, "generator sanity: no refresh swap in {RUNS} runs");
        }

        #[test]
        fn an_unfitting_or_stale_picture_is_cached_only() {
            let mut rng = Gen(94);
            for _ in 0..RUNS {
                let settled = rng.coin();
                let resizing = rng.coin();
                let progress = rng.coin().then(|| rng.below(1001) as f64 / 1000.0);
                assert_eq!(
                    should_swap_mid_flight(TileState::NotTiled, settled, rng.coin(), progress),
                    SwapDecision::CacheOnly,
                    "seed 94: no tile"
                );
                for state in [
                    TileState::Moving { fits: false, resizing },
                    TileState::MovingRefreshTarget { fits: false, resizing },
                ] {
                    assert_eq!(
                        should_swap_mid_flight(state, settled, rng.coin(), progress),
                        SwapDecision::CacheOnly,
                        "seed 94: {state:?} does not fit"
                    );
                }
                for state in [
                    TileState::Moving { fits: true, resizing },
                    TileState::MovingRefreshTarget { fits: true, resizing },
                ] {
                    assert_eq!(
                        should_swap_mid_flight(state, settled, false, progress),
                        SwapDecision::CacheOnly,
                        "seed 94: {state:?} is older than the refresh"
                    );
                }
            }
        }
    }

    mod handover {
        use objc2_core_foundation::{CGPoint, CGSize};

        use super::*;
        use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, rect};

        fn measured(
            frames: &[(WindowId, CGRect, CGRect)],
        ) -> (Vec<(WindowId, CGRect)>, Vec<WindowId>, HashMap<WindowId, CGRect>) {
            let final_frames = frames.iter().map(|(w, intended, _)| (*w, *intended)).collect();
            let tiled = frames.iter().map(|(w, _, _)| *w).collect();
            let real = frames.iter().map(|(w, _, actual)| (*w, *actual)).collect();
            (final_frames, tiled, real)
        }

        /// A park clamped by macOS is not the flight's error.
        #[test]
        fn the_handover_report_excludes_off_screen_intents_and_counts_misses() {
            let display = rect(0.0, 0.0, 1728.0, 1117.0);
            let parked = window(108);
            let strip = window(200);
            let final_frames = vec![
                (parked, rect(1727.0, 1116.0, 859.0, 1081.0)),
                (strip, rect(867.0, 32.0, 859.0, 1081.0)),
            ];
            let real: HashMap<WindowId, CGRect> = [
                (parked, rect(1727.0, 1051.0, 859.0, 1081.0)),
                (strip, rect(870.0, 32.0, 859.0, 1081.0)),
            ]
            .into_iter()
            .collect();
            let report = handover_report(&final_frames, &[parked, strip], &real, display);
            assert!(
                report.count_over == 1
                    && report.worst_visible_pt == 3.0
                    && report.worst_wsid == 200,
                "expected count_over=1 worst=3pt wsid=200; got {report:?}"
            );
        }

        /// The log's cases: a clamped park excluded, a leaving window's park miss excluded, two
        /// on-screen misses counted.
        #[test]
        fn handover_report_counts_on_screen_misses_only() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            // The park: clamped by macOS, not the flight's error.
            let clamp = [
                (
                    window(108),
                    rect(1727.0, 1116.0, 859.0, 1081.0),
                    rect(1727.0, 1051.0, 859.0, 1081.0),
                ),
                (window(200), slot, rect(870.0, 32.0, 859.0, 1081.0)),
            ];
            let (f, t, r) = measured(&clamp);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 1, "108 is excluded from the measured set");
            assert_eq!(report.count_over, 1);
            assert_eq!(report.worst_visible_pt, 3.0);
            assert_eq!(report.worst_wsid, 200);

            // The leaving window: intended at its park past the right edge, still in its slot.
            let park_miss = [
                (
                    window(1),
                    rect(1728.0, 32.0, 1720.0, 1081.0),
                    rect(-1718.0, 32.0, 1720.0, 1081.0),
                ),
                (
                    window(2),
                    rect(4.0, 32.0, 1720.0, 1081.0),
                    rect(4.0, 32.0, 1720.0, 1081.0),
                ),
            ];
            let (f, t, r) = measured(&park_miss);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 1);
            assert_eq!(report.count_over, 0, "a park intent is not measured");
            assert_eq!(report.worst_visible_pt, 0.0);

            let two = [
                (
                    window(1),
                    rect(4.0, 32.0, 859.0, 1081.0),
                    rect(9.0, 32.0, 859.0, 1081.0),
                ),
                (window(2), slot, rect(867.0, 40.0, 859.0, 1081.0)),
                (
                    window(3),
                    rect(1730.0, 32.0, 859.0, 1081.0),
                    rect(1730.0, 32.0, 859.0, 1081.0),
                ),
            ];
            let (f, t, r) = measured(&two);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 2, "wid 3 starts past the edge: excluded");
            assert_eq!(report.count_over, 2);
            assert_eq!(report.worst_visible_pt, 8.0);
            assert_eq!(report.worst_wsid, 2);

            let clean = [
                (window(1), slot, slot),
                (
                    window(2),
                    rect(4.0, 32.0, 859.0, 1081.0),
                    rect(5.0, 32.0, 859.0, 1081.0),
                ),
            ];
            let (f, t, r) = measured(&clean);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 2);
            assert_eq!(report.count_over, 0, "1pt is within the threshold");
            assert_eq!(report.worst_visible_pt, 1.0);
        }

        #[test]
        fn handover_report_matches_the_brute_force_over_on_screen_intents() {
            let mut rng = Gen(97);
            let mut over_seen = 0usize;
            for _ in 0..RUNS {
                let count = rng.below(6) as usize + 1;
                let mut frames = Vec::with_capacity(count);
                for i in 1..=count {
                    let intended = if rng.coin() {
                        rng.on_screen()
                    } else {
                        rng.park(CGSize::new(859.0, 1081.0))
                    };
                    let shift = if rng.coin() { 0.0 } else { rng.pt(-80.0, 80.0) };
                    let actual = CGRect::new(
                        CGPoint::new(intended.origin.x + shift, intended.origin.y + shift / 2.0),
                        intended.size,
                    );
                    frames.push((window(i as u32), intended, actual));
                }
                let (f, t, r) = measured(&frames);
                let report = handover_report(&f, &t, &r, DISPLAY);

                let visible: Vec<_> = frames
                    .iter()
                    .filter(|(_, intended, _)| !rini_geometry::is_off_screen(DISPLAY, *intended))
                    .collect();
                let error = |intended: &CGRect, actual: &CGRect| {
                    (actual.origin.x - intended.origin.x)
                        .abs()
                        .max((actual.origin.y - intended.origin.y).abs())
                };
                let expected_over = visible.iter().filter(|(_, i, a)| error(i, a) > 2.0).count();
                let expected_worst =
                    visible.iter().map(|(_, i, a)| error(i, a)).fold(0.0, f64::max);
                assert_eq!(report.total, visible.len(), "seed 97");
                assert_eq!(report.count_over, expected_over, "seed 97");
                assert_eq!(report.worst_visible_pt, expected_worst, "seed 97");
                if report.worst_visible_pt > 0.0 {
                    let worst = frames
                        .iter()
                        .find(|(w, _, _)| w.idx.get() == report.worst_wsid)
                        .expect("seed 97: the worst names a measured window");
                    assert!(
                        !rini_geometry::is_off_screen(DISPLAY, worst.1),
                        "seed 97: the worst came from a park"
                    );
                }
                over_seen += expected_over;
            }
            assert!(over_seen > 0, "generator sanity: no misses in {RUNS} runs");
        }
    }

    mod worth_flying {
        use objc2_core_foundation::CGPoint;

        use super::*;
        use crate::animation::domain::motion::plan::{GroupKey, ReflowPlan, RigidGroup, reflow_plan};
        use crate::animation::domain::motion::surface::surface_travel;
        use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, rect};
        use crate::transitions::domain::admission::entrance_reservation;
        use crate::transitions::domain::travel::is_moving;

        fn moving_drawable(tiles: &[(CGRect, CGRect)]) -> bool {
            tiles.iter().any(|(from, to)| is_moving(*from, *to))
        }

        fn shifted(frame: CGRect, dx: f64, dy: f64) -> CGRect {
            CGRect::new(
                CGPoint::new(frame.origin.x + dx, frame.origin.y + dy),
                frame.size,
            )
        }

        fn column(i: f64) -> CGRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        fn moving(plan: &ReflowPlan) -> Vec<&RigidGroup> {
            plan.groups
                .iter()
                .filter(|g| !g.members.is_empty() && g.key != GroupKey::STILL)
                .collect()
        }

        fn members(group: &RigidGroup) -> Vec<WindowId> {
            group.members.iter().map(|m| m.window).collect()
        }

        #[test]
        fn a_pass_where_nothing_drawable_moves_does_not_fly() {
            assert!(
                !worth_flying(false, false),
                "a still-only composition flew, hiding the new window until its picture landed"
            );
        }

        /// A window with no picture is an entrance, not a tile; nothing drawable moves.
        #[test]
        fn a_floating_open_over_a_still_strip_does_not_fly() {
            let s1 = rect(0.0, 32.0, 860.0, 1081.0);
            let s2 = rect(867.0, 32.0, 859.0, 1081.0);
            let tiles = vec![
                (s1, s1),
                (s2, s2),
            ];
            let zoom = rect(224.0, 95.0, 1280.0, 960.0);
            let (entrance, waiting) = entrance_reservation(window(3), zoom, true);
            assert_eq!(entrance.window, window(3));
            assert_eq!(
                waiting,
                (window(3), zoom.size),
                "the entrance is reserved but never drawn here"
            );
            assert!(!worth_flying(moving_drawable(&tiles), false));
        }

        #[test]
        fn a_strip_open_that_moves_a_neighbour_flies() {
            let before = rect(0.0, 32.0, 1720.0, 1081.0);
            let after = rect(0.0, 32.0, 860.0, 1081.0);
            let tiles = vec![(before, after)];
            let (_, waiting) =
                entrance_reservation(window(2), rect(867.0, 32.0, 859.0, 1081.0), false);
            assert_eq!(waiting.0, window(2));
            assert!(worth_flying(moving_drawable(&tiles), false));
        }

        #[test]
        fn a_pan_flies() {
            let frame = rect(867.0, 32.0, 859.0, 1081.0);
            let (from, to) =
                surface_travel(frame, CGPoint::new(0.0, 0.0), CGPoint::new(867.0, 0.0), false);
            let tiles = vec![(from, to)];
            assert!(worth_flying(moving_drawable(&tiles), false));
        }

        #[test]
        fn a_still_pass_joining_a_running_flight_flies() {
            let s1 = rect(0.0, 32.0, 860.0, 1081.0);
            let tiles = vec![(s1, s1)];
            assert!(worth_flying(moving_drawable(&tiles), true));
            assert!(worth_flying(false, true), "even with nothing drawable at all");
        }

        #[test]
        fn any_moving_tile_is_enough_to_fly() {
            let mut rng = Gen(81);
            let mut grounded = 0usize;
            for _ in 0..RUNS {
                let count = rng.below(4) as usize + 1;
                // Half the passes are still-only, so the grounded branch is exercised often.
                let all_still = rng.coin();
                let mut tiles = Vec::with_capacity(count);
                let mut any_moving = false;
                for _ in 0..count {
                    let from = rng.on_screen();
                    let to = if all_still || rng.coin() {
                        from
                    } else {
                        rect(
                            from.origin.x + rng.pt(1.0, 400.0),
                            32.0,
                            from.size.width,
                            from.size.height,
                        )
                    };
                    any_moving |= is_moving(from, to);
                    tiles.push((from, to));
                }
                let running = rng.coin();
                let flies = worth_flying(moving_drawable(&tiles), running);
                assert_eq!(flies, any_moving || running, "seed 81");
                if !flies {
                    grounded += 1;
                }
            }
            assert!(grounded > RUNS / 20, "generator sanity: {grounded} grounded");
        }

        /// The closed window is not in the pass at all.
        #[test]
        fn a_close_composes_only_the_survivors_and_worth_flying_needs_a_mover() {
            let w = 863.0;
            let closed = window(2);
            let survivors: Vec<(WindowId, CGRect, CGRect, bool)> = [1u32, 3, 4]
                .iter()
                .map(|&i| {
                    let f = column(i as f64);
                    let to = if i > 2 { shifted(f, -w, 0.0) } else { f };
                    (window(i), f, to, false)
                })
                .collect();
            let plan = reflow_plan(&survivors, DISPLAY);
            assert!(
                plan.member(closed).is_none(),
                "nothing is drawn for the closed window"
            );
            assert_eq!(moving(&plan).len(), 1);
            assert_eq!(members(moving(&plan)[0]), vec![window(3), window(4)]);
            assert!(!worth_flying(false, false));
            assert!(worth_flying(true, false));
            assert!(worth_flying(false, true));
        }
    }
}
