//! What a flight is, and when it is allowed to do work.
//!
//! One pass over the layout becomes a flight: which windows move as a rigid group, which z-band each
//! belongs to, whether a snapshot may be captured yet, and whether a picture that arrived mid-flight
//! is worth swapping in. Decided over ids, frames and progress fractions, with nothing of Core
//! Animation in it. Design in `src/animation/docs/animation-smoothness.md`.

use objc2_core_foundation::CGRect;
use std::collections::{HashMap, HashSet};

use rini_core::ids::{WindowId, WindowServerId};

use crate::animation::domain::request::SnapshotTarget;
use super::timing::{
    COMPANION_CENTER_SLACK, COMPANION_EXPANSION, HANDOVER_THRESHOLD_PT, REFRESH_APPLY_BEFORE,
};

/// Which of `tiles` to recapture mid-flight: the two ends of a focus change, and nothing else.
/// See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) fn refresh_targets(
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

/// How a fresh group of tiles begins moving.
pub(in crate::animation) enum GroupStart {
    /// Wait one `COALESCE_WINDOW` for the reactor's layout passes to settle. For layout changes.
    Coalesced,
    /// Move now. For strip movements, which arrive once per keystroke.
    Immediate,
}

/// The unmanaged window tracing `frame` as its border, if any. A parked window never traces and is
/// never traced. See "Window borders during animations" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) fn companion_of(
    frame: CGRect,
    candidates: &[(WindowServerId, CGRect)],
    display: CGRect,
) -> Option<(WindowServerId, CGRect)> {
    if rini_geometry::is_off_screen(display, frame) {
        return None;
    }
    let center = |r: CGRect| {
        (r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0)
    };
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

/// Which path composed a flight. See "The apply point" in `src/animation/docs/animation-smoothness.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::animation) enum FlightKind {
    /// A per-window layout pass: moves and resizes.
    Layout,
    /// A strip movement: pure translations.
    Pan,
}

/// What a tile is doing when a picture of its window lands mid-flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::animation) enum TileState {
    /// No tile for this window in the flight.
    NotTiled,
    /// The flight holds for this window's first picture: an entrance reservation.
    Awaiting,
    /// A grow waiting for its reveal, held or flying the placeholder. `fits`: the landed picture
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
pub(in crate::animation) enum SwapDecision {
    /// A held flight takes it at frame zero (`claim_reveal`).
    Claim,
    /// A moving flight adds the entrance late (`admit_entrance`).
    Admit,
    /// Hard-cut onto the moving tile, with the reason logged.
    Swap(&'static str),
    /// Cache only, for the next flight.
    CacheOnly,
}

/// How an incoming picture compares with the one cached for its window, judged before caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::animation) struct CacheComparison {
    /// Renders the same within thumbprint tolerance; non-bitmap pairs count as different.
    pub(in crate::animation) renders_like_cached: bool,
    /// Captured by the same route (`SnapshotSource`) as the cached picture. Routes render a
    /// translucent window differently, so a route change alone reads as a change.
    pub(in crate::animation) same_source: bool,
}

/// Whether a picture landing mid-flight may change what a tile draws. `progress` is `None`
/// before the flight starts moving. See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) fn should_swap_mid_flight(
    state: TileState,
    settled: bool,
    renders_like_cached: bool,
    same_source: bool,
    progress: Option<f64>,
) -> SwapDecision {
    match state {
        TileState::Awaiting | TileState::Reveal { .. } if progress.is_none() => {
            SwapDecision::Claim
        }
        TileState::Awaiting => SwapDecision::Admit,
        TileState::Reveal { fits: true }
            if settled && progress.is_some_and(|p| p < REFRESH_APPLY_BEFORE) =>
        {
            SwapDecision::Swap("reveal")
        }
        TileState::Reveal { .. } => SwapDecision::CacheOnly,
        TileState::MovingRefreshTarget { fits: true, resizing }
            if same_source
                && !renders_like_cached
                && (!resizing || settled)
                && progress.is_some_and(|p| p < REFRESH_APPLY_BEFORE) =>
        {
            SwapDecision::Swap("refresh")
        }
        TileState::NotTiled
        | TileState::Moving { .. }
        | TileState::MovingRefreshTarget { .. } => SwapDecision::CacheOnly,
    }
}

/// Where a flight is between composition and lift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::animation) enum FlightPhase {
    /// No flight.
    Idle,
    /// Composed, overlay up, not yet moving, nothing awaited.
    FrameZero,
    /// Overlay up, waiting at frame zero for reveal or entrance pictures.
    Holding,
    /// Tiles in motion.
    Moving,
}

/// A request to the window server that a flight might make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::animation) enum CaptureKind {
    /// Background captures for windows the reactor named (`warm_windows`).
    Warm,
    /// The full-display desktop render (`warm_desktop`).
    Desktop,
    /// The destination recapture (`refresh_destination_among`).
    Refresh,
    /// A reveal or entrance chase (`chase_reveal_pictures`).
    Chase,
    /// A hairline harvest for a landed snapshot.
    Harvest,
    /// A capture for a window SkyLight could not serve at composition.
    NeedsCapture,
}

/// Whether a flight in `phase` may start `kind` of capture work now.
/// See "Capture work in flight" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) fn capture_work_allowed(phase: FlightPhase, kind: CaptureKind) -> bool {
    match phase {
        FlightPhase::Idle => true,
        FlightPhase::FrameZero => matches!(kind, CaptureKind::Chase | CaptureKind::NeedsCapture),
        FlightPhase::Holding => matches!(kind, CaptureKind::Chase),
        FlightPhase::Moving => matches!(kind, CaptureKind::Chase | CaptureKind::Refresh),
    }
}

/// Parks warm targets asked for mid-flight, one per window; the latest request wins.
pub(in crate::animation) fn defer_warm(deferred: &mut Vec<SnapshotTarget>, targets: Vec<SnapshotTarget>) {
    for target in targets {
        match deferred.iter_mut().find(|held| held.window == target.window) {
            Some(held) => *held = target,
            None => deferred.push(target),
        }
    }
}

/// Which animated windows `finish` harvests a hairline for: each at most once per flight.
pub(in crate::animation) fn finish_harvest_set(
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
            !harvested.contains(w) && !requested.contains(w) && !dressed.contains(w) && seen.insert(*w)
        })
        .collect()
}

/// Whether an in-flight merge leaves the already-applied frames stale. A parked window has no
/// tile, so `frames_changed` counts too. See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) fn mark_stale_on_untiled_change(changed: bool, frames_changed: bool) -> bool {
    changed || frames_changed
}

/// How far the real windows were from their tiles at lift. See `report_handover_error`.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::animation) struct HandoverReport {
    /// Windows measured: tiled, answered by the server, intended on screen.
    pub(in crate::animation) total: usize,
    /// Windows more than `HANDOVER_THRESHOLD_PT` off.
    pub(in crate::animation) count_over: usize,
    /// The largest error among the windows the report counts.
    pub(in crate::animation) worst_visible_pt: f64,
    pub(in crate::animation) worst_wsid: u32,
}

/// Measures every tiled window's real frame against its intended one. Parks are excluded: macOS
/// clamps them. See "Real windows land before lift" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) fn handover_report(
    final_frames: &[(WindowId, CGRect)],
    tiled: &[WindowId],
    real: &HashMap<WindowId, CGRect>,
    display: CGRect,
) -> HandoverReport {
    let mut report =
        HandoverReport { total: 0, count_over: 0, worst_visible_pt: 0.0, worst_wsid: 0 };
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

/// Share of samples that may differ and still count as the same rendering: forgives a blinking
/// cursor, not a half-painted surface. See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) const STABLE_MAX_DIFFERING: f64 = 0.03;
pub(in crate::animation) const STABLE_CHANNEL_TOLERANCE: u8 = 8;

/// Whether two consecutive thumbprints show the same rendering.
pub(in crate::animation) fn renderings_match(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    let differing = a
        .iter()
        .zip(b)
        .filter(|(x, y)| x.abs_diff(**y) > STABLE_CHANNEL_TOLERANCE)
        .count();
    (differing as f64) <= (a.len() as f64) * STABLE_MAX_DIFFERING
}

/// Whether a chase capture counts as the window's settled rendering.
/// See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) fn chase_settled(prev: Option<&[u8]>, print: &[u8], pre_resize: Option<&[u8]>) -> bool {
    prev.is_some_and(|previous| renderings_match(previous, print))
        || pre_resize.is_some_and(|before| !renderings_match(before, print))
}

/// What a settled picture did for a flight holding at frame zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::animation) enum Claimed {
    /// Taken; other holds remain.
    Held,
    /// Taken, and it was the last hold: the flight may start moving.
    Released,
    /// Taken by a composed tile standing at frame zero; no hold was involved.
    Refreshed,
}

/// Whether a composed pass is worth an overlay flight.
/// See "Layout changes" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::animation) fn worth_flying(moving_drawable: bool, running: bool) -> bool {
    moving_drawable || running
}

/// The window server ids a border companion may never be: everything rini manages. Synthetic
/// (companion) ids carry pid 0 and are left out, or a border seen once could never match again.
pub(in crate::animation) fn managed_server_ids(
    pass: &std::collections::HashSet<u32>,
    cached: impl Iterator<Item = WindowId>,
    owed: impl Iterator<Item = WindowId>,
) -> std::collections::HashSet<u32> {
    let mut ids = pass.clone();
    ids.extend(cached.chain(owed).filter(|w| w.pid != 0).map(|w| w.idx.get()));
    ids
}

/// Which group a window belongs to.
pub(in crate::animation) fn group_of(floating: bool) -> crate::animation::domain::motion::z_group::StackGroup {
    if floating {
        crate::animation::domain::motion::z_group::StackGroup::Floating
    } else {
        crate::animation::domain::motion::z_group::StackGroup::Tiled
    }
}

/// The group drawn in front: the focus target's, or the strip when it is not being animated.
pub(in crate::animation) fn focus_group(
    focus: Option<WindowId>,
    mut windows: impl Iterator<Item = (WindowId, bool)>,
) -> crate::animation::domain::motion::z_group::StackGroup {
    let Some(focus) = focus else { return crate::animation::domain::motion::z_group::StackGroup::Tiled };
    windows
        .find(|(window, _)| *window == focus)
        .map(|(_, floating)| group_of(floating))
        .unwrap_or(crate::animation::domain::motion::z_group::StackGroup::Tiled)
}

/// A stable [`WindowId`] derived from a window server id. Pid 0 keeps it clear of real ids.
pub(in crate::animation) fn synthetic_window_id(server_id: WindowServerId) -> WindowId {
    WindowId { pid: 0, idx: std::num::NonZeroU32::new(server_id.as_u32().max(1)).unwrap() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rendering_matches_itself_through_cursor_noise_but_not_through_repaints() {
        let a = vec![100u8; 4096];
        assert!(renderings_match(&a, &a), "identical");
        let mut cursor = a.clone();
        for value in cursor.iter_mut().take(80) {
            *value = 200; // ~2% of samples
        }
        assert!(renderings_match(&a, &cursor), "cursor-sized noise still matches");
        let mut repaint = a.clone();
        for value in repaint.iter_mut().take(1024) {
            *value = 200; // a quarter of the image
        }
        assert!(!renderings_match(&a, &repaint), "a repaint does not");
    }

    #[test]
    fn mismatched_or_empty_thumbprints_never_match() {
        assert!(!renderings_match(&[1, 2, 3], &[1, 2]));
        assert!(!renderings_match(&[], &[]));
    }
}
