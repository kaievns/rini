//! Drives the capture-based animation overlay: owns the overlay and the snapshot cache.
//! Runs on the main thread because Core Animation requires it.
//!
//! Design in `docs/animation-smoothness.md`; measurements in `docs/capture-overlay-research.md`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::MainThreadMarker;
use tracing::{debug, warn};

use rini_runloop::channel;
use rini_core::ids::WindowId;
use rini_geometry::SameAs;
use rini_runloop::run_loop::RepeatingTimer;
use rini_core::ids::WindowServerId;
use crate::snapshot_service::{SnapshotService, SnapshotTarget};
use crate::window_snapshot::{
    SnapshotCache, WindowSnapshot, capture_via_framed_with_dressing,
};
use crate::overlay::{OverlayTile, TileOverlay};

pub(crate) use crate::motion::plan;
use crate::motion::travel::{
    is_moving, neighbour_travel, resolve_end, resolve_start, travel_subject, worth_animating,
};
pub use crate::motion::surface::SurfaceWindow;
pub(crate) use crate::motion::surface::{pan_travel, surface_travel, to_overlay_space};


/// One window's part in an animation, as the caller describes it.
#[derive(Debug, Clone)]
pub struct AnimationRequest {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// Frame the window is leaving, in display coordinates.
    pub from: CGRect,
    /// Frame the window is arriving at, in display coordinates.
    pub to: CGRect,
    /// Off the strip, and so in the other z-order group.
    pub floating: bool,
}

#[derive(Debug)]
pub enum Event {
    /// Animate a set of windows. The caller must have already placed the real windows at their
    /// final frames, or arrange to do so immediately after sending this.
    Animate { windows: Vec<AnimationRequest>, focus: Option<WindowId>, duration: Duration },
    /// Display geometry for the overlay. Must be the USABLE frame, excluding the menu bar strip,
    /// so the user's bar is not covered and made to flicker.
    SetDisplay { id: u32, frame: CGRect, scale: f64 },
    /// Drop snapshots for windows that no longer exist, so the cache cannot grow without bound.
    ForgetWindow(WindowId),
    /// Slide every currently visible window in from an offset, purely to evaluate animation quality
    /// by eye. Does not touch any real window, so it is safe to fire at any time.
    DebugSlide { dx: f64, dy: f64, duration: Duration },
    /// Move the whole strip surface by one travel, as one rigid group; a leaving window animates
    /// off screen while its real frame parks. See "Strip movements" in `docs/animation-smoothness.md`.
    AnimateSurface {
        windows: Vec<SurfaceWindow>,
        from_offset: CGPoint,
        to_offset: CGPoint,
        /// Real screen frames to apply once the overlay is covering them.
        final_frames: Vec<(WindowId, CGRect)>,
        /// The window that will hold focus once this settles, drawn in front of the rest.
        focus: Option<WindowId>,
        duration: Duration,
    },
    /// Nudge the strip surface by `overshoot` and bring it back; real windows stay put. Rides an
    /// in-flight movement additively. See "Edge bounce" in `docs/animation-smoothness.md`.
    Bounce {
        windows: Vec<SurfaceWindow>,
        overshoot: CGPoint,
        final_frames: Vec<(WindowId, CGRect)>,
        focus: Option<WindowId>,
        duration: Duration,
    },
    /// One frame of the running animation. Posted by the run loop timer.
    Tick,
    /// The layout passes have settled; start the clock. Posted by the coalesce timer.
    StartMoving,
    /// No flight began for `SETTLE_BEFORE_CAPTURES` after a lift; run the owed captures.
    Quiet,
    /// A background capture has landed. Posted by the snapshot service.
    SnapshotsReady,
    /// A framed recapture has landed. `settled` (`chase_settled`) means the window has repainted;
    /// only settled pictures may satisfy a reveal hold or replace a resizing tile's picture.
    PictureReady { window: WindowId, snapshot: WindowSnapshot, settled: bool },
    /// A hairline harvest finished. Harvested off the capture service's completion queue, which
    /// the framed capture behind it would deadlock (see `snapshot_service`).
    DressingReady { window: WindowId, dressing: crate::edge_dressing::EdgeDressing },
    /// Recapture the bar, now that nothing is animating over it. Posted by the refresh timer.
    RefreshBar,
    /// Recapture this window because focus moved to or from it: focus changes the rendering
    /// without changing the size, so the ordinary warm's size test cannot see it.
    RefreshFocus(SnapshotTarget),
    /// Warm the cache for these windows. Only queues background work.
    /// Targets come from the reactor because only it knows each window's real [`WindowId`].
    WarmWindows(Vec<SnapshotTarget>),
    /// Warm from the window server rather than rini's window table. Debug command only.
    WarmCache,
}

/// Called with real-window frames to apply while the overlay covers them.
pub type PlaceFrames = Box<dyn Fn(Vec<(WindowId, CGRect)>)>;

pub type Sender = channel::Sender<Event>;
pub type Receiver = channel::Receiver<Event>;

/// Tick interval. Nothing is drawn on ticks; this only paces the mid-flight orchestration.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);



/// How long to collect the reactor's layout passes before the movement starts.
/// See "Layout changes" in `docs/animation-smoothness.md`.
const COALESCE_WINDOW: Duration = Duration::from_millis(25);

/// Progress at which the focus change's two ends are recaptured, once per flight.
/// See "Mid-flight passes" in `docs/animation-smoothness.md`.
const REFRESH_DESTINATION_AT: f64 = 0.5;

/// A refresh landing at or after this progress is cached only; a later cut reads as lift flicker.
const REFRESH_APPLY_BEFORE: f64 = 0.6;

/// How long after an animation to recapture the bar: a bar composite is too slow (31ms median)
/// to pay per switch, so a burst of switches pays it once, at the end.
const BAR_REFRESH_DELAY: Duration = Duration::from_millis(250);

/// Which of `tiles` to recapture mid-flight: the two ends of a focus change, and nothing else.
/// See "Mid-flight passes" in `docs/animation-smoothness.md`.
fn refresh_targets(
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

/// The destination refresh's requests: one ScreenCaptureKit target per wanted window, and the
/// windows covered. One route only, so the refresh compares like with like against the cache.
fn refresh_requests(
    tiles: &[(WindowId, WindowServerId, CGSize)],
    wanted: &[WindowId],
) -> (Vec<WindowId>, Vec<SnapshotTarget>) {
    let requests: Vec<SnapshotTarget> = wanted
        .iter()
        .filter_map(|window| tiles.iter().find(|(w, _, _)| w == window))
        .map(|&(window, server_id, size)| SnapshotTarget { window, server_id, size })
        .collect();
    let covered = requests.iter().map(|t| t.window).collect();
    (covered, requests)
}

/// Progress at which a move-only layout flight places the real windows. Resizes and strips place
/// earlier (`apply_frames_at`). See "The apply point" in `docs/animation-smoothness.md`.
const APPLY_FRAMES_AT: f64 = 0.75;

/// How a fresh group of tiles begins moving.
enum GroupStart {
    /// Wait one `COALESCE_WINDOW` for the reactor's layout passes to settle. For layout changes.
    Coalesced,
    /// Move now. For strip movements, which arrive once per keystroke.
    Immediate,
}

/// How much larger than the window it traces a border window may be, per axis.
/// See "Window borders during animations" in `docs/animation-smoothness.md`.
const COMPANION_EXPANSION: f64 = 8.0;

/// How far the centers may disagree. The border window is centered on what it traces.
const COMPANION_CENTER_SLACK: f64 = 4.0;

/// The unmanaged window tracing `frame` as its border, if any. A parked window never traces and is
/// never traced. See "Window borders during animations" in `docs/animation-smoothness.md`.
fn companion_of(
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


struct RunningAnimation {
    tiles: Vec<OverlayTile>,
    /// Where each window must end up, in display coordinates. Sent once the overlay covers them.
    final_frames: Vec<(WindowId, CGRect)>,
    frames_applied: bool,
    /// `None` while still collecting windows: on screen but not yet moving.
    started: Option<Instant>,
    duration: Duration,
    /// Progress at which the real windows are placed (`apply_frames_at`).
    apply_at: f64,
    /// Windows waiting for a first picture; each also holds in `awaiting`.
    entrances: Vec<PendingEntrance>,
    /// Windows whose pixels are still rendering, with the size that counts as ready. The flight
    /// holds at frame zero until this empties or `hold_deadline` passes.
    awaiting: Vec<(WindowId, CGSize)>,
    /// When to stop waiting for reveal pixels and fly the placeholder (`reveal_hold_limit`).
    hold_deadline: Option<Instant>,
    /// Whether the focus change's ends have been recaptured. See `refresh_destination_among`.
    destination_refreshed: bool,
    /// Windows the refresh recaptured: the only tiles that may take a picture mid-flight.
    refresh_targets: Vec<WindowId>,
    /// Windows whose hairline landed this flight, so `finish` harvests nothing twice.
    harvested: HashSet<WindowId>,
    /// The window gaining focus, from the latest pass that named one; its group is banded in front.
    focus: Option<WindowId>,
    /// The flight as rigid pieces: what `install` composed and `fly` animates.
    /// See "The overlay engine" in `docs/animation-smoothness.md`.
    plan: plan::FlightPlan,
    /// Dropped when the animation ends, which invalidates the timer.
    _clock: Option<RepeatingTimer>,
}

/// A window that joins the animation as soon as it has a picture. The flight holds at frame zero
/// for it. See "The reservation fallback" in `docs/animation-smoothness.md`.
#[derive(Debug, Clone)]
struct PendingEntrance {
    window: WindowId,
    /// Destination, in the overlay's coordinate space.
    to: CGRect,
    floating: bool,
}

/// Where an entering window grows in from: zero width at its own left edge, full height.
fn entrance_from(to: CGRect) -> CGRect {
    CGRect::new(to.origin, CGSize::new(0.0, to.size.height))
}

/// The earlier apply point when a window resizes: the resize costs three synchronous round trips
/// into the owning app. See "The apply point" in `docs/animation-smoothness.md`.
const APPLY_FRAMES_AT_RESIZE: f64 = 0.5;

/// The apply point for a strip movement: frame zero, so a switch's serialized AX writes land
/// before lift. See "The apply point" in `docs/animation-smoothness.md`.
const APPLY_FRAMES_AT_PAN: f64 = 0.0;

/// Which path composed a flight. See "The apply point" in `docs/animation-smoothness.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlightKind {
    /// A per-window layout pass: moves and resizes.
    Layout,
    /// A strip movement: pure translations.
    Pan,
}

/// Which apply point an animation needs.
fn apply_frames_at(kind: FlightKind, any_resize: bool) -> f64 {
    match (kind, any_resize) {
        (_, true) => APPLY_FRAMES_AT_RESIZE,
        (FlightKind::Layout, false) => APPLY_FRAMES_AT,
        (FlightKind::Pan, false) => APPLY_FRAMES_AT_PAN,
    }
}

/// What a tile is doing when a picture of its window lands mid-flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileState {
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
enum SwapDecision {
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
struct CacheComparison {
    /// Renders the same within thumbprint tolerance; non-bitmap pairs count as different.
    renders_like_cached: bool,
    /// Captured by the same route (`SnapshotSource`) as the cached picture. Routes render a
    /// translucent window differently, so a route change alone reads as a change.
    same_source: bool,
}

/// Whether a picture landing mid-flight may change what a tile draws. `progress` is `None`
/// before the flight starts moving. See "Mid-flight passes" in `docs/animation-smoothness.md`.
fn should_swap_mid_flight(
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
enum FlightPhase {
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
enum CaptureKind {
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
/// See "Capture work in flight" in `docs/animation-smoothness.md`.
fn capture_work_allowed(phase: FlightPhase, kind: CaptureKind) -> bool {
    match phase {
        FlightPhase::Idle => true,
        FlightPhase::FrameZero => matches!(kind, CaptureKind::Chase | CaptureKind::NeedsCapture),
        FlightPhase::Holding => matches!(kind, CaptureKind::Chase),
        FlightPhase::Moving => matches!(kind, CaptureKind::Chase | CaptureKind::Refresh),
    }
}

/// Parks warm targets asked for mid-flight, one per window; the latest request wins.
fn defer_warm(deferred: &mut Vec<SnapshotTarget>, targets: Vec<SnapshotTarget>) {
    for target in targets {
        match deferred.iter_mut().find(|held| held.window == target.window) {
            Some(held) => *held = target,
            None => deferred.push(target),
        }
    }
}

/// Which animated windows `finish` harvests a hairline for: each at most once per flight.
fn finish_harvest_set(
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

/// Whether `finish` should ask for a new desktop render: missing, for another display, or stale.
fn desktop_render_wanted(render: Option<(Duration, (f64, f64))>, display: (f64, f64)) -> bool {
    match render {
        None => true,
        Some((age, covered)) => {
            !crate::window_snapshot::spans_display(covered, display)
                || crate::window_snapshot::picture_is_stale(age)
        }
    }
}

/// Whether an in-flight merge leaves the already-applied frames stale. A parked window has no
/// tile, so `frames_changed` counts too. See "Mid-flight passes" in `docs/animation-smoothness.md`.
fn mark_stale_on_untiled_change(changed: bool, frames_changed: bool) -> bool {
    changed || frames_changed
}

/// A real window further than this from its intended frame at lift is a handover miss.
const HANDOVER_THRESHOLD_PT: f64 = 2.0;

/// How far the real windows were from their tiles at lift. See `report_handover_error`.
#[derive(Debug, Clone, PartialEq)]
struct HandoverReport {
    /// Windows measured: tiled, answered by the server, intended on screen.
    total: usize,
    /// Windows more than `HANDOVER_THRESHOLD_PT` off.
    count_over: usize,
    /// The largest error among the windows the report counts.
    worst_visible_pt: f64,
    worst_wsid: u32,
}

/// Measures every tiled window's real frame against its intended one. Parks are excluded: macOS
/// clamps them. See "Real windows land before lift" in `docs/animation-smoothness.md`.
fn handover_report(
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

/// Whether a chase capture counts as the window's settled rendering.
/// See "A grow holds, then reveals" in `docs/animation-smoothness.md`.
fn chase_settled(prev: Option<&[u8]>, print: &[u8], pre_resize: Option<&[u8]>) -> bool {
    use crate::edge_dressing::renderings_match;
    prev.is_some_and(|previous| renderings_match(previous, print))
        || pre_resize.is_some_and(|before| !renderings_match(before, print))
}

/// The thumbprint of a snapshot's bitmap; `None` for a surface, which cannot be compared.
fn bitmap_thumbprint(snapshot: &WindowSnapshot) -> Option<Vec<u8>> {
    match &snapshot.image {
        crate::window_snapshot::SnapshotImage::Bitmap(image) => {
            crate::edge_dressing::thumbprint(image)
        }
        _ => None,
    }
}

/// The longest a flight stands still at frame zero for a reveal.
/// See "A grow holds, then reveals" in `docs/animation-smoothness.md`.
const HOLD_CAP: Duration = Duration::from_millis(300);

/// How long a grow may hold at frame zero for its reveal pixels, capped at `HOLD_CAP`.
fn reveal_hold_limit(duration: Duration) -> Duration {
    duration.mul_f64(0.4).max(Duration::from_millis(300)).min(HOLD_CAP)
}

/// Time a holding flight still waits before flying the placeholder; `None` once past the deadline.
fn hold_wait(hold_deadline: Option<Instant>, now: Instant) -> Option<Duration> {
    let deadline = hold_deadline?;
    (now < deadline).then(|| (deadline - now).max(Duration::from_millis(10)))
}

/// Chase poll interval and attempt budget for a growing window's real frame (about a second).
/// See "A grow holds, then reveals" in `docs/animation-smoothness.md`.
const REVEAL_CHASE_INTERVAL: Duration = Duration::from_millis(8);
const REVEAL_CHASE_ATTEMPTS: usize = 125;

/// What became of a tile offered to an animation in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admitted {
    /// Same window, same destination: a redundant pass. Nothing restarts, so rapid presses
    /// neither restart nor extend the flight.
    Redundant,
    /// Same window, new destination: the tile bends toward it mid-flight.
    Retargeted,
    /// A window this animation had not seen yet.
    Joined,
}

/// The merge decision for one tile.
fn merge_action(current_to: Option<CGRect>, incoming_to: CGRect) -> Admitted {
    match current_to {
        Some(to) if to.same_as(incoming_to) => Admitted::Redundant,
        Some(_) => Admitted::Retargeted,
        None => Admitted::Joined,
    }
}

/// Folds a later pass's destinations into the flight's; latest frame per window wins.
fn merge_final_frames(
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
/// for `merge_pass` to retarget. See "Mid-flight passes" in `docs/animation-smoothness.md`.
fn retarget_entrances(
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

/// Writes every tile's destination back from the merged plan: a member the pass did not compose
/// rides its group all the same.
fn sync_tiles_to_plan(tiles: &mut [OverlayTile], plan: &plan::FlightPlan) {
    for tile in tiles.iter_mut() {
        let Some(member) = plan.member(tile.window) else { continue };
        tile.to = match member {
            plan::Member::Rigid { key, rel } => plan::overlay_of(rel, plan.position_of(key)),
            plan::Member::Changing { to, .. } | plan::Member::Entrance { to, .. } => to,
            plan::Member::Floating { to, .. } => {
                plan::overlay_of(to, plan.position_of(plan::GroupKey::Floating))
            }
        };
    }
}


/// The frames a coalescing merge must send again: `step` will not place frame-zero frames twice.
/// See "The reservation fallback" in `docs/animation-smoothness.md`.
fn reapply_set(
    frames_applied: bool,
    in_flight: bool,
    changed: bool,
    final_frames: &[(WindowId, CGRect)],
) -> Option<Vec<(WindowId, CGRect)>> {
    (frames_applied && !in_flight && changed).then(|| final_frames.to_vec())
}

/// How long a tile joining a flight already in motion travels: what is left of the flight.
fn late_join_duration(duration: Duration, progress: f64) -> Duration {
    duration.mul_f64((1.0 - progress).max(0.0))
}

/// A newly opened window's reservation, and the hold entry it adds to `awaiting`.
fn entrance_reservation(
    window: WindowId,
    to: CGRect,
    floating: bool,
) -> (PendingEntrance, Option<(WindowId, CGSize)>) {
    (PendingEntrance { window, to, floating }, Some((window, to.size)))
}

/// How long after a lift the flight's owed captures wait for the user to stop pressing.
/// See "Capture work in flight" in `docs/animation-smoothness.md`.
const SETTLE_BEFORE_CAPTURES: Duration = Duration::from_millis(400);

/// The capture work a lift leaves for the quiet period.
#[derive(Default)]
struct AfterFlight {
    targets: Vec<SnapshotTarget>,
    harvested: HashSet<WindowId>,
}

/// How long past its clock a flight waits for the render server and the real windows before
/// lifting anyway. See "Real windows land before lift" in `docs/animation-smoothness.md`.
const LIFT_GRACE: Duration = Duration::from_millis(350);

/// The flight's clock once a bounce joins it: long enough for the return leg, never shorter.
fn clock_for_bounce(started: Option<Instant>, duration: Duration, bounce: Duration) -> Duration {
    let needed = started.map_or(bounce, |s| s.elapsed() + bounce);
    duration.max(needed)
}

/// Whether the overlay lifts now: clock done AND (presented and landed, or `LIFT_GRACE` overdue).
fn lift_now(clock_done: bool, settled: bool, landed: bool, overdue: bool) -> bool {
    clock_done && ((settled && landed) || overdue)
}

/// How many new windows one pass captures synchronously at spawn; the rest take a reservation.
/// See "A window that opens travels from its spawn frame" in `docs/animation-smoothness.md`.
const MAX_SYNC_ENTRANCE_CAPTURES: usize = 4;

/// How a newly opened window enters a flight.
#[derive(Debug, Clone, Copy, PartialEq)]
enum EntranceDecision {
    /// Its tile travels from the frame macOS showed it at to its slot.
    Travel { from: CGRect, to: CGRect },
    /// No usable picture at spawn: a reservation held for the first picture, with the reason.
    Reserve(&'static str),
}

/// `Travel` iff the server reports a sized frame, the spawn capture is usable and budget remains.
fn entrance_plan(
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
fn frame_zero_work(
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
    let now_frames = if holding { final_frames.to_vec() } else { entrance_frames.to_vec() };
    (holding, chase_set, now_frames)
}

/// The tile for a reserved entrance whose picture has landed: frontmost and focused, since a
/// window is raised on open and about to hold focus.
fn entrance_tile(entrance: &PendingEntrance, snapshot: &WindowSnapshot) -> OverlayTile {
    OverlayTile {
        window: entrance.window,
        from: entrance_from(entrance.to),
        to: entrance.to,
        snapshot: snapshot.clone(),
        floating: entrance.floating,
        server_order: Some(0),
        depth: 0,
        companion: false,
        focused: true,
    }
}

/// What a settled picture did for a flight holding at frame zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Claimed {
    /// Taken; other holds remain.
    Held,
    /// Taken, and it was the last hold: the flight may start moving.
    Released,
    /// Taken by a composed tile standing at frame zero; no hold was involved.
    Refreshed,
}

/// Whether a composed pass is worth an overlay flight.
/// See "Layout changes" in `docs/animation-smoothness.md`.
fn worth_flying(moving_drawable: bool, running: bool) -> bool {
    moving_drawable || running
}

/// Depth for every tile, banded by z-group (`tile_depth`); companions keep their window's depth.
/// The reactor's regroup matches it. See "Mid-flight passes" in `docs/animation-smoothness.md`.
fn restack(tiles: &mut [OverlayTile], focus: Option<WindowId>) {
    let focused_group = focus_group(focus, tiles.iter().map(|t| (t.window, t.floating)));
    for tile in tiles.iter_mut().filter(|t| !t.companion) {
        tile.depth = crate::motion::z_group::tile_depth(
            tile.server_order,
            focus == Some(tile.window),
            group_of(tile.floating),
            focused_group,
        );
    }
}

/// The flight's z-order as containers: `container_z - within` reproduces `-tile_depth`.
/// See "The overlay engine" in `docs/animation-smoothness.md`.
fn band_plan(
    plan: &plan::FlightPlan,
    tiles: &[OverlayTile],
    focus: Option<WindowId>,
) -> plan::Banding {
    use crate::motion::z_group::{GROUP_STRIDE, StackGroup, tile_depth};
    let focused_group = focus_group(focus, tiles.iter().map(|t| (t.window, t.floating)));
    let within: HashMap<WindowId, usize> = tiles
        .iter()
        .map(|t| {
            let group = group_of(t.floating);
            let depth = if t.companion {
                // Its window's banded depth, less the band.
                t.depth % GROUP_STRIDE
            } else {
                tile_depth(t.server_order, focus == Some(t.window), group, group)
            };
            (t.window, depth)
        })
        .collect();
    let mut strip: Vec<(plan::GroupKey, bool, usize)> = Vec::new();
    for group in plan.groups.iter().filter(|g| !g.members.is_empty()) {
        let holds_focus = focus.is_some_and(|f| group.members.iter().any(|m| m.window == f));
        let shallowest =
            group.members.iter().filter_map(|m| within.get(&m.window)).copied().min().unwrap_or(0);
        strip.push((group.key, holds_focus, shallowest));
    }
    if !plan.changing.is_empty() || !plan.entrances.is_empty() {
        let loose: Vec<WindowId> =
            plan.changing.iter().chain(&plan.entrances).map(|(w, _, _)| *w).collect();
        let holds_focus = focus.is_some_and(|f| loose.contains(&f));
        let shallowest = loose.iter().filter_map(|w| within.get(w)).copied().min().unwrap_or(0);
        strip.push((plan::GroupKey::Loose, holds_focus, shallowest));
    }
    strip.sort_by_key(|(_, holds_focus, shallowest)| (!*holds_focus, *shallowest));
    plan::Banding {
        floating_in_front: focused_group == StackGroup::Floating,
        within,
        group_order: strip.into_iter().map(|(key, _, _)| key).collect(),
    }
}

/// The window server ids a border companion may never be: everything rini manages. Synthetic
/// (companion) ids carry pid 0 and are left out, or a border seen once could never match again.
fn managed_server_ids(
    pass: &std::collections::HashSet<u32>,
    cached: impl Iterator<Item = WindowId>,
    owed: impl Iterator<Item = WindowId>,
) -> std::collections::HashSet<u32> {
    let mut ids = pass.clone();
    ids.extend(cached.chain(owed).filter(|w| w.pid != 0).map(|w| w.idx.get()));
    ids
}

/// Which group a window belongs to.
fn group_of(floating: bool) -> crate::motion::z_group::StackGroup {
    if floating {
        crate::motion::z_group::StackGroup::Floating
    } else {
        crate::motion::z_group::StackGroup::Tiled
    }
}

/// The group drawn in front: the focus target's, or the strip when it is not being animated.
fn focus_group(
    focus: Option<WindowId>,
    mut windows: impl Iterator<Item = (WindowId, bool)>,
) -> crate::motion::z_group::StackGroup {
    let Some(focus) = focus else { return crate::motion::z_group::StackGroup::Tiled };
    windows
        .find(|(window, _)| *window == focus)
        .map(|(_, floating)| group_of(floating))
        .unwrap_or(crate::motion::z_group::StackGroup::Tiled)
}

impl RunningAnimation {
    /// Progress from the clock, not a frame count, so a late frame skips instead of stretching.
    fn progress(&self) -> f64 {
        let Some(started) = self.started else {
            return 0.0;
        };
        if self.duration.is_zero() {
            return 1.0;
        }
        let elapsed = started.elapsed().as_secs_f64();
        (elapsed / self.duration.as_secs_f64()).clamp(0.0, 1.0)
    }

    fn is_done(&self) -> bool {
        self.started.is_some() && self.progress() >= 1.0
    }

    /// Past the clock by more than `LIFT_GRACE`: lift whether or not the tiles report settled.
    fn overdue(&self) -> bool {
        self.started.is_some_and(|started| started.elapsed() > self.duration + LIFT_GRACE)
    }

    /// Wall-clock time left before the overlay lifts.
    fn remaining(&self) -> Duration {
        self.duration.mul_f64((1.0 - self.progress()).max(0.0))
    }

    fn phase(&self) -> FlightPhase {
        match (self.started.is_some(), self.awaiting.is_empty()) {
            (true, _) => FlightPhase::Moving,
            (false, false) => FlightPhase::Holding,
            (false, true) => FlightPhase::FrameZero,
        }
    }

    /// `progress` as `should_swap_mid_flight` wants it: `None` until the flight starts moving.
    fn progress_if_started(&self) -> Option<f64> {
        self.started.map(|_| self.progress())
    }

    /// Whether the destination refresh is due at `progress`; takes the one slot when it is.
    fn take_refresh(&mut self, progress: f64) -> bool {
        let due = !self.destination_refreshed
            && progress >= REFRESH_DESTINATION_AT
            && capture_work_allowed(self.phase(), CaptureKind::Refresh);
        if due {
            self.destination_refreshed = true;
        }
        due
    }

    /// What `window` is doing in this flight when a picture of it lands, for `should_swap_mid_flight`.
    fn tile_state(&self, window: WindowId, snapshot: &WindowSnapshot) -> TileState {
        if self.entrances.iter().any(|e| e.window == window) {
            return TileState::Awaiting;
        }
        if let Some((_, size)) = self.awaiting.iter().find(|(w, _)| *w == window) {
            return TileState::Reveal { fits: snapshot.fits(*size) };
        }
        let Some(tile) = self.tiles.iter().find(|tile| tile.window == window) else {
            return TileState::NotTiled;
        };
        let fits = snapshot.fits(tile.to.size);
        if crate::window_snapshot::outgrows(tile.snapshot.coverage.covered, tile.to.size) {
            return TileState::Reveal { fits };
        }
        let resizing = crate::window_snapshot::is_a_resize(tile.from.size, tile.to.size);
        if self.refresh_targets.contains(&window) {
            TileState::MovingRefreshTarget { fits, resizing }
        } else {
            TileState::Moving { fits, resizing }
        }
    }

    /// A later pass carrying reveal holds. A grow can only extend a hold, never stop a moving
    /// flight. Returns the frames a held merge must apply now, so the app can rerender.
    fn extend_hold(
        &mut self,
        awaiting: &[(WindowId, CGSize)],
        in_flight: bool,
        duration: Duration,
        now: Instant,
    ) -> Option<Vec<(WindowId, CGRect)>> {
        if in_flight || awaiting.is_empty() {
            return None;
        }
        for (window, size) in awaiting.iter().copied() {
            if let Some(waiting) = self.awaiting.iter_mut().find(|(w, _)| *w == window) {
                waiting.1 = size;
            } else {
                self.awaiting.push((window, size));
            }
        }
        if self.hold_deadline.is_none() {
            self.hold_deadline = Some(now + reveal_hold_limit(duration));
        }
        self.frames_applied = true;
        Some(self.final_frames.clone())
    }

    /// The frames the flight owes the reactor at `progress`: all of them, once at the apply point.
    fn frames_due(&mut self, progress: f64) -> Option<Vec<(WindowId, CGRect)>> {
        if progress < self.apply_at || self.frames_applied {
            return None;
        }
        self.frames_applied = true;
        Some(self.final_frames.clone())
    }

    /// Takes a settled picture for a window this flight is holding for. The picture must fit the
    /// destination, for entrances too. See "The reservation fallback" in `docs/animation-smoothness.md`.
    fn claim(&mut self, window: WindowId, snapshot: &WindowSnapshot) -> Option<Claimed> {
        if self.started.is_some() {
            return None;
        }
        let Some(position) = self.awaiting.iter().position(|(w, _)| *w == window) else {
            // Not a hold: a spawn-frame newcomer whose chase landed before the flight moved.
            let tile = self.tiles.iter_mut().find(|tile| tile.window == window)?;
            if !snapshot.is_usable() || !snapshot.fits(tile.to.size) {
                return None;
            }
            tile.snapshot = snapshot.clone();
            return Some(Claimed::Refreshed);
        };
        let (_, size) = self.awaiting[position];
        if !snapshot.is_usable() || !snapshot.fits(size) {
            return None;
        }
        self.awaiting.remove(position);
        if let Some(at) = self.entrances.iter().position(|e| e.window == window) {
            let entrance = self.entrances.remove(at);
            self.merge(entrance_tile(&entrance, snapshot));
            restack(&mut self.tiles, self.focus);
        } else if let Some(tile) = self.tiles.iter_mut().find(|tile| tile.window == window) {
            tile.snapshot = snapshot.clone();
        }
        Some(if self.awaiting.is_empty() { Claimed::Released } else { Claimed::Held })
    }

    /// Takes the first picture of a reserved entrance after the flight started moving. Returns the
    /// banded tile and what is left of the flight for it to travel.
    fn admit(&mut self, window: WindowId, snapshot: &WindowSnapshot) -> Option<(OverlayTile, Duration)> {
        if self.started.is_none() || !snapshot.is_usable() {
            return None;
        }
        let position = self.entrances.iter().position(|e| e.window == window)?;
        let entrance = self.entrances.remove(position);
        self.merge(entrance_tile(&entrance, snapshot));
        restack(&mut self.tiles, self.focus);
        let tile = self
            .tiles
            .iter()
            .find(|t| t.window == window)
            .expect("merge just admitted it")
            .clone();
        Some((tile, late_join_duration(self.duration, self.progress())))
    }

    /// After an in-flight merge: stale frames are re-sent at the apply point.
    fn absorb_in_flight_change(&mut self, changed: bool, frames_changed: bool) {
        if mark_stale_on_untiled_change(changed, frames_changed) {
            self.frames_applied = false;
        }
    }

    /// Folds one later pass into the flight's tiles and focus; the overlay follows `merge_plans`.
    fn merge_pass(
        &mut self,
        tiles: Vec<OverlayTile>,
        focus: Option<WindowId>,
    ) -> Vec<(WindowId, Admitted)> {
        if focus.is_some() {
            self.focus = focus;
        }
        let outcomes = tiles
            .into_iter()
            .map(|tile| {
                let window = tile.window;
                (window, self.merge(tile))
            })
            .collect();
        restack(&mut self.tiles, self.focus);
        outcomes
    }

    /// Adds or retargets one window without disturbing anything already moving.
    fn merge(&mut self, tile: OverlayTile) -> Admitted {
        let action = merge_action(
            self.tiles.iter().find(|t| t.window == tile.window).map(|t| t.to),
            tile.to,
        );
        match action {
            Admitted::Redundant => {}
            Admitted::Retargeted => {
                let existing = self
                    .tiles
                    .iter_mut()
                    .find(|t| t.window == tile.window)
                    .expect("retarget implies the tile exists");
                // The original start is kept so a moving window is not yanked backwards.
                existing.to = tile.to;
                existing.snapshot = tile.snapshot;
                existing.floating = tile.floating;
                existing.server_order = tile.server_order;
                existing.depth = tile.depth;
                existing.companion = tile.companion;
                existing.focused = tile.focused;
            }
            Admitted::Joined => self.tiles.push(tile),
        }
        action
    }
}

/// The pictures that only make sense for the display the overlay is on; forgotten as a unit.
/// See "A render of the wrong display" in `docs/capture-overlay-research.md`.
#[derive(Default)]
struct DisplayPictures {
    /// Whatever the backdrop is currently showing; reused rather than recaptured.
    shown: Option<WindowSnapshot>,
    /// The desktop as ScreenCaptureKit rendered it: the only source that reliably has the wallpaper.
    desktop: Option<WindowSnapshot>,
    /// The last usable picture of the bar; capturable only while the overlay is not covering it.
    bar: Option<WindowSnapshot>,
    /// Whether a usable desktop has ever been drawn; until then a wallpaper-less capture beats black.
    drawn_once: bool,
}

impl DisplayPictures {
    /// Assigns the whole struct so a new field cannot be left behind.
    fn forget(&mut self) {
        *self = Self::default();
    }
}

pub struct FlightEngine {
    rx: Receiver,
    /// Timers post back into this actor's own queue through it.
    tx: Sender,
    mtm: MainThreadMarker,
    overlay: Option<TileOverlay>,
    cache: SnapshotCache,
    /// Full-size captures for windows SkyLight cannot serve. Results go through `cache`, never
    /// straight to a tile.
    service: SnapshotService,
    display: Option<(CGRect, f64)>,
    /// Which display the overlay is on, for the desktop capture.
    display_id: Option<u32>,
    running: Option<RunningAnimation>,
    /// Fires once after the layout passes settle, to start the animation moving.
    coalesce: Option<RepeatingTimer>,
    /// Fires once, `SETTLE_BEFORE_CAPTURES` after a lift, unless a flight begins first.
    quiet: Option<RepeatingTimer>,
    /// The capture work the last flights owe, run at `Quiet`.
    after_flight: Option<AfterFlight>,
    /// Windows from the most recent animation, so the post-animation refresh uses real ids.
    last_animated: Vec<SnapshotTarget>,
    /// Warms asked for during a flight, one per window, requested at `finish`.
    deferred_warm: Vec<SnapshotTarget>,
    /// Whether the desktop render was missing or stale at composition; re-rendered at `finish`.
    deferred_desktop: bool,
    /// The focus the previous flight landed on, for `refresh_targets`. Kept across a flight that
    /// names no focus.
    last_focus: Option<WindowId>,
    /// Everything held that is a picture of one particular display.
    pictures: DisplayPictures,
    /// Fires once after an animation, to recapture the bar away from the critical path.
    bar_refresh: Option<RepeatingTimer>,
    /// Places the real windows once the overlay covers them. Supplied by the owner.
    place_frames: Option<PlaceFrames>,
}

impl FlightEngine {
    pub fn new(rx: Receiver, tx: Sender, mtm: MainThreadMarker) -> Self {
        // The service completes on a background queue and wakes this actor through its channel.
        let notify_tx = tx.clone();
        let service = SnapshotService::new(
            2.0,
            std::sync::Arc::new(move || {
                _ = notify_tx.send(Event::SnapshotsReady);
            }),
        );
        Self {
            rx,
            tx,
            mtm,
            overlay: None,
            cache: SnapshotCache::new(),
            service,
            display: None,
            display_id: None,
            running: None,
            coalesce: None,
            quiet: None,
            after_flight: None,
            last_animated: Vec::new(),
            deferred_warm: Vec::new(),
            deferred_desktop: false,
            last_focus: None,
            pictures: DisplayPictures::default(),
            bar_refresh: None,
            place_frames: None,
        }
    }

    pub fn set_place_frames(&mut self, place: PlaceFrames) {
        self.place_frames = Some(place);
    }

    pub async fn run(mut self) {
        while let Some((span, event)) = self.rx.recv().await {
            let _guard = span.enter();
            self.handle(event);
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::SetDisplay { id, frame, scale } => self.set_display(id, frame, scale),
            Event::Animate { windows, focus, duration } => self.start(windows, focus, duration),
            Event::AnimateSurface {
                windows,
                from_offset,
                to_offset,
                final_frames,
                focus,
                duration,
            } => {
                self.start_surface(windows, from_offset, to_offset, final_frames, focus, duration)
            }
            Event::Bounce { windows, overshoot, final_frames, focus, duration } => {
                self.start_bounce(windows, overshoot, final_frames, focus, duration)
            }
            Event::ForgetWindow(window) => self.cache.forget(window),
            Event::DebugSlide { dx, dy, duration } => self.debug_slide(dx, dy, duration),
            Event::Tick => self.step(),
            Event::StartMoving => self.start_moving(),
            Event::Quiet => self.after_flight_captures(),
            Event::RefreshBar => {
                // One shot: dropping the timer stops it repeating.
                self.bar_refresh = None;
                self.refresh_bar();
            }
            Event::SnapshotsReady => self.collect_snapshots(),
            Event::PictureReady { window, snapshot, settled } => {
                self.picture_ready(window, snapshot, settled)
            }
            Event::DressingReady { window, dressing } => self.dressing_ready(window, dressing),
            Event::WarmCache => self.warm_cache(),
            Event::WarmWindows(targets) => {
                self.warm_windows(targets);
            }
            // Straight to the service, with no size test in the way.
            Event::RefreshFocus(target) => self.service.request(vec![target]),
        }
    }

    /// Moves completed background captures into the cache.
    fn collect_snapshots(&mut self) {
        if let Some(desktop) = self.service.take_desktop() {
            debug!(
                covered = format!(
                    "{:.0}x{:.0}",
                    desktop.coverage.covered.0, desktop.coverage.covered.1
                ),
                "desktop capture landed"
            );
            self.pictures.desktop = Some(desktop);
        }
        let landed = self.service.collect();
        if landed.is_empty() {
            return;
        }
        // Mid-flight the batch is cached without a hairline; `finish` harvests instead.
        if capture_work_allowed(self.phase(), CaptureKind::Harvest) {
            let harvested = self.running.as_ref().map(|running| &running.harvested);
            let to_dress: Vec<WindowId> = landed
                .iter()
                .filter(|(window, snapshot)| {
                    snapshot.is_usable() && !harvested.is_some_and(|done| done.contains(window))
                })
                .map(|(window, _)| *window)
                .collect();
            self.harvest_dressings(to_dress);
        }
        for (window, snapshot) in landed {
            debug!(
                pid = window.pid,
                idx = window.idx.get(),
                covered = format!(
                    "{:.0}x{:.0}",
                    snapshot.coverage.covered.0, snapshot.coverage.covered.1
                ),
                window_size = format!(
                    "{:.0}x{:.0}",
                    snapshot.coverage.window.0, snapshot.coverage.window.1
                ),
                usable = snapshot.is_usable(),
                "background snapshot landed"
            );
            // Background captures are never settled: the service knows sizes, not paint states.
            let comparison = if self.running.is_some() {
                self.compare_with_cached(window, &snapshot)
            } else {
                CacheComparison::default()
            };
            self.cache.insert(window, snapshot.clone());
            if snapshot.is_usable() {
                self.offer_mid_flight(window, &snapshot, false, comparison);
            }
        }
    }

    /// Offers an already-cached picture to the running flight per `should_swap_mid_flight`.
    fn offer_mid_flight(
        &mut self,
        window: WindowId,
        snapshot: &WindowSnapshot,
        settled: bool,
        comparison: CacheComparison,
    ) {
        let Some(running) = self.running.as_ref() else { return };
        let progress = running.progress_if_started();
        let state = running.tile_state(window, snapshot);
        let CacheComparison { renders_like_cached, same_source } = comparison;
        match should_swap_mid_flight(state, settled, renders_like_cached, same_source, progress) {
            // An unsettled capture of a held window can be its unpainted surface.
            SwapDecision::Claim => {
                if settled {
                    self.claim_reveal(window, snapshot);
                }
            }
            SwapDecision::Admit => {
                self.admit_entrance(window, snapshot);
            }
            SwapDecision::Swap(reason) => {
                debug!(
                    pid = window.pid,
                    idx = window.idx.get(),
                    reason,
                    progress = progress.unwrap_or(0.0),
                    "picture swapped mid-flight"
                );
                let remaining = self.remaining_flight();
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_tile_picture(window, snapshot, remaining);
                }
            }
            SwapDecision::CacheOnly => {}
        }
    }

    /// Queues background captures for windows the reactor identified; during a flight they wait
    /// in `deferred_warm` for `finish`. Returns the windows requested now.
    fn warm_windows(&mut self, targets: Vec<SnapshotTarget>) -> Vec<WindowId> {
        if !capture_work_allowed(self.phase(), CaptureKind::Warm) {
            defer_warm(&mut self.deferred_warm, targets);
            return Vec::new();
        }
        let wanted: Vec<SnapshotTarget> = targets
            .into_iter()
            // A usable picture of the wrong size, or a stale one, is recaptured. See "A window
            // that was resized keeps a usable picture" in `docs/capture-overlay-research.md`.
            .filter(|target| {
                let cached = self.cache.usable(target.window);
                crate::window_snapshot::needs_capture(
                    cached.map(|snapshot| snapshot.coverage),
                    (target.size.width, target.size.height),
                ) || cached.is_some_and(|snapshot| {
                    crate::window_snapshot::picture_is_stale(snapshot.taken.elapsed())
                })
            })
            .collect();
        if wanted.is_empty() {
            return Vec::new();
        }
        debug!(count = wanted.len(), "warming snapshots for reactor-supplied windows");
        let requested = wanted.iter().map(|target| target.window).collect();
        self.service.request(wanted);
        requested
    }

    /// Queues background captures for every visible window on the display. Cheap to repeat.
    fn warm_cache(&mut self) {
        let Some((display_frame, _)) = self.display else {
            warn!("no display geometry yet; cannot warm the snapshot cache");
            return;
        };
        let windows = rini_windows::window_server::visible_windows_on_display(display_frame);
        let targets: Vec<SnapshotTarget> = windows
            .into_iter()
            .map(|(server_id, frame)| SnapshotTarget {
                window: synthetic_window_id(server_id),
                server_id,
                size: frame.size,
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        debug!(count = targets.len(), "warming the snapshot cache");
        self.service.request(targets);
    }

    fn set_display(&mut self, id: u32, frame: CGRect, scale: f64) {
        let first = self.display.is_none();
        let changed = self.display != Some((frame, scale)) || self.display_id != Some(id);
        self.display = Some((frame, scale));
        self.display_id = Some(id);
        self.service.set_scale(scale);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_frame(frame, scale);
        }
        if first || changed {
            self.warm_cache();
            self.warm_desktop();
            // Anything in flight, and the desktop render, belong to the display just left.
            self.service.invalidate();
            self.pictures.forget();
            self.arm_bar_refresh();
        }
    }

    /// Creates the overlay on first use and keeps it forever: creation is too slow to pay per
    /// animation. See "Toggle alpha, do not order the window in and out" in the research doc.
    fn ensure_overlay(&mut self) -> Option<&mut TileOverlay> {
        if self.overlay.is_none() {
            let (frame, scale) = self.display?;
            match TileOverlay::new(frame, scale, self.mtm) {
                Some(overlay) => self.overlay = Some(overlay),
                None => {
                    warn!("could not create the animation overlay; animations will be skipped");
                    return None;
                }
            }
        }
        self.overlay.as_mut()
    }

    /// Recaptures both ends of a focus change once per flight, by the service route only.
    /// See "Mid-flight passes" in `docs/animation-smoothness.md`.
    fn refresh_destination_among(&mut self, tiles: &[(WindowId, WindowServerId, CGSize)]) {
        let current = self.running.as_ref().and_then(|running| running.focus);
        let windows: Vec<WindowId> = tiles.iter().map(|(w, _, _)| *w).collect();
        let wanted = refresh_targets(self.last_focus, current, &windows);
        if wanted.is_empty() {
            return;
        }
        let (wanted, requests) = refresh_requests(tiles, &wanted);
        if let Some(running) = self.running.as_mut() {
            running.refresh_targets = wanted.clone();
        }
        debug!(windows = wanted.len(), "destination refresh requested");
        self.service.request(requests);
    }

    /// Harvests hairlines for `windows` on a plain thread: the service's completion queue must not
    /// make capture calls (see `snapshot_service`). Results come back as `DressingReady`.
    fn harvest_dressings(&self, windows: Vec<WindowId>) {
        if windows.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        std::thread::Builder::new()
            .name("dressing-harvest".to_string())
            .spawn(move || {
                for window in windows {
                    let server_id = WindowServerId::from(window);
                    let Some(dressing) =
                        crate::edge_dressing::harvest_edge_dressing(server_id, scale)
                    else {
                        continue;
                    };
                    _ = tx.send(Event::DressingReady { window, dressing });
                }
            })
            .ok();
    }

    /// Takes a finished hairline harvest: onto the cached snapshot, and onto a tile in flight.
    fn dressing_ready(&mut self, window: WindowId, dressing: crate::edge_dressing::EdgeDressing) {
        if let Some(snapshot) = self.cache.get_mut(window) {
            snapshot.dressing = Some(dressing.clone());
        }
        let Some(running) = self.running.as_mut() else { return };
        running.harvested.insert(window);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_tile_dressing(window, &dressing);
        }
    }

    /// Takes a framed recapture: a chase's reveal, or the destination refresh.
    fn picture_ready(&mut self, window: WindowId, snapshot: WindowSnapshot, settled: bool) {
        // Compared before the cache absorbs the newcomer; a same-looking swap is a cut for nothing.
        let comparison = self.compare_with_cached(window, &snapshot);
        if snapshot.dressing.is_some() {
            if let Some(running) = self.running.as_mut() {
                running.harvested.insert(window);
            }
        }
        self.cache.insert(window, snapshot.clone());
        self.offer_mid_flight(window, &snapshot, settled, comparison);
    }

    /// How an incoming picture compares with the cached one. With nothing cached the source counts
    /// as the same and the rendering as different.
    fn compare_with_cached(&self, window: WindowId, incoming: &WindowSnapshot) -> CacheComparison {
        use crate::window_snapshot::SnapshotImage;
        let Some(cached) = self.cache.get(window) else {
            return CacheComparison { renders_like_cached: false, same_source: true };
        };
        let same_source = cached.source == incoming.source;
        if !cached.fits(CGSize::new(incoming.coverage.covered.0, incoming.coverage.covered.1)) {
            return CacheComparison { renders_like_cached: false, same_source };
        }
        let (SnapshotImage::Bitmap(old), SnapshotImage::Bitmap(new)) =
            (&cached.image, &incoming.image)
        else {
            return CacheComparison { renders_like_cached: false, same_source };
        };
        let renders_like_cached = match (
            crate::edge_dressing::thumbprint(old),
            crate::edge_dressing::thumbprint(new),
        ) {
            (Some(a), Some(b)) => crate::edge_dressing::renderings_match(&a, &b),
            _ => false,
        };
        CacheComparison { renders_like_cached, same_source }
    }

    /// How much of the running flight is left, in wall-clock time.
    fn remaining_flight(&self) -> Option<Duration> {
        self.running.as_ref().map(RunningAnimation::remaining)
    }

    fn phase(&self) -> FlightPhase {
        self.running.as_ref().map_or(FlightPhase::Idle, RunningAnimation::phase)
    }

    /// Chases the first settled picture for a holding grow or entrance, one thread per window.
    /// See "A grow holds, then reveals" in `docs/animation-smoothness.md`.
    fn chase_reveal_pictures(&self, awaiting: &[(WindowId, CGSize)]) {
        if !capture_work_allowed(self.phase(), CaptureKind::Chase) {
            return;
        }
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        for (window, size) in awaiting.iter().copied() {
            let server_id = WindowServerId::from(window);
            let tx = self.tx.clone();
            // The picture the tile flies from; a capture that no longer renders like it is the
            // app's repaint. An entrance has none.
            let pre_resize = self.cache.get(window).and_then(bitmap_thumbprint);
            std::thread::Builder::new()
                .name("reveal-chase".to_string())
                .spawn(move || {
                    // The frame resizes instantly; the pixels lag. Only a settled capture counts.
                    let mut last_print: Option<Vec<u8>> = None;
                    for _ in 0..REVEAL_CHASE_ATTEMPTS {
                        std::thread::sleep(REVEAL_CHASE_INTERVAL);
                        let Some(info) = rini_windows::window_server::get_window(server_id) else {
                            continue;
                        };
                        let frame_fits = crate::window_snapshot::fits_frame(
                            (info.frame.size.width, info.frame.size.height),
                            (size.width, size.height),
                        );
                        if !frame_fits {
                            last_print = None;
                            continue;
                        }
                        let Some(snapshot) =
                            crate::window_snapshot::capture_via_framed_with_dressing(
                                server_id, scale,
                            )
                        else {
                            continue;
                        };
                        if !snapshot.is_usable() || !snapshot.fits(size) {
                            last_print = None;
                            continue;
                        }
                        let Some(print) = bitmap_thumbprint(&snapshot) else { continue };
                        let settled =
                            chase_settled(last_print.as_deref(), &print, pre_resize.as_deref());
                        last_print = Some(print);
                        if !settled {
                            continue;
                        }
                        _ = tx.send(Event::PictureReady { window, snapshot, settled: true });
                        return;
                    }
                    debug!(
                        pid = window.pid,
                        idx = window.idx.get(),
                        "reveal chase gave up; the hold deadline will fly the placeholder"
                    );
                })
                .ok();
        }
    }

    /// Takes a landed picture for a window a holding flight is waiting on. Returns whether the
    /// hold claimed it.
    fn claim_reveal(&mut self, window: WindowId, snapshot: &WindowSnapshot) -> bool {
        let Some(running) = self.running.as_mut() else { return false };
        let Some(claimed) = running.claim(window, snapshot) else { return false };
        self.recompose();
        if claimed == Claimed::Released {
            debug!(
                pid = window.pid,
                idx = window.idx.get(),
                "reveal pixels landed; starting the flight"
            );
            self.start_moving();
        }
        true
    }

    /// Frame zero again, for a flight still collecting passes: the plan is rebuilt and installed.
    fn recompose(&mut self) {
        let Self { overlay, running, .. } = self;
        let Some(running) = running.as_mut() else { return };
        let mut rebuilt = plan::plan_from_tiles(&running.tiles);
        // A tile only knows it resizes; the plan remembers which of those are entrances.
        let entrances: Vec<WindowId> = running.plan.entrances.iter().map(|(w, _, _)| *w).collect();
        rebuilt.mark_entrances(&entrances);
        running.plan = plan::FlightPlan::from(rebuilt);
        if let Some(overlay) = overlay.as_mut() {
            let banding = band_plan(&running.plan, &running.tiles, running.focus);
            overlay.install(&running.plan, &running.tiles, &banding);
        }
    }

    /// Adds the tile for a reserved entrance whose first picture landed after the flight started
    /// moving. Returns whether the picture was taken.
    fn admit_entrance(&mut self, window: WindowId, snapshot: &WindowSnapshot) -> bool {
        let Some(running) = self.running.as_mut() else { return false };
        let Some((tile, duration)) = running.admit(window, snapshot) else { return false };
        running.plan.entrances.push((tile.window, tile.from, tile.to));
        let banding = band_plan(&running.plan, &running.tiles, running.focus);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.add_tile(&tile, plan::GroupKey::Loose, &banding, duration);
        }
        debug!(pid = window.pid, idx = window.idx.get(), "window entered mid-flight");
        true
    }


    /// The snapshot to draw for one window: any usable cached picture, whatever its shape; a
    /// wrong-shaped one is drawn cropped. See "Resizes through the overlay" in the doc.
    fn snapshot_for(&mut self, request: &AnimationRequest) -> Option<WindowSnapshot> {
        self.cache.usable(request.window).cloned()
    }


    /// Tiles for the border windows tracing the animated windows; each anchor is the window's real
    /// frame plus its tile's from/to/depth. See "Window borders during animations" in the doc.
    fn companion_tiles(
        &mut self,
        display: CGRect,
        anchors: &[(CGRect, CGRect, CGRect, usize)],
        exclude: &std::collections::HashSet<u32>,
        needs_capture: &mut Vec<SnapshotTarget>,
    ) -> (Vec<OverlayTile>, Vec<SnapshotTarget>) {
        if anchors.is_empty() {
            return (Vec::new(), Vec::new());
        }
        // Every window rini manages is excluded, not only the pass's: parked windows share a frame.
        let managed = managed_server_ids(
            exclude,
            self.cache.iter().map(|(window, _)| *window),
            self.deferred_warm.iter().chain(self.after_flight.iter().flat_map(|a| a.targets.iter())).map(|t| t.window),
        );
        let candidates: Vec<(WindowServerId, CGRect)> =
            rini_windows::window_server::visible_windows_on_display(display)
                .into_iter()
                .filter(|(id, _)| !managed.contains(&id.as_u32()))
                .collect();
        let mut claimed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut tiles = Vec::new();
        let mut targets = Vec::new();
        for &(real, from, to, depth) in anchors {
            let Some((server_id, frame)) = companion_of(real, &candidates, display) else { continue };
            // One border traces one window; stacked twins share a frame.
            if !claimed.insert(server_id.as_u32()) {
                continue;
            }
            let window = synthetic_window_id(server_id);
            debug!(
                wsid = server_id.as_u32(),
                anchor = format!("{:.0},{:.0} {:.0}x{:.0}", real.origin.x, real.origin.y, real.size.width, real.size.height),
                "border companion matched"
            );
            targets.push(SnapshotTarget { window, server_id, size: frame.size });
            let offset = (frame.origin.x - real.origin.x, frame.origin.y - real.origin.y);
            let follow = |rect: CGRect| {
                CGRect::new(
                    CGPoint::new(rect.origin.x + offset.0, rect.origin.y + offset.1),
                    frame.size,
                )
            };
            match self.cache.usable(window).cloned() {
                Some(snapshot) => tiles.push(OverlayTile {
                    window,
                    from: follow(from),
                    to: follow(to),
                    snapshot,
                    floating: false,
                    server_order: None,
                    // Its window's banded depth; `restack` leaves companions alone.
                    depth,
                    companion: true,
                    focused: false,
                    }),
                None => needs_capture.push(SnapshotTarget { window, server_id, size: frame.size }),
            }
        }
        (tiles, targets)
    }

    fn start(
        &mut self,
        windows: Vec<AnimationRequest>,
        focus: Option<WindowId>,
        duration: Duration,
    ) {
        if windows.is_empty() {
            return;
        }
        let Some((display_frame, _)) = self.display else {
            debug!("no display geometry yet; skipping animation");
            return;
        };

        // Every moving window's destination, picture or not. A still window is not re-placed: the
        // round trip invites another layout pass.
        let final_frames: Vec<(WindowId, CGRect)> = windows
            .iter()
            .filter(|request| is_moving(request.from, request.to))
            .map(|request| (request.window, request.to))
            .collect();

        let depths = rini_windows::window_server::front_to_back_depths();

        let any_resize = windows.iter().any(|request| {
            crate::window_snapshot::is_a_resize(request.from.size, request.to.size)
        });
        let apply_at = apply_frames_at(FlightKind::Layout, any_resize);

        let mut tiles = Vec::with_capacity(windows.len());
        let mut skipped = 0usize;
        let mut offscreen = 0usize;
        let mut needs_capture: Vec<SnapshotTarget> = Vec::new();
        let mut entrances: Vec<PendingEntrance> = Vec::new();
        let mut awaiting: Vec<(WindowId, CGSize)> = Vec::new();
        // Newly opened windows travelling from their spawn frame.
        let mut spawn_entrances: Vec<(WindowId, CGRect, CGRect)> = Vec::new();
        let mut chase: Vec<(WindowId, CGSize)> = Vec::new();
        let mut entrance_frames: Vec<(WindowId, CGRect)> = Vec::new();
        let mut sync_captures = 0usize;
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        let mut starts: Vec<(WindowId, CGRect)> = Vec::new();
        let mut resolved: Vec<(WindowId, CGRect, CGRect, bool)> = Vec::new();
        // For `neighbour_travel`: a window leaving for or returning from a park rides its
        // nearest strip neighbour's vector.
        let others: Vec<(CGRect, CGRect, bool)> =
            windows.iter().map(|r| (r.from, r.to, r.floating)).collect();
        for (index, request) in windows.iter().enumerate() {
            let travel = (!request.floating)
                .then(|| {
                    let excluding_self: Vec<(CGRect, CGRect, bool)> = others
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| *j != index)
                        .map(|(_, o)| *o)
                        .collect();
                    let subject = travel_subject(request.from, request.to, display_frame);
                    neighbour_travel(subject, &excluding_self, display_frame)
                })
                .flatten();
            let start = actual_start(request, display_frame, travel);
            // The tile's visual destination; `final_frames` keeps the real park in `request.to`.
            let end = resolve_end(start, request.to, display_frame, travel);
            if !worth_animating(start, end, display_frame) {
                offscreen += 1;
                debug!(
                    wsid = request.server_id.as_u32(),
                    start = format!(
                        "{:.0},{:.0} {:.0}x{:.0}",
                        start.origin.x, start.origin.y, start.size.width, start.size.height
                    ),
                    to = format!(
                        "{:.0},{:.0} {:.0}x{:.0}",
                        request.to.origin.x,
                        request.to.origin.y,
                        request.to.size.width,
                        request.to.size.height
                    ),
                    "skipped as off screen"
                );
                continue;
            }
            let snapshot = self.snapshot_for(request);
            // Queued now so the next switch has pixels, even if this one does not.
            if snapshot
                .as_ref()
                .is_none_or(|s| s.source == crate::window_snapshot::SnapshotSource::SkyLight
                    && !s.is_usable())
            {
                needs_capture.push(SnapshotTarget {
                    window: request.window,
                    server_id: request.server_id,
                    size: request.from.size,
                });
            }
            if snapshot.is_none() {
                debug!(
                    pid = request.window.pid,
                    idx = request.window.idx.get(),
                    "animation wanted a snapshot for this window and had none"
                );
            }
            match snapshot {
                Some(snapshot) => {
                    // A grow whose picture cannot cover the destination holds for the reveal.
                    if crate::window_snapshot::outgrows(
                        snapshot.coverage.covered,
                        request.to.size,
                    ) {
                        awaiting.push((request.window, request.to.size));
                    }
                    tiles.push(OverlayTile {
                        window: request.window,
                        from: to_overlay_space(start, display_frame),
                        to: to_overlay_space(end, display_frame),
                        snapshot,
                        floating: request.floating,
                        server_order: depths.get(&request.server_id.as_u32()).copied(),
                        depth: 0,
                        companion: false,
                        focused: focus == Some(request.window),
                            });
                    starts.push((request.window, start));
                    resolved.push((request.window, start, end, request.floating));
                }
                // No picture: almost always a window that just opened. See "A window that opens
                // travels from its spawn frame" in `docs/animation-smoothness.md`.
                None => {
                    // Only a frame on this display counts as a spawn; capturing off screen is slow.
                    let spawn = rini_windows::window_server::get_window(request.server_id)
                        .map(|info| info.frame)
                        .filter(|f| !rini_geometry::is_off_screen(display_frame, *f));
                    let budget_left = sync_captures < MAX_SYNC_ENTRANCE_CAPTURES;
                    let captured_at = Instant::now();
                    let picture = spawn
                        .filter(|f| f.size.width > 0.0 && f.size.height > 0.0 && budget_left)
                        .and_then(|_| {
                            sync_captures += 1;
                            capture_via_framed_with_dressing(request.server_id, scale)
                        });
                    let usable = picture.as_ref().is_some_and(|p| p.is_usable());
                    match entrance_plan(spawn, request.to, usable, budget_left) {
                        EntranceDecision::Travel { from, to } => {
                            let snapshot = picture.expect("usable implies a picture");
                            debug!(
                                pid = request.window.pid,
                                idx = request.window.idx.get(),
                                spawn = format!(
                                    "{:.0},{:.0} {:.0}x{:.0}",
                                    from.origin.x, from.origin.y, from.size.width, from.size.height
                                ),
                                slot = format!(
                                    "{:.0},{:.0} {:.0}x{:.0}",
                                    to.origin.x, to.origin.y, to.size.width, to.size.height
                                ),
                                capture_ms = captured_at.elapsed().as_millis() as u64,
                                "entrance from spawn"
                            );
                            self.cache.insert(request.window, snapshot.clone());
                            let from_o = to_overlay_space(from, display_frame);
                            let to_o = to_overlay_space(to, display_frame);
                            // Already at its slot (a cold cache, not an open): a still tile, no chase.
                            let standing = from.same_as(to);
                            tiles.push(OverlayTile {
                                window: request.window,
                                from: from_o,
                                to: to_o,
                                snapshot,
                                floating: request.floating,
                                server_order: if standing {
                                    depths.get(&request.server_id.as_u32()).copied()
                                } else {
                                    Some(0)
                                },
                                depth: 0,
                                companion: false,
                                focused: standing && focus == Some(request.window) || !standing,
                                            });
                            if standing {
                                starts.push((request.window, from));
                                resolved.push((request.window, from, to, request.floating));
                            } else {
                                spawn_entrances.push((request.window, from_o, to_o));
                                chase.push((request.window, to.size));
                                entrance_frames.push((request.window, to));
                            }
                        }
                        EntranceDecision::Reserve(reason) => {
                            skipped += 1;
                            debug!(
                                pid = request.window.pid,
                                idx = request.window.idx.get(),
                                reason,
                                "entrance reserved"
                            );
                            let (entrance, waiting) = entrance_reservation(
                                request.window,
                                to_overlay_space(request.to, display_frame),
                                request.floating,
                            );
                            entrances.push(entrance);
                            awaiting.extend(waiting);
                        }
                    }
                }
            }
        }
        // Stacked here so the companions can anchor to their windows' depths.
        restack(&mut tiles, focus);
        let anchors: Vec<(CGRect, CGRect, CGRect, usize)> = starts
            .iter()
            .filter_map(|(window, start)| {
                let tile = tiles.iter().find(|tile| tile.window == *window)?;
                Some((*start, tile.from, tile.to, tile.depth))
            })
            .collect();
        let exclude: std::collections::HashSet<u32> =
            windows.iter().map(|request| request.server_id.as_u32()).collect();
        let (companions, companion_targets) =
            self.companion_tiles(display_frame, &anchors, &exclude, &mut needs_capture);
        tiles.extend(companions);
        if !needs_capture.is_empty() {
            // Frame zero: the overlay is not up yet. A pass merging into a flight defers instead.
            if capture_work_allowed(self.phase(), CaptureKind::NeedsCapture) {
                debug!(
                    count = needs_capture.len(),
                    "queueing background captures for windows SkyLight could not serve"
                );
                self.service.request(needs_capture);
            } else {
                defer_warm(&mut self.deferred_warm, needs_capture);
            }
        }
        debug!(
            requested = windows.len(),
            tiles = tiles.len(),
            offscreen,
            no_snapshot = skipped,
            display = format!(
                "{:.0},{:.0} {:.0}x{:.0}",
                display_frame.origin.x,
                display_frame.origin.y,
                display_frame.size.width,
                display_frame.size.height
            ),
            "overlay animation composition"
        );
        // Companions included: a border recolors when focus moves.
        self.last_animated = windows
            .iter()
            .map(|request| SnapshotTarget {
                window: request.window,
                server_id: request.server_id,
                size: request.to.size,
            })
            .chain(companion_targets)
            .collect();

        let moving_drawable = tiles.iter().any(|tile| is_moving(tile.from, tile.to));
        if !worth_flying(moving_drawable, self.running.is_some()) {
            self.request_frames(final_frames);
            // Warm anyway, or the cache never fills: it only filled when an animation completed.
            let targets = std::mem::take(&mut self.last_animated);
            self.warm_windows(targets);
            return;
        }

        let mut plan = plan::reflow_plan(&resolved, display_frame);
        plan.entrances.extend(spawn_entrances);
        for tile in &tiles {
            if plan.member(tile.window).is_none() {
                plan.adopt(tile);
            }
        }

        self.begin_group(
            tiles,
            final_frames,
            duration,
            "per-window",
            GroupStart::Coalesced,
            apply_at,
            entrances,
            awaiting,
            chase,
            entrance_frames,
            focus,
            None,
            plan,
        );
    }

    /// Runs one plan through the shared machinery: merge into a running flight (`merge_plans`),
    /// or install a fresh one. See "The overlay engine" in `docs/animation-smoothness.md`.
    fn begin_group(
        &mut self,
        mut tiles: Vec<OverlayTile>,
        final_frames: Vec<(WindowId, CGRect)>,
        duration: Duration,
        label: &'static str,
        start: GroupStart,
        apply_at: f64,
        entrances: Vec<PendingEntrance>,
        awaiting: Vec<(WindowId, CGSize)>,
        chase: Vec<(WindowId, CGSize)>,
        entrance_frames: Vec<(WindowId, CGRect)>,
        focus: Option<WindowId>,
        pan: Option<CGPoint>,
        plan: plan::ReflowPlan,
    ) {
        // Merge before the empty check: a pass with nothing drawable can still carry fresh
        // destinations for a flight in progress.
        if self.running.is_some() {
            let in_flight;
            let hold_frames: Option<Vec<(WindowId, CGRect)>>;
            let frames_changed;
            let focus_before: Option<WindowId>;
            let display = self.display.map(|(frame, _)| frame);
            {
                let running = self.running.as_mut().expect("checked above");
                in_flight = running.started.is_some();
                // A resize joining mid-flight needs the earlier apply point just as much.
                running.apply_at = running.apply_at.min(apply_at);
                for entrance in entrances {
                    if !running.entrances.iter().any(|e| e.window == entrance.window) {
                        running.entrances.push(entrance);
                    }
                }
                if let Some(display) = display {
                    retarget_entrances(&mut running.entrances, &final_frames, display);
                }
                // Merged before the hold reads them, so a held merge re-requests this pass's frames.
                frames_changed = merge_final_frames(&mut running.final_frames, final_frames);
                hold_frames = running.extend_hold(&awaiting, in_flight, duration, Instant::now());
                focus_before = running.focus;
                running.merge_pass(tiles, focus);
            }
            if in_flight {
                // See "Mid-flight passes" in `docs/animation-smoothness.md`.
                let Self { overlay, running, .. } = self;
                let running = running.as_mut().expect("checked above");
                let presented = overlay.as_ref().map(|o| o.presented_positions()).unwrap_or_default();
                let focus_changed = focus.is_some() && focus != focus_before;
                // The viewport in overlay space: what a member's destination is judged against.
                let viewport = display
                    .map(|d| to_overlay_space(d, d))
                    .unwrap_or(CGRect::new(CGPoint::new(-1e9, -1e9), CGSize::new(2e9, 2e9)));
                let (merged, mut delta) = plan::merge_plans(
                    &running.plan,
                    &plan,
                    pan,
                    &presented,
                    running.focus,
                    viewport,
                );
                delta.focus_changed = focus_changed;
                running.plan = merged;
                sync_tiles_to_plan(&mut running.tiles, &running.plan);
                if !delta.is_empty() {
                    debug!(
                        groups = running.plan.groups.iter().filter(|g| !g.members.is_empty()).count(),
                        changing = running.plan.changing.len(),
                        entrances = running.plan.entrances.len(),
                        reparented = delta.reparented.len(),
                        retargeted_groups = delta.retargeted_groups.len(),
                        joined = delta.joined_tiles.len(),
                        "flight merged"
                    );
                    for (key, to) in &delta.retargeted_groups {
                        let presented = presented.get(key).copied().unwrap_or_default();
                        debug!(
                            key = format!("{key:?}"),
                            presented = format!("{:.0},{:.0}", presented.x, presented.y),
                            to = format!("{:.0},{:.0}", to.x, to.y),
                            "group retargeted"
                        );
                    }
                    for (window, from_key, to_key) in &delta.reparented {
                        let dest = running.plan.member(*window).map(|m| match m {
                            plan::Member::Rigid { key, rel } => plan::overlay_of(rel, running.plan.position_of(key)),
                            plan::Member::Changing { to, .. } | plan::Member::Entrance { to, .. } => to,
                            plan::Member::Floating { to, .. } => plan::overlay_of(to, running.plan.position_of(plan::GroupKey::Floating)),
                        });
                        let from_to = running.plan.position_of(*from_key);
                        debug!(
                            pid = window.pid,
                            idx = window.idx.get(),
                            from = format!("{from_key:?}"),
                            to = format!("{to_key:?}"),
                            group_destination = format!("{:.0},{:.0}", from_to.x, from_to.y),
                            member_destination = dest.map(|d| format!("{:.0},{:.0} {:.0}x{:.0}", d.origin.x, d.origin.y, d.size.width, d.size.height)).unwrap_or_default(),
                            "member reparented"
                        );
                    }
                }
                if let Some(overlay) = overlay.as_mut() {
                    let banding = band_plan(&running.plan, &running.tiles, running.focus);
                    overlay.retarget(&delta, &running.plan, &running.tiles, &banding, duration);
                }
                let changed = delta.moves_anything();
                if changed {
                    // The orchestration clock restarts so placement and teardown cover the new legs.
                    running.started = Some(Instant::now());
                    running.duration = duration;
                }
                running.absorb_in_flight_change(changed, frames_changed);
                // A grow joining mid-flight cannot hold; its chase lands as `Swap("reveal")`.
                let (_, chase_set, _) = frame_zero_work(&awaiting, &chase, &[], &[]);
                if !entrance_frames.is_empty() {
                    self.request_frames(entrance_frames);
                }
                if !chase_set.is_empty() {
                    self.chase_reveal_pictures(&chase_set);
                }
            } else {
                // Still collecting behind the coalesce window: recompose frame zero statically.
                self.recompose();
                let reapply = self.running.as_mut().and_then(|running| {
                    // A held merge already carries the merged frames below.
                    if hold_frames.is_some() {
                        return None;
                    }
                    reapply_set(
                        running.frames_applied,
                        false,
                        frames_changed,
                        &running.final_frames,
                    )
                });
                if let Some(frames) = reapply {
                    self.request_frames(frames);
                }
                // A newcomer's slot goes out now, unless a hold below sends every frame anyway.
                if hold_frames.is_none() {
                    if !entrance_frames.is_empty() {
                        self.request_frames(entrance_frames);
                    }
                    if !chase.is_empty() {
                        self.chase_reveal_pictures(&chase);
                    }
                }
            }
            if let Some(frames) = hold_frames {
                self.request_frames(frames);
                let (_, chase_set, _) = frame_zero_work(&awaiting, &chase, &[], &[]);
                self.chase_reveal_pictures(&chase_set);
            }
            return;
        }

        // Guards the strip path: a pan with no usable picture leaves nothing to draw.
        if tiles.is_empty() {
            self.request_frames(final_frames);
            // Warm anyway, or the cache never fills.
            let targets = std::mem::take(&mut self.last_animated);
            self.warm_windows(targets);
            return;
        }

        // A flight beginning inside the quiet period carries the owed captures to its own lift.
        self.quiet = None;
        let held = self.pictures.shown.is_some();
        let backdrop = self.capture_backdrop().or_else(|| self.pictures.shown.clone());
        if backdrop.is_some() {
            self.pictures.shown = backdrop.clone();
        }
        let (bar, strip) = self.bar_picture();
        Self::log_dressing(label, backdrop.as_ref(), bar.as_ref(), strip, held);
        let Some(overlay) = self.ensure_overlay() else {
            self.request_frames(final_frames);
            return;
        };
        restack(&mut tiles, focus);
        overlay.set_backdrop(backdrop.as_ref());
        overlay.set_bar(bar.as_ref(), strip);
        let plan = plan::FlightPlan::from(plan);
        overlay.install(&plan, &tiles, &band_plan(&plan, &tiles, focus));
        // Shown at once, so the real windows can be placed underneath without a visible jump.
        overlay.show();

        let tx = self.tx.clone();
        let clock = RepeatingTimer::every(FRAME_INTERVAL, move || {
            _ = tx.send(Event::Tick);
        });
        if clock.is_none() {
            warn!("could not start the frame clock; drawing the final frame directly");
        }

        // A holding flight applies the real frames now, so the app rerenders under the overlay.
        let (holding, chase_set, now_frames) =
            frame_zero_work(&awaiting, &chase, &final_frames, &entrance_frames);
        if !now_frames.is_empty() {
            self.request_frames(now_frames);
        }
        if !chase_set.is_empty() {
            self.chase_reveal_pictures(&chase_set);
        }
        self.running = Some(RunningAnimation {
            tiles,
            final_frames,
            frames_applied: holding,
            started: None,
            duration,
            apply_at,
            entrances,
            hold_deadline: holding.then(|| Instant::now() + reveal_hold_limit(duration)),
            awaiting,
            destination_refreshed: false,
            refresh_targets: Vec::new(),
            harvested: HashSet::new(),
            focus,
            plan,
            _clock: clock,
        });

        // With no clock the animation would never advance.
        if self.running.as_ref().is_some_and(|running| running._clock.is_none()) {
            if let Some(running) = self.running.as_mut() {
                running.started = Some(Instant::now());
            }
            self.step_to_end();
            return;
        }

        match start {
            GroupStart::Immediate => self.start_moving(),
            GroupStart::Coalesced => {
                let tx = self.tx.clone();
                self.coalesce = RepeatingTimer::every(COALESCE_WINDOW, move || {
                    _ = tx.send(Event::StartMoving);
                });
            }
        }
    }

    /// Animates the whole strip surface as one rigid group: one container, one position
    /// animation. See "Strip movements" in `docs/animation-smoothness.md`.
    fn start_surface(
        &mut self,
        windows: Vec<SurfaceWindow>,
        from_offset: CGPoint,
        to_offset: CGPoint,
        final_frames: Vec<(WindowId, CGRect)>,
        focus: Option<WindowId>,
        duration: Duration,
    ) {
        // Remembered before anything can fail, so a window with no picture is still warmed.
        self.last_animated = windows
            .iter()
            .map(|w| SnapshotTarget {
                window: w.window,
                server_id: w.server_id,
                size: w.frame.size,
            })
            .collect();

        let depths = rini_windows::window_server::front_to_back_depths();
        let mut tiles = Vec::with_capacity(windows.len());
        let mut missing = 0usize;
        let mut misshapen = 0usize;
        let mut needs_capture: Vec<SnapshotTarget> = Vec::new();
        let mut starts: Vec<(WindowId, CGRect)> = Vec::new();
        for window in &windows {
            let (from, to) = surface_travel(window.frame, from_offset, to_offset, window.pinned);
            match self.cache.usable(window.window).cloned() {
                Some(snapshot) => {
                    // A wrong-shaped picture is stretched rather than dropped. See "A window that
                    // was resized keeps a usable picture" in `docs/capture-overlay-research.md`.
                    if !snapshot.fits(window.frame.size) {
                        misshapen += 1;
                    }
                    if let Some(info) = rini_windows::window_server::get_window(window.server_id) {
                        starts.push((window.window, info.frame));
                    }
                    tiles.push(OverlayTile {
                        window: window.window,
                        from,
                        to,
                        snapshot,
                        floating: window.floating,
                        server_order: depths.get(&window.server_id.as_u32()).copied(),
                        depth: 0,
                        companion: false,
                        focused: focus == Some(window.window),
                            });
                }
                // Still placed by `final_frames`, and warmed once the movement settles.
                None => missing += 1,
            }
        }
        restack(&mut tiles, focus);
        let anchors: Vec<(CGRect, CGRect, CGRect, usize)> = starts
            .iter()
            .filter_map(|(window, real)| {
                let tile = tiles.iter().find(|tile| tile.window == *window)?;
                Some((*real, tile.from, tile.to, tile.depth))
            })
            .collect();
        let exclude: std::collections::HashSet<u32> =
            windows.iter().map(|window| window.server_id.as_u32()).collect();
        let (companions, companion_targets) = match self.display {
            Some((display_frame, _)) => {
                self.companion_tiles(display_frame, &anchors, &exclude, &mut needs_capture)
            }
            None => (Vec::new(), Vec::new()),
        };
        tiles.extend(companions);
        self.last_animated.extend(companion_targets);
        if !needs_capture.is_empty() {
            // Frame zero unless this switch chains onto a flight; then it waits for `finish`.
            if capture_work_allowed(self.phase(), CaptureKind::NeedsCapture) {
                self.service.request(needs_capture);
            } else {
                defer_warm(&mut self.deferred_warm, needs_capture);
            }
        }
        debug!(
            requested = windows.len(),
            tiles = tiles.len(),
            missing,
            misshapen,
            travel = format!(
                "{:.0},{:.0} -> {:.0},{:.0}",
                from_offset.x, from_offset.y, to_offset.x, to_offset.y
            ),
            "surface group animation"
        );

        // One rigid piece for the strip; companions are adopted by their own vectors.
        let drawn: Vec<SurfaceWindow> = windows
            .iter()
            .filter(|w| tiles.iter().any(|t| t.window == w.window && !t.companion))
            .cloned()
            .collect();
        let mut plan = plan::surface_plan(&drawn, from_offset, to_offset);
        for tile in &tiles {
            if plan.member(tile.window).is_none() {
                plan.adopt(tile);
            }
        }

        // A strip movement never resizes and never carries a brand-new window.
        self.begin_group(
            tiles,
            final_frames,
            duration,
            "surface",
            GroupStart::Immediate,
            apply_frames_at(FlightKind::Pan, false),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            focus,
            Some(pan_travel(from_offset, to_offset)),
            plan,
        );
    }

    /// Nudges the strip surface by `overshoot` and back, additively on a running flight or as the
    /// only motion of a no-travel one. See "Edge bounce" in `docs/animation-smoothness.md`.
    fn start_bounce(
        &mut self,
        windows: Vec<SurfaceWindow>,
        overshoot: CGPoint,
        final_frames: Vec<(WindowId, CGRect)>,
        focus: Option<WindowId>,
        duration: Duration,
    ) {
        if self.running.is_none() {
            let at_rest = CGPoint::new(0.0, 0.0);
            self.start_surface(windows, at_rest, at_rest, final_frames, focus, duration);
        }
        let Self { overlay, running, .. } = self;
        let (Some(overlay), Some(running)) = (overlay.as_mut(), running.as_mut()) else {
            return;
        };
        running.duration = clock_for_bounce(running.started, running.duration, duration);
        overlay.bounce(overshoot, duration);
        debug!(
            overshoot = format!("{:.0},{:.0}", overshoot.x, overshoot.y),
            joined = running.started.is_some(),
            "edge bounce"
        );
    }

    /// Starts an animation that is on screen but not yet moving: hands the plan to Core Animation
    /// in one transaction and starts the clock.
    fn start_moving(&mut self) {
        // Dropping the timer stops it repeating.
        self.coalesce = None;
        // A hold waits for its reveal pixels or the deadline; the timer re-fires at the deadline.
        let now = Instant::now();
        if let Some(running) = self.running.as_ref()
            && running.started.is_none()
            && !running.awaiting.is_empty()
        {
            if let Some(wait) = hold_wait(running.hold_deadline, now) {
                let tx = self.tx.clone();
                self.coalesce = RepeatingTimer::every(wait, move || {
                    _ = tx.send(Event::StartMoving);
                });
                return;
            }
            let running = self.running.as_mut().expect("checked above");
            warn!(
                still_waiting = running.awaiting.len(),
                "reveal pixels did not arrive in time; flying with the placeholder"
            );
            running.awaiting.clear();
        }
        let Some(running) = self.running.as_mut() else { return };
        if running.started.is_some() {
            return;
        }
        debug!(windows = running.tiles.len(), "starting the animation after coalescing");
        running.started = Some(Instant::now());
        let tiles = std::mem::take(&mut running.tiles);
        let duration = running.duration;
        // The acceptance greps count this line against "overlay lifted".
        let flight = &running.plan;
        debug!(
            groups = flight.groups.iter().filter(|g| !g.members.is_empty()).count(),
            changing = flight.changing.len(),
            entrances = flight.entrances.len(),
            floating = flight.floating.len(),
            "flight composed"
        );
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.fly(flight, duration);
        }
        if let Some(running) = self.running.as_mut() {
            running.tiles = tiles;
        }
    }

    fn step(&mut self) {
        let (done, place_now, refresh_now) = {
            let Some(running) = self.running.as_mut() else { return };
            let progress = running.progress();
            // Nothing is drawn here; the tick only paces the mid-flight work.
            let place_now = running.frames_due(progress);
            let refresh_now = running.take_refresh(progress);
            (running.is_done(), place_now, refresh_now)
        };
        // The render server runs a frame or so behind the actor's clock; lifting on the clock
        // alone shows the real windows one frame ahead of their tiles.
        let clock_done = done;
        let mut done = false;
        if clock_done {
            let settled = self.overlay.as_ref().is_none_or(TileOverlay::settled);
            let handover = self.handover();
            let landed = handover.as_ref().is_none_or(|report| report.count_over == 0);
            let overdue = self.running.as_ref().is_some_and(RunningAnimation::overdue);
            done = lift_now(clock_done, settled, landed, overdue);
            if done && let Some(running) = self.running.as_ref() {
                // `landed=false` or `settled=false` means the grace ran out.
                let late_ms = running
                    .started
                    .map(|s| s.elapsed().saturating_sub(running.duration).as_millis() as u64)
                    .unwrap_or(0);
                let worst_pt = handover.as_ref().map_or(0.0, |r| r.worst_visible_pt);
                debug!(settled, landed, late_ms, worst_pt = format!("{worst_pt:.2}"), "flight landed");
            }
        }
        if refresh_now {
            let tiles: Vec<(WindowId, WindowServerId, CGSize)> = self
                .running
                .as_ref()
                .map(|running| {
                    running
                        .tiles
                        .iter()
                        // Companions never take the one mid-flight recapture.
                        .filter(|tile| !tile.companion)
                        .map(|tile| (tile.window, WindowServerId::from(tile.window), tile.to.size))
                        .collect()
                })
                .unwrap_or_default();
            self.refresh_destination_among(&tiles);
        }
        if let Some(frames) = place_now {
            self.request_frames(frames);
        }
        if done {
            self.finish();
        }
    }

    /// Logs how many real windows are not where their tiles finished, and the worst of them.
    /// See "Real windows land before lift" in `docs/animation-smoothness.md`.
    fn report_handover_error(&self) {
        let Some(report) = self.handover() else { return };
        if report.count_over > 0 {
            debug!(
                count_over = report.count_over,
                total = report.total,
                worst_visible_pt = format!("{:.0}", report.worst_visible_pt),
                wsid = report.worst_wsid,
                "handover mismatch: a real window is not where its tile finished"
            );
        }
    }

    /// Every tiled window's real frame against its intended one, read from the window server now.
    fn handover(&self) -> Option<HandoverReport> {
        let running = self.running.as_ref()?;
        let (display_frame, _) = self.display?;
        let tiled: Vec<WindowId> = running.tiles.iter().map(|t| t.window).collect();
        let real: HashMap<WindowId, CGRect> = running
            .final_frames
            .iter()
            .filter(|(window, _)| tiled.contains(window))
            .filter_map(|(window, _)| {
                let info = rini_windows::window_server::get_window(
                    rini_core::ids::WindowServerId::new(window.idx.get()),
                )?;
                Some((*window, info.frame))
            })
            .collect();
        Some(handover_report(&running.final_frames, &tiled, &real, display_frame))
    }

    /// Asks for a fresh desktop render in the background, or defers it to `finish` while a flight
    /// is up. Cheap to repeat.
    fn warm_desktop(&mut self) {
        if !capture_work_allowed(self.phase(), CaptureKind::Desktop) {
            self.deferred_desktop = true;
            return;
        }
        let (Some((frame, _)), Some(id)) = (self.display, self.display_id) else {
            return;
        };
        self.service.request_desktop(id, frame.size);
    }

    /// The desktop to draw behind the strips: the ScreenCaptureKit render when in hand, else a
    /// SkyLight composite. See "The wallpaper is not reliably a window" in the research doc.
    fn capture_backdrop(&mut self) -> Option<WindowSnapshot> {
        let (display_frame, scale) = self.display?;
        let display_size = (display_frame.size.width, display_frame.size.height);

        // A render asked for here would land mid-flight; `finish` asks instead.
        let in_hand = self
            .pictures
            .desktop
            .as_ref()
            .map(|render| (render.taken.elapsed(), render.coverage.covered));
        if desktop_render_wanted(in_hand, display_size) {
            self.deferred_desktop = true;
        }

        // Size-checked: a render for the other display can land after a display change. See "A
        // render of the wrong display, drawn at its own size" in `docs/capture-overlay-research.md`.
        if let Some(rendered) = self.pictures.desktop.clone().filter(|rendered| {
            crate::window_snapshot::spans_display(rendered.coverage.covered, display_size)
        }) {
            self.pictures.drawn_once = true;
            return Some(rendered);
        }

        // No render yet: the synchronous composite covers the gap rather than leaving the overlay black.
        let desktop = crate::backdrop::desktop_backdrop_windows(display_frame);
        let composite = crate::window_snapshot::capture_composite_via_skylight(
            &desktop.windows,
            display_size,
            scale,
        );
        let usable = composite.filter(|snapshot| {
            crate::window_snapshot::is_backdrop_worth_drawing(
                self.pictures.drawn_once,
                desktop.has_wallpaper,
                snapshot.coverage.covered,
                display_size,
            )
        });
        match usable {
            Some(snapshot) => {
                self.pictures.drawn_once = true;
                Some(snapshot)
            }
            None => {
                debug!(
                    windows = desktop.windows.len(),
                    has_wallpaper = desktop.has_wallpaper,
                    wanted = format!("{:.0}x{:.0}", display_size.0, display_size.1),
                    "no drawable desktop yet; keeping whatever the backdrop already holds"
                );
                None
            }
        }
    }

    /// Records what the overlay was dressed with; a black backdrop is only diagnosable after the fact.
    fn log_dressing(
        path: &str,
        backdrop: Option<&WindowSnapshot>,
        bar: Option<&WindowSnapshot>,
        strip: Option<CGRect>,
        held: bool,
    ) {
        debug!(
            path,
            held,
            backdrop = backdrop
                .map(|b| format!(
                    "{:.0}x{:.0} {:?}",
                    b.coverage.covered.0, b.coverage.covered.1, b.source
                ))
                .unwrap_or_else(|| "NONE".to_string()),
            bar = bar
                .map(|b| format!("{:.0}x{:.0}", b.coverage.covered.0, b.coverage.covered.1))
                .unwrap_or_else(|| "NONE".to_string()),
            strip = strip
                .map(|r| format!("{:.0},{:.0} {:.0}x{:.0}", r.origin.x, r.origin.y, r.size.width, r.size.height))
                .unwrap_or_else(|| "none".to_string()),
            "overlay dressed"
        );
    }

    /// The bar's held picture and where it sits in overlay coordinates. Only the very first call
    /// captures inline; [`Self::refresh_bar`] pays for the rest after an animation.
    fn bar_picture(&mut self) -> (Option<WindowSnapshot>, Option<CGRect>) {
        let Some((display_frame, _)) = self.display else { return (None, None) };
        let strip = crate::backdrop::bar_strip(display_frame);
        let Some(bounds) = strip.bounds else { return (None, None) };
        if self.pictures.bar.is_none() {
            self.refresh_bar();
        }
        let at = CGRect::new(
            CGPoint::new(
                bounds.origin.x - display_frame.origin.x,
                bounds.origin.y - display_frame.origin.y,
            ),
            bounds.size,
        );
        (self.pictures.bar.clone(), Some(at))
    }

    /// Asks for the bar to be recaptured after `BAR_REFRESH_DELAY`: a capture taken as the overlay
    /// hides still reads the overlay's own pixels out of the framebuffer.
    fn arm_bar_refresh(&mut self) {
        let tx = self.tx.clone();
        self.bar_refresh = RepeatingTimer::every(BAR_REFRESH_DELAY, move || {
            _ = tx.send(Event::RefreshBar);
        });
    }

    /// Recaptures the bar on its own, keeping its alpha; a no-op while the overlay covers it.
    /// See "The bar has to be captured on its own" in `docs/capture-overlay-research.md`.
    fn refresh_bar(&mut self) {
        let Some((display_frame, scale)) = self.display else { return };
        if self.overlay.as_ref().is_some_and(TileOverlay::is_visible) {
            return;
        }
        let strip = crate::backdrop::bar_strip(display_frame);
        let Some(bounds) = strip.bounds else { return };
        let fresh = crate::window_snapshot::capture_composite_via_skylight(
            &strip.windows,
            (bounds.size.width, bounds.size.height),
            scale,
        )
        .filter(|snapshot| snapshot.fits(bounds.size));
        if fresh.is_some() {
            self.pictures.bar = fresh;
        }
    }

    /// Asks the reactor to place windows at their final frames.
    fn request_frames(&self, frames: Vec<(WindowId, CGRect)>) {
        if frames.is_empty() {
            return;
        }
        let Some(place) = &self.place_frames else {
            warn!("no frame sink; cannot place windows at their final frames");
            return;
        };
        debug!(count = frames.len(), "placing real windows behind the overlay");
        place(frames);
    }

    /// Jumps to the end and tears down, for the case where no frame clock could be created.
    fn step_to_end(&mut self) {
        {
            let Self { overlay, running, .. } = self;
            if let (Some(overlay), Some(running)) = (overlay.as_mut(), running.as_ref()) {
                overlay.fly(&running.plan, Duration::ZERO);
            }
        }
        self.finish();
    }

    fn finish(&mut self) {
        // Whatever is still owed (a clockless flight's held entrance slots) goes out before lift.
        if let Some(frames) = self.running.as_mut().and_then(|running| running.frames_due(1.0)) {
            self.request_frames(frames);
        }
        self.report_handover_error();
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.hide();
            // The acceptance greps pair "placing real windows" with this line.
            let windows = self.running.as_ref().map_or(0, |running| running.tiles.len());
            debug!(windows, "overlay lifted");
            overlay.release_tiles();
        }
        let (harvested, focus) = self
            .running
            .take()
            .map(|running| (running.harvested, running.focus))
            .unwrap_or_default();
        if focus.is_some() {
            self.last_focus = focus;
        }
        self.coalesce = None;
        self.arm_bar_refresh();

        // The owed captures run after `SETTLE_BEFORE_CAPTURES`; a flight beginning first carries them over.
        let targets = std::mem::take(&mut self.last_animated);
        let after = self.after_flight.get_or_insert_with(AfterFlight::default);
        for target in targets {
            if !after.targets.iter().any(|t| t.window == target.window) {
                after.targets.push(target);
            }
        }
        after.harvested.extend(harvested);
        let tx = self.tx.clone();
        self.quiet = RepeatingTimer::every(SETTLE_BEFORE_CAPTURES, move || {
            _ = tx.send(Event::Quiet);
        });
        if self.quiet.is_none() {
            self.after_flight_captures();
        }
    }

    /// The capture work the last flights left, once per quiet period.
    fn after_flight_captures(&mut self) {
        self.quiet = None;
        if self.running.is_some() {
            return;
        }
        let Some(after) = self.after_flight.take() else { return };
        let animated: Vec<WindowId> = after.targets.iter().map(|target| target.window).collect();
        let requested =
            if after.targets.is_empty() { Vec::new() } else { self.warm_windows(after.targets) };
        let dressed: HashSet<WindowId> = animated
            .iter()
            .copied()
            .filter(|window| self.cache.get(*window).is_some_and(|s| s.dressing.is_some()))
            .collect();
        self.harvest_dressings(finish_harvest_set(&animated, &after.harvested, &requested, &dressed));
        let deferred = std::mem::take(&mut self.deferred_warm);
        if !deferred.is_empty() {
            self.warm_windows(deferred);
        }
        if std::mem::take(&mut self.deferred_desktop) {
            self.warm_desktop();
        }
    }

    /// Slides every window on screen in from an offset; touches no real window.
    fn debug_slide(&mut self, dx: f64, dy: f64, duration: Duration) {
        let Some((display_frame, _)) = self.display else {
            warn!("no display geometry yet; cannot run the debug slide");
            return;
        };
        let windows = rini_windows::window_server::visible_windows_on_display(display_frame);
        if windows.is_empty() {
            warn!("no visible windows found for the debug slide");
            return;
        }
        let requests: Vec<AnimationRequest> = windows
            .into_iter()
            .map(|(server_id, frame)| AnimationRequest {
                window: synthetic_window_id(server_id),
                server_id,
                // The debug slide knows nothing about the layout; everything is on the strip.
                floating: false,
                from: CGRect::new(
                    CGPoint::new(frame.origin.x + dx, frame.origin.y + dy),
                    frame.size,
                ),
                to: frame,
            })
            .collect();
        debug!(count = requests.len(), dx, dy, "running debug slide");
        self.start(requests, None, duration);
    }
}


/// Where a window really is right now, from the window server: the reactor's `from` can be the
/// previous pass's destination rather than where the window sits.
fn actual_start(request: &AnimationRequest, display: CGRect, travel: Option<CGPoint>) -> CGRect {
    let real = match rini_windows::window_server::get_window(request.server_id) {
        Some(info) if info.frame.size.width > 0.0 && info.frame.size.height > 0.0 => {
            Some(info.frame)
        }
        _ => None,
    };
    resolve_start(real, request.from, request.to, display, travel)
}













/// A stable [`WindowId`] derived from a window server id. Pid 0 keeps it clear of real ids.
fn synthetic_window_id(server_id: WindowServerId) -> WindowId {
    WindowId { pid: 0, idx: std::num::NonZeroU32::new(server_id.as_u32().max(1)).unwrap() }
}


#[cfg(test)]
mod tests {
    use super::*;


    /// The built-in display, for tests that need a screen to judge parks against.
    const DISPLAY: CGRect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize { width: 1728.0, height: 1117.0 },
    };





    #[test]
    fn a_resize_places_the_real_windows_earlier() {
        assert!(apply_frames_at(FlightKind::Layout, true) < apply_frames_at(FlightKind::Layout, false));
        assert_eq!(apply_frames_at(FlightKind::Layout, false), APPLY_FRAMES_AT);
        assert_eq!(apply_frames_at(FlightKind::Layout, true), APPLY_FRAMES_AT_RESIZE);
    }

    #[test]
    fn an_entrance_is_a_resize_from_zero_width() {
        let to = rect(100.0, 32.0, 859.0, 1081.0);
        let from = entrance_from(to);
        assert_eq!(from.origin.x, 100.0);
        assert_eq!(from.origin.y, 32.0);
        assert_eq!(from.size.width, 0.0);
        assert_eq!(from.size.height, 1081.0);
    }




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
        assert!(!managed.contains(&9001), "a border seen before is still a border");
        // The clamped park is not judged off screen, so geometry alone cannot exclude it.
        let park = rect(1727.0, 1076.0, 1720.0, 1081.0);
        assert!(!rini_geometry::is_off_screen(DISPLAY, park));
        assert!(companion_of(park, &[(WindowServerId::new(102682), park)], DISPLAY).is_some());
    }

    #[test]
    fn a_parked_window_neither_traces_nor_is_traced() {
        let park = rect(DISPLAY.size.width - 1.0, DISPLAY.size.height - 1.0, 859.0, 1081.0);
        assert!(rini_geometry::is_off_screen(DISPLAY, park));
        let twin = (WindowServerId::new(7), park);
        assert!(companion_of(park, &[twin], DISPLAY).is_none(), "a parked anchor");
        let on_screen = rect(4.0, 32.0, 859.0, 1081.0);
        let border = (WindowServerId::new(8), rect(1.0, 29.0, 865.0, 1087.0));
        assert!(companion_of(on_screen, &[twin, border], DISPLAY).map(|(id, _)| id.as_u32()) == Some(8));
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
    fn forgetting_a_display_leaves_no_picture_behind() {
        let mut pictures = DisplayPictures {
            shown: None,
            desktop: None,
            bar: None,
            drawn_once: true,
        };
        pictures.forget();
        assert!(pictures.shown.is_none());
        assert!(pictures.desktop.is_none());
        assert!(pictures.bar.is_none());
        assert!(!pictures.drawn_once, "a display we have never drawn has no backdrop to keep");
    }

    mod refresh {
        use super::*;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 1, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }




        #[test]
        fn refresh_targets_are_the_two_ends_of_a_focus_change() {
            let tiles = [wid(1), wid(2), wid(3)];
            // No change: a strip pan with focus where it was.
            assert!(refresh_targets(Some(wid(1)), Some(wid(1)), &tiles).is_empty());
            // No current focus: nothing to recapture towards.
            assert!(refresh_targets(Some(wid(1)), None, &tiles).is_empty());
            assert!(refresh_targets(None, None, &tiles).is_empty());
            // A change between two tiles: both, destination first.
            assert_eq!(refresh_targets(Some(wid(1)), Some(wid(2)), &tiles), vec![wid(2), wid(1)]);
            // Only one end is in the flight: only that one.
            assert_eq!(refresh_targets(Some(wid(9)), Some(wid(2)), &tiles), vec![wid(2)]);
            assert_eq!(refresh_targets(Some(wid(1)), Some(wid(9)), &tiles), vec![wid(1)]);
            // First flight ever: the destination alone.
            assert_eq!(refresh_targets(None, Some(wid(3)), &tiles), vec![wid(3)]);
            // Neither end tiled: nothing, whatever else is flying.
            assert!(refresh_targets(Some(wid(8)), Some(wid(9)), &tiles).is_empty());
        }

        #[test]
        fn a_strip_pan_with_unchanged_focus_requests_no_refresh() {
            let size = CGSize::new(859.0, 1081.0);
            let tiles = [
                (wid(1), WindowServerId::new(10), size),
                (wid(2), WindowServerId::new(20), size),
                (wid(3), WindowServerId::new(30), size),
            ];
            let windows: Vec<WindowId> = tiles.iter().map(|(w, _, _)| *w).collect();

            let pan = refresh_targets(Some(wid(2)), Some(wid(2)), &windows);
            assert!(pan.is_empty());
            assert!(refresh_requests(&tiles, &pan).1.is_empty(), "nothing is captured");

            let change = refresh_targets(Some(wid(2)), Some(wid(3)), &windows);
            let (covered, requests) = refresh_requests(&tiles, &change);
            assert_eq!(covered, vec![wid(3), wid(2)]);
            let asked: Vec<u32> = requests.iter().map(|t| t.server_id.as_u32()).collect();
            assert_eq!(asked, vec![30, 20], "exactly the two ends");
        }
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    /// Bug-condition exploration for `.kiro/specs/exit-entrance-animation-regressions/bugfix.md`;
    /// each test names the clause it pins.
    mod exploration {
        use super::*;
                use crate::window_snapshot::test_snapshot;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        fn tile(
            window: WindowId,
            from: CGRect,
            to: CGRect,
            server_order: Option<usize>,
            floating: bool,
        ) -> OverlayTile {
            OverlayTile {
                window,
                from,
                to,
                snapshot: test_snapshot(to.size),
                floating,
                server_order,
                depth: 0,
                companion: false,
                focused: false,
            }
        }

        fn running(started: Option<Instant>, duration: Duration) -> RunningAnimation {
            RunningAnimation {
                tiles: Vec::new(),
                final_frames: Vec::new(),
                frames_applied: false,
                started,
                duration,
                apply_at: APPLY_FRAMES_AT,
                entrances: Vec::new(),
                awaiting: Vec::new(),
                hold_deadline: None,
                destination_refreshed: false,
                refresh_targets: Vec::new(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                _clock: None,
            }
        }

        /// T1 (1.7).
        #[test]
        fn a_coalescing_merge_re_requests_frames_it_already_applied() {
            let window = wid(1);
            let a = rect(1727.0, 1116.0, 859.0, 1081.0);
            let b = rect(867.0, 32.0, 859.0, 1081.0);
            let mut running = running(None, Duration::from_millis(300));
            running.frames_applied = true;
            running.final_frames = vec![(window, a)];

            let changed = merge_final_frames(&mut running.final_frames, vec![(window, b)]);
            let reapply = reapply_set(
                running.frames_applied,
                running.started.is_some(),
                changed,
                &running.final_frames,
            );

            assert!(changed, "B differs from A");
            assert_eq!(running.final_frames, vec![(window, b)], "latest frame wins");
            assert_eq!(
                reapply,
                Some(vec![(window, b)]),
                "frames applied early and then merged must be re-requested; got {reapply:?}"
            );
        }

        /// T2 (1.4).
        #[test]
        fn a_late_entrance_ends_no_later_than_the_flight() {
            let duration = Duration::from_millis(300);
            let running = running(Some(Instant::now() - duration.mul_f64(0.6)), duration);
            // Read before `progress`: the clock only moves forward between the two reads.
            let remaining = running.remaining();
            let progress = running.progress();
            assert!(progress >= 0.6 && progress < 0.7, "clock sanity: {progress}");

            let entrance = late_join_duration(running.duration, progress);
            assert!(
                entrance <= remaining,
                "entrance travels {entrance:?} but the overlay lifts in {remaining:?}"
            );
        }

        /// T4 (1.1). Depth is banded from the flight's latest focus for every tile, redundant ones included.
        #[test]
        fn depths_do_not_interleave_groups_across_passes() {
            let (s1, s2, f) = (wid(1), wid(2), wid(3));
            let slot_a = rect(4.0, 32.0, 859.0, 1081.0);
            let slot_b = rect(867.0, 32.0, 859.0, 1081.0);
            let slot_c = rect(1730.0, 32.0, 859.0, 1081.0);
            let zoom = rect(224.0, 95.0, 1280.0, 960.0);

            let pass1 = vec![
                tile(s1, slot_a, slot_a, Some(1), false),
                tile(s2, slot_c, slot_b, Some(2), false),
                tile(f, zoom, zoom, Some(0), true),
            ];
            let mut flight = running(None, Duration::from_millis(300));
            for (_, outcome) in flight.merge_pass(pass1, None) {
                assert_eq!(outcome, Admitted::Joined);
            }

            let pass2 = vec![
                tile(s1, slot_a, slot_a, Some(1), false),
                tile(s2, slot_c, slot_c, Some(2), false),
                tile(f, zoom, zoom, Some(0), true),
            ];
            let outcomes: Vec<Admitted> =
                flight.merge_pass(pass2, Some(f)).into_iter().map(|(_, o)| o).collect();
            assert_eq!(
                outcomes,
                vec![Admitted::Redundant, Admitted::Retargeted, Admitted::Redundant],
                "S1 confirmed, S2 retargeted, F confirmed"
            );

            let depth = |w: WindowId| flight.tiles.iter().find(|t| t.window == w).unwrap().depth;
            let (d1, d2, df) = (depth(s1), depth(s2), depth(f));
            assert!(
                !(d1 < df && df < d2) && !(d2 < df && df < d1),
                "S1={d1} S2={d2} F={df}: the floating window is between two strip tiles"
            );
            assert!(df < d1 && df < d2, "the floating focus leads, and its group with it");
        }

        /// T5 (1.2, 1.3). Parks clamped past 40pt (Kiro 41pt, Finder 52pt) and one the server reports at
        /// its slot.
        #[test]
        fn a_clamped_park_enters_from_the_display_edge() {
            let display = rect(0.0, 0.0, 1728.0, 1117.0);
            let slot = rect(4.0, 32.0, 1720.0, 1081.0);
            let park = rect(1727.0, 1116.0, 1720.0, 1081.0);
            let kiro_real = rect(1727.0, 1076.0, 1720.0, 1081.0);
            let finder_real = rect(1727.0, 1065.0, 859.0, 1081.0);
            let finder_slot = rect(867.0, 32.0, 859.0, 1081.0);
            let finder_park = rect(1727.0, 1116.0, 859.0, 1081.0);

            let cases = [
                ("41pt Kiro park", Some(kiro_real), park, slot),
                ("52pt Finder park", Some(finder_real), finder_park, finder_slot),
                ("park the server already reports at its slot", Some(slot), park, slot),
            ];
            let wrong: Vec<String> = cases
                .iter()
                .filter_map(|(name, real, from, to)| {
                    let expected = rini_geometry::park_entry_frame(*from, *to, display);
                    let got = resolve_start(*real, *from, *to, display, None);
                    (got != expected).then(|| {
                        format!(
                            "{name}: started at {:.0},{:.0}, wanted {:.0},{:.0}",
                            got.origin.x, got.origin.y, expected.origin.x, expected.origin.y
                        )
                    })
                })
                .collect();
            assert!(wrong.is_empty(), "parks not remapped to the edge:\n{}", wrong.join("\n"));
        }

        /// T7 (1.8).
        #[test]
        fn a_pass_where_nothing_drawable_moves_does_not_fly() {
            assert!(
                !worth_flying(false, false),
                "a still-only composition flew, hiding the new window until its picture landed"
            );
        }
    }

    /// Bug-condition exploration for `.kiro/specs/flight-render-stability/bugfix.md`; each test
    /// names the clause it pins.
    mod render_stability_exploration {
        use super::*;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        /// T1 (1.1).
        #[test]
        fn a_background_picture_never_swaps_onto_a_moving_tile() {
            let decision = should_swap_mid_flight(
                TileState::Moving { fits: true, resizing: false },
                false,
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

        /// T2 (1.1, 1.2).
        #[test]
        fn a_refresh_landing_late_is_cached_only() {
            let decision = should_swap_mid_flight(
                TileState::MovingRefreshTarget { fits: true, resizing: false },
                false,
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

        /// T3 (1.2).
        #[test]
        fn no_capture_work_starts_between_frame_zero_and_lift() {
            let cases = [
                (FlightPhase::Moving, CaptureKind::Warm),
                (FlightPhase::Moving, CaptureKind::Desktop),
                (FlightPhase::Holding, CaptureKind::Refresh),
                (FlightPhase::Moving, CaptureKind::Harvest),
            ];
            let allowed: Vec<String> = cases
                .iter()
                .filter(|(phase, kind)| capture_work_allowed(*phase, *kind))
                .map(|(phase, kind)| format!("{phase:?}/{kind:?}"))
                .collect();
            assert!(allowed.is_empty(), "capture work allowed in flight: {}", allowed.join(", "));
        }

        /// T4 (1.3).
        #[test]
        fn a_strip_movement_applies_frames_by_the_midpoint() {
            let at = apply_frames_at(FlightKind::Pan, false);
            assert!(at <= 0.5, "strip apply point is {at}, leaving too little runway");
        }

        /// T6 (1.3).
        #[test]
        fn an_untiled_frame_change_in_flight_marks_frames_stale() {
            assert!(
                mark_stale_on_untiled_change(false, true),
                "frames_applied stays true after an untiled frame change"
            );
        }

        /// T7 (1.3). A park clamped by macOS is not the flight's error.
        #[test]
        fn the_handover_report_excludes_off_screen_intents_and_counts_misses() {
            let display = rect(0.0, 0.0, 1728.0, 1117.0);
            let parked = wid(108);
            let strip = wid(200);
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
                report.count_over == 1 && report.worst_visible_pt == 3.0 && report.worst_wsid == 200,
                "expected count_over=1 worst=3pt wsid=200; got {report:?}"
            );
        }

        /// T8 (1.4). See "A grow holds, then reveals" in `docs/animation-smoothness.md`.
        #[test]
        fn a_hold_is_short_and_settles_on_the_first_repaint() {
            let limit = reveal_hold_limit(Duration::from_millis(300));
            let p = vec![10u8; 64];
            let q = vec![200u8; 64];
            let settled = chase_settled(None, &p, Some(&q));
            let wrong: Vec<String> = [
                (limit <= HOLD_CAP, format!("reveal_hold_limit(300ms) = {limit:?}")),
                (
                    REVEAL_CHASE_INTERVAL == Duration::from_millis(8),
                    format!("REVEAL_CHASE_INTERVAL = {REVEAL_CHASE_INTERVAL:?}"),
                ),
                (settled, format!("chase_settled(None, p, Some(q != p)) = {settled}")),
            ]
            .into_iter()
            .filter(|(ok, _)| !ok)
            .map(|(_, why)| why)
            .collect();
            assert!(wrong.is_empty(), "hold is not bounded and cheap:\n{}", wrong.join("\n"));
        }

        /// T10 (1.6), inverted: a holding flight sends every frame at frame zero, the newcomer's included.
        #[test]
        fn an_entrances_frame_goes_out_at_frame_zero_so_its_picture_fits() {
            let newcomer = wid(51462);
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let final_frames = vec![
                (wid(1), rect(4.0, 32.0, 859.0, 1081.0)),
                (newcomer, slot),
                (wid(2), rect(1730.0, 32.0, 859.0, 1081.0)),
            ];
            let mut running = super::render_stability_fix::flight(None);
            running.final_frames = final_frames.clone();
            let (entrance, waiting) = entrance_reservation(newcomer, slot, false);
            running.entrances.push(entrance);
            let awaiting: Vec<_> = waiting.into_iter().collect();
            let sent = running
                .extend_hold(&awaiting, false, Duration::from_millis(300), Instant::now())
                .expect("a held merge sends frames");
            assert_eq!(sent, final_frames, "the newcomer's slot went out with the rest");
            assert!(running.frames_applied);
            assert_eq!(running.frames_due(1.0), None, "nothing left for the apply point");
        }
    }

    /// Fix checking for `.kiro/specs/flight-render-stability/bugfix.md` 2.x.
    mod render_stability_fix {
        use super::preservation::{Gen, RUNS};
        use super::*;
        use crate::window_snapshot::test_snapshot;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        pub(super) fn flight(started: Option<Instant>) -> RunningAnimation {
            RunningAnimation {
                tiles: Vec::new(),
                final_frames: Vec::new(),
                frames_applied: false,
                started,
                duration: Duration::from_millis(300),
                apply_at: APPLY_FRAMES_AT,
                entrances: Vec::new(),
                awaiting: Vec::new(),
                hold_deadline: None,
                destination_refreshed: false,
                refresh_targets: Vec::new(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                _clock: None,
            }
        }

        fn tile(window: WindowId, from: CGRect, to: CGRect) -> OverlayTile {
            OverlayTile {
                window,
                from,
                to,
                snapshot: test_snapshot(to.size),
                floating: false,
                server_order: None,
                depth: 0,
                companion: false,
                focused: false,
            }
        }

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

        /// The rule in 2.1, spelled out independently of the implementation.
        fn expected(
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
                // 2.4: a settled reveal before 0.6 is swapped whatever route it came by.
                TileState::Reveal { fits } => {
                    if fits && settled && progress.is_some_and(|p| p < 0.6) {
                        SwapDecision::Swap("reveal")
                    } else {
                        SwapDecision::CacheOnly
                    }
                }
                TileState::MovingRefreshTarget { fits: true, resizing } => {
                    let early = progress.is_some_and(|p| p < 0.6);
                    if early && same_source && !renders_like_cached && (!resizing || settled) {
                        SwapDecision::Swap("refresh")
                    } else {
                        SwapDecision::CacheOnly
                    }
                }
                _ => SwapDecision::CacheOnly,
            }
        }

        /// 2.1.
        #[test]
        fn should_swap_mid_flight_full_table() {
            let progresses = [None, Some(0.3), Some(0.59), Some(0.6), Some(0.9)];
            let mut swaps = 0usize;
            for state in STATES {
                for settled in [false, true] {
                    for same in [false, true] {
                        for same_source in [false, true] {
                            for progress in progresses {
                                let got =
                                    should_swap_mid_flight(state, settled, same, same_source, progress);
                                assert_eq!(
                                    got,
                                    expected(state, settled, same, same_source, progress),
                                    "{state:?} settled={settled} same={same} same_source={same_source} progress={progress:?}"
                                );
                                if matches!(got, SwapDecision::Swap(_)) {
                                    swaps += 1;
                                }
                            }
                        }
                    }
                }
            }
            // Refresh: fits, differs, same route, 2 early progresses x 3 (settle x resizing) = 6;
            // reveal: fits, settled, 2 same x 2 routes x 2 progresses = 8.
            assert_eq!(swaps, 6 + 8, "the table has exactly the early refresh and reveal swaps");
        }

        /// 2.1, 2.4. Seed 95, 200 runs.
        #[test]
        fn swap_only_for_the_early_refresh_target() {
            let mut rng = Gen(95);
            let mut swaps = 0usize;
            for _ in 0..RUNS {
                let state = STATES[rng.below(STATES.len() as u64) as usize];
                let settled = rng.coin();
                let same = rng.coin();
                let same_source = rng.coin();
                let progress = rng.coin().then(|| rng.below(1001) as f64 / 1000.0);
                let decision = should_swap_mid_flight(state, settled, same, same_source, progress);
                if let SwapDecision::Swap(reason) = decision {
                    swaps += 1;
                    match state {
                        TileState::MovingRefreshTarget { fits: true, .. } => {
                            assert_eq!(reason, "refresh", "seed 95: {state:?}");
                            assert!(!same, "seed 95: swapped a picture rendering like the cache");
                            assert!(same_source, "seed 95: swapped a picture from another route");
                        }
                        TileState::Reveal { fits: true } => {
                            assert_eq!(reason, "reveal", "seed 95: {state:?}");
                            assert!(settled, "seed 95: an unsettled reveal swapped");
                        }
                        other => panic!("seed 95: swapped onto {other:?}"),
                    }
                    assert!(progress.is_some_and(|p| p < 0.6), "seed 95: swap at {progress:?}");
                }
                if matches!(state, TileState::Moving { .. }) {
                    assert_eq!(decision, SwapDecision::CacheOnly, "seed 95: {state:?}");
                }
            }
            assert!(swaps > 0, "generator sanity: no Swap in {RUNS} runs");
        }

        /// A picture from another route differs by route alone, so it never reaches the tile.
        #[test]
        fn a_route_change_alone_never_swaps_the_refresh() {
            for settled in [false, true] {
                for resizing in [false, true] {
                    let decision = should_swap_mid_flight(
                        TileState::MovingRefreshTarget { fits: true, resizing },
                        settled,
                        false,
                        false,
                        Some(0.3),
                    );
                    assert_eq!(
                        decision,
                        SwapDecision::CacheOnly,
                        "settled={settled} resizing={resizing}: a route change swapped"
                    );
                }
            }
        }

        #[test]
        fn the_same_route_rendering_differently_swaps_the_refresh() {
            assert_eq!(
                should_swap_mid_flight(
                    TileState::MovingRefreshTarget { fits: true, resizing: false },
                    false,
                    false,
                    true,
                    Some(0.3),
                ),
                SwapDecision::Swap("refresh")
            );
        }

        /// Seed 97, 200 runs.
        #[test]
        fn a_refresh_swap_implies_the_same_route() {
            let mut rng = Gen(97);
            let mut swaps = 0usize;
            for _ in 0..RUNS {
                let state = STATES[rng.below(STATES.len() as u64) as usize];
                let same_source = rng.coin();
                let progress = rng.coin().then(|| rng.below(1001) as f64 / 1000.0);
                let decision =
                    should_swap_mid_flight(state, rng.coin(), rng.coin(), same_source, progress);
                if decision == SwapDecision::Swap("refresh") {
                    swaps += 1;
                    assert!(same_source, "seed 97: {state:?} at {progress:?} swapped across routes");
                }
            }
            assert!(swaps > 0, "generator sanity: no refresh swap in {RUNS} runs");
        }

        #[test]
        fn the_destination_refresh_uses_one_route() {
            let size = CGSize::new(859.0, 1081.0);
            let tiles = vec![
                (wid(1), WindowServerId::new(10), size),
                (wid(2), WindowServerId::new(20), size),
                (wid(3), WindowServerId::new(30), size),
            ];
            let (covered, requests) = refresh_requests(&tiles, &[wid(2), wid(1), wid(9)]);
            assert_eq!(covered, vec![wid(2), wid(1)], "an unknown window is not a target");
            let asked: Vec<(WindowId, u32)> =
                requests.iter().map(|t| (t.window, t.server_id.as_u32())).collect();
            assert_eq!(asked, vec![(wid(2), 20), (wid(1), 10)], "one request per window");
            assert!(requests.iter().all(|t| t.size == size));
            assert!(refresh_requests(&tiles, &[]).1.is_empty());
        }

        /// 2.1, 3.6.
        #[test]
        fn one_refresh_per_flight_at_the_midpoint_and_none_at_frame_zero() {
            let mut holding = flight(None);
            holding.awaiting.push((wid(1), CGSize::new(859.0, 1081.0)));
            assert!(!holding.take_refresh(0.0), "a hold does not refresh");
            assert!(!holding.destination_refreshed);

            let mut running = flight(Some(Instant::now()));
            let fired: Vec<f64> = (0..=100)
                .map(|i| i as f64 / 100.0)
                .filter(|&progress| running.take_refresh(progress))
                .collect();
            assert_eq!(fired, vec![0.5], "refresh slots taken: {fired:?}");
            assert!(running.destination_refreshed);
            assert!(!(0..=100).any(|i| running.take_refresh(i as f64 / 100.0)), "spent");
            assert_eq!(REFRESH_DESTINATION_AT, 0.5);
            assert_eq!(REFRESH_APPLY_BEFORE, 0.6);
        }

        /// 2.1.
        #[test]
        fn tile_state_ranks_hold_over_refresh_over_tile() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let parked = rect(-1720.0, 32.0, 859.0, 1081.0);
            let grown = rect(4.0, 32.0, 1147.0, 1081.0);
            let picture = test_snapshot(slot.size);
            let mut running = flight(None);
            running.tiles.push(tile(wid(1), parked, slot));
            running.tiles.push(tile(wid(2), slot, grown));
            running.tiles.push(tile(wid(3), parked, slot));
            running.awaiting.push((wid(2), grown.size));
            running.refresh_targets.push(wid(3));
            let (entrance, waiting) = entrance_reservation(wid(4), slot, false);
            running.entrances.push(entrance);
            running.awaiting.extend(waiting);

            assert_eq!(
                running.tile_state(wid(1), &picture),
                TileState::Moving { fits: true, resizing: false }
            );
            assert_eq!(
                running.tile_state(wid(2), &picture),
                TileState::Reveal { fits: false },
                "a grow's hold"
            );
            assert_eq!(
                running.tile_state(wid(2), &test_snapshot(grown.size)),
                TileState::Reveal { fits: true },
                "a grow's hold, its reveal landing"
            );
            assert_eq!(
                running.tile_state(wid(3), &picture),
                TileState::MovingRefreshTarget { fits: true, resizing: false }
            );
            assert_eq!(running.tile_state(wid(4), &picture), TileState::Awaiting, "an entrance");
            assert_eq!(running.tile_state(wid(5), &picture), TileState::NotTiled);

            // Once claimed, the grow is an ordinary resizing tile: a smaller picture no longer fits.
            running.awaiting.retain(|(w, _)| *w != wid(2));
            assert_eq!(
                running.tile_state(wid(2), &picture),
                TileState::Moving { fits: false, resizing: true }
            );
            // A refresh target in flight but past its hold is still only the refresh target.
            running.refresh_targets.push(wid(2));
            assert_eq!(
                running.tile_state(wid(2), &test_snapshot(grown.size)),
                TileState::MovingRefreshTarget { fits: true, resizing: true }
            );
        }


        const PHASES: [FlightPhase; 4] =
            [FlightPhase::Idle, FlightPhase::FrameZero, FlightPhase::Holding, FlightPhase::Moving];
        const KINDS: [CaptureKind; 6] = [
            CaptureKind::Warm,
            CaptureKind::Desktop,
            CaptureKind::Refresh,
            CaptureKind::Chase,
            CaptureKind::Harvest,
            CaptureKind::NeedsCapture,
        ];

        /// The rule in 2.2, spelled out independently of the implementation.
        fn expected_allowed(phase: FlightPhase, kind: CaptureKind) -> bool {
            match (phase, kind) {
                (FlightPhase::Idle, _) => true,
                (_, CaptureKind::Chase) => true,
                (FlightPhase::FrameZero, CaptureKind::NeedsCapture) => true,
                (FlightPhase::Moving, CaptureKind::Refresh) => true,
                _ => false,
            }
        }

        fn target(idx: u32, width: f64) -> SnapshotTarget {
            SnapshotTarget {
                window: wid(idx),
                server_id: WindowServerId::new(idx),
                size: CGSize::new(width, 1081.0),
            }
        }

        /// 2.2. The full table over phase x kind.
        #[test]
        fn capture_work_allowed_full_table() {
            let mut allowed = 0usize;
            for phase in PHASES {
                for kind in KINDS {
                    let got = capture_work_allowed(phase, kind);
                    assert_eq!(got, expected_allowed(phase, kind), "{phase:?}/{kind:?}");
                    allowed += got as usize;
                }
            }
            // Idle 6, frame zero 2, holding 1, moving 2.
            assert_eq!(allowed, 11);
        }

        /// 2.2. Seed 96, 200 runs.
        #[test]
        fn in_flight_capture_work_is_only_a_chase_or_the_refresh() {
            let mut rng = Gen(96);
            let mut allowed = 0usize;
            for _ in 0..RUNS {
                let phase = PHASES[rng.below(PHASES.len() as u64) as usize];
                let kind = KINDS[rng.below(KINDS.len() as u64) as usize];
                if phase == FlightPhase::Idle {
                    assert!(capture_work_allowed(phase, kind), "seed 96: idle refused {kind:?}");
                    continue;
                }
                if capture_work_allowed(phase, kind) {
                    allowed += 1;
                    assert!(
                        matches!(kind, CaptureKind::Chase | CaptureKind::Refresh)
                            || (phase == FlightPhase::FrameZero
                                && kind == CaptureKind::NeedsCapture),
                        "seed 96: {kind:?} allowed at {phase:?}"
                    );
                    if kind == CaptureKind::Refresh {
                        assert_eq!(phase, FlightPhase::Moving, "seed 96: a refresh while holding");
                    }
                }
            }
            assert!(allowed > 0, "generator sanity: nothing allowed in {RUNS} runs");
        }

        /// 2.2, 3.4.
        #[test]
        fn a_mid_flight_warm_is_deferred_once_per_window_and_drained_once() {
            let mut deferred: Vec<SnapshotTarget> = Vec::new();
            defer_warm(&mut deferred, vec![target(1, 859.0), target(2, 859.0)]);
            defer_warm(&mut deferred, vec![target(1, 1147.0), target(3, 859.0)]);
            let windows: Vec<WindowId> = deferred.iter().map(|t| t.window).collect();
            assert_eq!(windows, vec![wid(1), wid(2), wid(3)], "one entry per window");
            assert_eq!(deferred[0].size.width, 1147.0, "the later request replaces the earlier");

            let drained = std::mem::take(&mut deferred);
            assert_eq!(drained.len(), 3);
            assert!(deferred.is_empty(), "a second drain has nothing");

            // The desktop flag drains the same way.
            let mut deferred_desktop = true;
            assert!(std::mem::take(&mut deferred_desktop));
            assert!(!std::mem::take(&mut deferred_desktop), "drained once");
        }

        /// 2.2.
        #[test]
        fn the_desktop_render_is_wanted_when_missing_misfit_or_stale() {
            let display = (1728.0, 1117.0);
            let fresh = Duration::from_millis(500);
            let stale = Duration::from_secs(3);
            assert!(desktop_render_wanted(None, display), "missing");
            assert!(desktop_render_wanted(Some((fresh, (2560.0, 1440.0))), display), "misfit");
            assert!(desktop_render_wanted(Some((stale, display)), display), "stale");
            assert!(!desktop_render_wanted(Some((fresh, display)), display), "in hand");
        }

        /// 2.2.
        #[test]
        fn finish_harvests_each_animated_window_at_most_once() {
            let animated = [wid(1), wid(2), wid(3), wid(4), wid(2)];
            let mut harvested = HashSet::new();
            assert!(harvested.insert(wid(1)), "the chase's dressing");
            assert!(!harvested.insert(wid(1)), "a second harvest for the same window is skipped");
            let requested = [wid(3)];
            let dressed: HashSet<WindowId> = [wid(4)].into_iter().collect();
            assert_eq!(
                finish_harvest_set(&animated, &harvested, &requested, &dressed),
                vec![wid(2)]
            );
            assert!(
                finish_harvest_set(&[], &harvested, &requested, &dressed).is_empty(),
                "nothing animated, nothing harvested"
            );
        }

        /// 2.2.
        #[test]
        fn a_flight_starts_with_nothing_harvested() {
            let running = flight(None);
            assert!(running.harvested.is_empty());
            assert!(!capture_work_allowed(running.phase(), CaptureKind::Harvest));
        }


        const DISPLAY: CGRect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width: 1728.0, height: 1117.0 },
        };

        /// 2.3, 3.3.
        #[test]
        fn apply_points_by_flight_kind() {
            assert_eq!(apply_frames_at(FlightKind::Layout, false), 0.75);
            assert_eq!(apply_frames_at(FlightKind::Layout, true), 0.5);
            assert_eq!(apply_frames_at(FlightKind::Pan, false), APPLY_FRAMES_AT_PAN);
            assert_eq!(APPLY_FRAMES_AT_PAN, 0.0, "a strip movement places its windows at frame zero");
            assert_eq!(apply_frames_at(FlightKind::Pan, true), 0.5);
        }

        /// 2.3. Applied frames go stale when a tile changed or any final frame did.
        #[test]
        fn frames_go_stale_on_a_tile_or_an_untiled_change() {
            assert!(!mark_stale_on_untiled_change(false, false));
            assert!(mark_stale_on_untiled_change(true, false));
            assert!(mark_stale_on_untiled_change(false, true));
            assert!(mark_stale_on_untiled_change(true, true));
        }

        /// 2.3.
        #[test]
        fn an_untiled_frame_change_in_flight_clears_frames_applied() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let park_a = rect(1727.0, 1116.0, 859.0, 1081.0);
            let park_b = rect(-858.0, 1116.0, 859.0, 1081.0);
            let mut running = flight(Some(Instant::now()));
            running.tiles.push(tile(wid(1), rect(867.0, 32.0, 859.0, 1081.0), slot));
            running.final_frames = vec![(wid(1), slot), (wid(2), park_a)];
            running.frames_applied = true;

            // The same tile again, the untiled window to the other park.
            let frames_changed =
                merge_final_frames(&mut running.final_frames, vec![(wid(1), slot), (wid(2), park_b)]);
            let outcomes = running.merge_pass(vec![tile(wid(1), slot, slot)], None);
            let changed = outcomes.iter().any(|(_, o)| *o != Admitted::Redundant);
            assert!(frames_changed && !changed, "the pass changes only the untiled frame");
            running.absorb_in_flight_change(changed, frames_changed);
            assert!(!running.frames_applied, "an untiled change left frames_applied set");

            // Nothing changes: the applied frames stand.
            running.frames_applied = true;
            let frames_changed =
                merge_final_frames(&mut running.final_frames, vec![(wid(1), slot), (wid(2), park_b)]);
            let outcomes = running.merge_pass(vec![tile(wid(1), slot, slot)], None);
            let changed = outcomes.iter().any(|(_, o)| *o != Admitted::Redundant);
            running.absorb_in_flight_change(changed, frames_changed);
            assert!(running.frames_applied, "a redundant pass cleared frames_applied");
        }

        fn measured(
            frames: &[(WindowId, CGRect, CGRect)],
        ) -> (Vec<(WindowId, CGRect)>, Vec<WindowId>, HashMap<WindowId, CGRect>) {
            let final_frames = frames.iter().map(|(w, intended, _)| (*w, *intended)).collect();
            let tiled = frames.iter().map(|(w, _, _)| *w).collect();
            let real = frames.iter().map(|(w, _, actual)| (*w, *actual)).collect();
            (final_frames, tiled, real)
        }

        /// 2.3. The log's cases: a clamped park excluded, a leaving window's park miss excluded, two
        /// on-screen misses counted.
        #[test]
        fn handover_report_counts_on_screen_misses_only() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            // The park: clamped by macOS, not the flight's error.
            let clamp = [
                (wid(108), rect(1727.0, 1116.0, 859.0, 1081.0), rect(1727.0, 1051.0, 859.0, 1081.0)),
                (wid(200), slot, rect(870.0, 32.0, 859.0, 1081.0)),
            ];
            let (f, t, r) = measured(&clamp);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 1, "108 is excluded from the measured set");
            assert_eq!(report.count_over, 1);
            assert_eq!(report.worst_visible_pt, 3.0);
            assert_eq!(report.worst_wsid, 200);

            // The leaving window: intended at its park past the right edge, still in its slot.
            let park_miss = [
                (wid(1), rect(1728.0, 32.0, 1720.0, 1081.0), rect(-1718.0, 32.0, 1720.0, 1081.0)),
                (wid(2), rect(4.0, 32.0, 1720.0, 1081.0), rect(4.0, 32.0, 1720.0, 1081.0)),
            ];
            let (f, t, r) = measured(&park_miss);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 1);
            assert_eq!(report.count_over, 0, "a park intent is not measured");
            assert_eq!(report.worst_visible_pt, 0.0);

            let two = [
                (wid(1), rect(4.0, 32.0, 859.0, 1081.0), rect(9.0, 32.0, 859.0, 1081.0)),
                (wid(2), slot, rect(867.0, 40.0, 859.0, 1081.0)),
                (wid(3), rect(1730.0, 32.0, 859.0, 1081.0), rect(1730.0, 32.0, 859.0, 1081.0)),
            ];
            let (f, t, r) = measured(&two);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 2, "wid 3 starts past the edge: excluded");
            assert_eq!(report.count_over, 2);
            assert_eq!(report.worst_visible_pt, 8.0);
            assert_eq!(report.worst_wsid, 2);

            let clean = [(wid(1), slot, slot), (wid(2), rect(4.0, 32.0, 859.0, 1081.0), rect(5.0, 32.0, 859.0, 1081.0))];
            let (f, t, r) = measured(&clean);
            let report = handover_report(&f, &t, &r, DISPLAY);
            assert_eq!(report.total, 2);
            assert_eq!(report.count_over, 0, "1pt is within the threshold");
            assert_eq!(report.worst_visible_pt, 1.0);
        }

        /// 2.3. Seed 97, 200 runs.
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
                    frames.push((wid(i as u32), intended, actual));
                }
                let (f, t, r) = measured(&frames);
                let report = handover_report(&f, &t, &r, DISPLAY);

                let visible: Vec<_> = frames
                    .iter()
                    .filter(|(_, intended, _)| {
                        !rini_geometry::is_off_screen(DISPLAY, *intended)
                    })
                    .collect();
                let error = |intended: &CGRect, actual: &CGRect| {
                    (actual.origin.x - intended.origin.x)
                        .abs()
                        .max((actual.origin.y - intended.origin.y).abs())
                };
                let expected_over =
                    visible.iter().filter(|(_, i, a)| error(i, a) > 2.0).count();
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


        /// The hold bound before Change D, kept here so the cap is checked against it.
        fn reveal_hold_limit_old(duration: Duration) -> Duration {
            duration.mul_f64(0.4).max(Duration::from_millis(300))
        }

        /// 2.4.
        #[test]
        fn chase_settled_on_a_match_or_a_repaint() {
            let p = vec![10u8; 64];
            let q = vec![200u8; 64];
            assert!(!chase_settled(None, &p, None), "nothing to compare against");
            assert!(chase_settled(Some(&p), &p, None), "two consecutive match");
            assert!(chase_settled(None, &p, Some(&q)), "differs from the pre-resize picture");
            assert!(!chase_settled(None, &p, Some(&p)), "still the old rendering");
            assert!(chase_settled(Some(&q), &p, Some(&q)), "a repaint settles even after a change");
        }

        /// 2.4. See "A grow holds, then reveals" in `docs/animation-smoothness.md`.
        #[test]
        fn the_hold_is_capped_at_a_blink() {
            for ms in [180u64, 300, 375, 500, 1000] {
                let d = Duration::from_millis(ms);
                assert_eq!(reveal_hold_limit(d), HOLD_CAP, "{ms}ms flight");
                assert!(reveal_hold_limit(d) <= reveal_hold_limit_old(d), "{ms}ms flight");
            }
            assert_eq!(HOLD_CAP, Duration::from_millis(300));
            assert_eq!(REVEAL_CHASE_INTERVAL, Duration::from_millis(8));
            assert_eq!(REVEAL_CHASE_INTERVAL * REVEAL_CHASE_ATTEMPTS as u32, Duration::from_secs(1));
        }

        /// 2.4. Seed 98, 200 runs.
        #[test]
        fn the_hold_cap_holds_for_any_duration() {
            let mut rng = Gen(98);
            for _ in 0..RUNS {
                let d = Duration::from_millis(rng.below(3000));
                let limit = reveal_hold_limit(d);
                assert!(limit <= HOLD_CAP, "seed 98: {d:?} -> {limit:?}");
                assert!(limit <= reveal_hold_limit_old(d), "seed 98: {d:?} -> {limit:?}");
            }
        }

        /// 2.4. A placeholder tile is a reveal in waiting after a hold timeout and after a mid-flight join.
        #[test]
        fn a_placeholder_tile_takes_its_reveal_early() {
            let small = rect(4.0, 32.0, 859.0, 1081.0);
            let grown = rect(4.0, 32.0, 1147.0, 1081.0);
            let mut running = flight(Some(Instant::now()));
            let mut placeholder = tile(wid(1), small, grown);
            placeholder.snapshot = test_snapshot(small.size);
            running.tiles.push(placeholder);
            assert!(running.awaiting.is_empty(), "the deadline cleared the hold");

            let reveal = test_snapshot(grown.size);
            assert_eq!(running.tile_state(wid(1), &reveal), TileState::Reveal { fits: true });
            assert_eq!(
                running.tile_state(wid(1), &test_snapshot(small.size)),
                TileState::Reveal { fits: false },
                "a background picture at the old size is not the reveal"
            );
            let state = running.tile_state(wid(1), &reveal);
            assert_eq!(
                should_swap_mid_flight(state, true, false, true, Some(0.3)),
                SwapDecision::Swap("reveal")
            );
            assert_eq!(
                should_swap_mid_flight(state, true, false, false, Some(0.3)),
                SwapDecision::Swap("reveal"),
                "the chase's framed picture is the truth for a grow, whatever the cached route"
            );
            assert_eq!(
                should_swap_mid_flight(state, false, false, true, Some(0.3)),
                SwapDecision::CacheOnly,
                "an unsettled capture can be the unpainted surface"
            );
            assert_eq!(
                should_swap_mid_flight(state, true, false, true, Some(0.6)),
                SwapDecision::CacheOnly,
                "too late: a cut this close to lift reads as flicker"
            );
            // Once the reveal is worn, the tile is an ordinary resizing tile again.
            running.tiles[0].snapshot = reveal.clone();
            assert_eq!(
                running.tile_state(wid(1), &reveal),
                TileState::Moving { fits: true, resizing: true }
            );
        }

        /// 2.6, inverted. Nothing is owed at the apply point.
        #[test]
        fn an_entrances_frame_goes_out_at_frame_zero_so_its_picture_fits() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let neighbour = (wid(1), rect(4.0, 32.0, 859.0, 1081.0));
            let newcomer = wid(51462);
            let final_frames = vec![neighbour, (newcomer, slot)];

            // The held merge, as `begin_group` runs it: every frame, once.
            let mut running = flight(None);
            running.final_frames = final_frames.clone();
            let (entrance, waiting) = entrance_reservation(newcomer, slot, false);
            running.entrances.push(entrance);
            let awaiting: Vec<(WindowId, CGSize)> = waiting.into_iter().collect();
            let now = Instant::now();
            let requested = running.extend_hold(&awaiting, false, Duration::from_millis(300), now);
            assert_eq!(requested, Some(final_frames.clone()), "the slot went out with the rest");
            assert!(running.frames_applied);
            running.started = Some(now);
            assert_eq!(running.frames_due(running.apply_at), None, "nothing left to send");

            // Nothing placed yet: everything goes at the apply point, once.
            let mut running = flight(Some(Instant::now()));
            running.final_frames = final_frames.clone();
            assert_eq!(running.frames_due(running.apply_at - 0.01), None);
            assert_eq!(running.frames_due(running.apply_at), Some(final_frames.clone()));
            assert!(running.frames_applied);
            assert_eq!(running.frames_due(1.0), None, "sent once");
        }

        /// 2.6, inverted.
        #[test]
        fn an_entrance_needs_the_fit_like_a_grow() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let newcomer = wid(51462);
            let mut running = flight(None);
            let (entrance, waiting) = entrance_reservation(newcomer, slot, false);
            running.entrances.push(entrance);
            running.awaiting.extend(waiting);
            let spawn = test_snapshot(CGSize::new(572.0, 540.0));
            assert_eq!(running.claim(newcomer, &spawn), None, "a spawn-size picture is refused");
            assert_eq!(running.entrances.len(), 1);
            assert_eq!(running.awaiting.len(), 1);
            assert_eq!(
                running.claim(newcomer, &test_snapshot(slot.size)),
                Some(Claimed::Released),
                "a slot-size picture is the entrance"
            );
            assert!(running.entrances.is_empty() && running.awaiting.is_empty());
            let tile = running.tiles.iter().find(|t| t.window == newcomer).expect("tile");
            assert_eq!((tile.from, tile.to), (entrance_from(slot), slot));
        }
    }

    /// Preservation for `.kiro/specs/flight-render-stability/bugfix.md` 3.x: flights outside the bug
    /// condition. Paths that need the actor are asserted on the decisions they make.
    mod render_stability_preservation {
        use super::preservation::{DISPLAY, Gen, RUNS, stacked};
        use super::*;
                use crate::window_snapshot::{
            SnapshotCache, WindowSnapshot, needs_capture, outgrows, should_replace, test_snapshot,
        };

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        fn flight(started: Option<Instant>) -> RunningAnimation {
            RunningAnimation {
                tiles: Vec::new(),
                final_frames: Vec::new(),
                frames_applied: false,
                started,
                duration: Duration::from_millis(300),
                apply_at: APPLY_FRAMES_AT,
                entrances: Vec::new(),
                awaiting: Vec::new(),
                hold_deadline: None,
                destination_refreshed: false,
                refresh_targets: Vec::new(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                _clock: None,
            }
        }

        /// A SkyLight sliver: 40pt of an `size`-wide window, which `is_usable` rejects.
        fn clipped(size: CGSize) -> WindowSnapshot {
            let mut snapshot = test_snapshot(size);
            snapshot.coverage.covered = (40.0, size.height);
            snapshot
        }

        fn any_state(rng: &mut Gen) -> TileState {
            match rng.below(4) {
                0 => TileState::NotTiled,
                1 => TileState::Awaiting,
                2 => TileState::Moving { fits: rng.coin(), resizing: rng.coin() },
                _ => TileState::MovingRefreshTarget { fits: rng.coin(), resizing: rng.coin() },
            }
        }

        /// The apply point before this spec, kept here for P-3.3.
        fn apply_frames_at_old(any_resize: bool) -> f64 {
            if any_resize { APPLY_FRAMES_AT_RESIZE } else { APPLY_FRAMES_AT }
        }

        /// P-3.2.
        #[test]
        fn an_awaited_picture_is_claimed_before_start_and_admitted_after() {
            let mut rng = Gen(92);
            for _ in 0..RUNS {
                let settled = rng.coin();
                let same = rng.coin();
                let same_source = rng.coin();
                let progress = rng.below(1001) as f64 / 1000.0;
                assert_eq!(
                    should_swap_mid_flight(TileState::Awaiting, settled, same, same_source, None),
                    SwapDecision::Claim,
                    "seed 92: settled={settled} same={same}"
                );
                assert_eq!(
                    should_swap_mid_flight(
                        TileState::Awaiting,
                        settled,
                        same,
                        same_source,
                        Some(progress)
                    ),
                    SwapDecision::Admit,
                    "seed 92: settled={settled} same={same} progress={progress}"
                );
            }
            let mut flight = flight(None);
            assert_eq!(flight.progress_if_started(), None, "holding: the claim path");
            flight.started = Some(Instant::now());
            assert!(flight.progress_if_started().is_some(), "moving: the admit path");
        }

        /// Outside the bug condition on the swap path.
        #[test]
        fn an_unfitting_or_identical_picture_is_cached_only() {
            let mut rng = Gen(94);
            for _ in 0..RUNS {
                let settled = rng.coin();
                let resizing = rng.coin();
                let progress = rng.coin().then(|| rng.below(1001) as f64 / 1000.0);
                assert_eq!(
                    should_swap_mid_flight(
                        TileState::NotTiled,
                        settled,
                        rng.coin(),
                        rng.coin(),
                        progress
                    ),
                    SwapDecision::CacheOnly,
                    "seed 94: no tile"
                );
                for state in [
                    TileState::Moving { fits: false, resizing },
                    TileState::MovingRefreshTarget { fits: false, resizing },
                ] {
                    assert_eq!(
                        should_swap_mid_flight(state, settled, rng.coin(), rng.coin(), progress),
                        SwapDecision::CacheOnly,
                        "seed 94: {state:?} does not fit"
                    );
                }
                for state in [
                    TileState::Moving { fits: true, resizing },
                    TileState::MovingRefreshTarget { fits: true, resizing },
                ] {
                    assert_eq!(
                        should_swap_mid_flight(state, settled, true, rng.coin(), progress),
                        SwapDecision::CacheOnly,
                        "seed 94: {state:?} renders like the cached picture"
                    );
                }
            }
        }

        /// P-3.3.
        #[test]
        fn layout_apply_points_are_unchanged() {
            for any_resize in [false, true] {
                assert_eq!(
                    apply_frames_at(FlightKind::Layout, any_resize),
                    apply_frames_at_old(any_resize),
                    "any_resize={any_resize}"
                );
            }
            assert_eq!(apply_frames_at_old(false), 0.75);
            assert_eq!(apply_frames_at_old(true), 0.5);
        }

        /// P-3.4. `finish` drops the flight before it warms `last_animated`.
        #[test]
        fn warming_and_the_desktop_render_are_allowed_once_the_flight_is_dropped() {
            assert!(capture_work_allowed(FlightPhase::Idle, CaptureKind::Warm));
            assert!(capture_work_allowed(FlightPhase::Idle, CaptureKind::Desktop));
            assert!(capture_work_allowed(FlightPhase::Idle, CaptureKind::NeedsCapture));

            let mut running = flight(None);
            assert_eq!(running.phase(), FlightPhase::FrameZero);
            running.awaiting.push((wid(1), CGSize::new(859.0, 1081.0)));
            assert_eq!(running.phase(), FlightPhase::Holding);
            running.started = Some(Instant::now());
            assert_eq!(running.phase(), FlightPhase::Moving);
            running.awaiting.clear();
            assert_eq!(running.phase(), FlightPhase::Moving);
        }

        /// P-3.5.
        #[test]
        fn every_landed_picture_reaches_the_cache_and_never_downgrades() {
            let mut rng = Gen(93);
            let mut refused = 0usize;
            let mut decisions: Vec<SwapDecision> = Vec::new();
            for _ in 0..RUNS {
                let mut cache: SnapshotCache = SnapshotCache::new();
                let size = rng.on_screen().size;
                for _ in 0..rng.below(4) + 1 {
                    let incoming = if rng.coin() { test_snapshot(size) } else { clipped(size) };
                    let before = cache.get(wid(1)).map(|s| s.coverage);
                    let progress = rng.coin().then(|| rng.below(1001) as f64 / 1000.0);
                    let decision = should_swap_mid_flight(
                        any_state(&mut rng),
                        rng.coin(),
                        rng.coin(),
                        rng.coin(),
                        progress,
                    );
                    decisions.push(decision);
                    cache.insert(wid(1), incoming.clone());
                    let held = cache.get(wid(1)).expect("seed 93: every landing leaves an entry");
                    if should_replace(before, incoming.coverage) {
                        assert_eq!(held.coverage, incoming.coverage, "seed 93: taken");
                    } else {
                        refused += 1;
                        assert_eq!(held.coverage, before.unwrap(), "seed 93: kept");
                    }
                    if before.is_some_and(|c| c.is_usable()) {
                        assert!(held.is_usable(), "seed 93: a usable picture was downgraded");
                    }
                }
            }
            assert!(refused > RUNS / 8, "generator sanity: {refused} downgrades refused");
            let seen = |wanted: fn(&SwapDecision) -> bool| decisions.iter().any(wanted);
            assert!(seen(|d| *d == SwapDecision::Claim), "generator sanity: no Claim");
            assert!(seen(|d| *d == SwapDecision::Admit), "generator sanity: no Admit");
            assert!(seen(|d| matches!(d, SwapDecision::Swap(_))), "generator sanity: no Swap");
            assert!(seen(|d| *d == SwapDecision::CacheOnly), "generator sanity: no CacheOnly");
        }

        /// P-3.6. Unfixed code fired at 0.00 and 0.50; the preserved part is the one slot at the midpoint.
        #[test]
        fn one_destination_refresh_fires_at_the_midpoint() {
            let mut running = flight(Some(Instant::now()));
            let fired: Vec<f64> = (0..=100)
                .map(|i| i as f64 / 100.0)
                .filter(|&progress| running.take_refresh(progress))
                .collect();
            let late: Vec<f64> = fired.iter().copied().filter(|p| *p >= 0.5).collect();
            assert_eq!(late, vec![0.5], "refreshes at or after the midpoint: {fired:?}");
            assert!(fired.len() <= 2, "more than the schedule allows: {fired:?}");
            assert_eq!(REFRESH_DESTINATION_AT, 0.5);
            assert!(refresh_targets(Some(wid(1)), Some(wid(2)), &[wid(1), wid(2), wid(3)]).len() <= 2);
            assert!(capture_work_allowed(FlightPhase::Moving, CaptureKind::Refresh));
            // A second sweep on the same flight fires nothing: the slots are spent.
            assert!(!(0..=100).any(|i| running.take_refresh(i as f64 / 100.0)));
        }

        /// P-3.13.
        #[test]
        fn a_grow_holds_bounded_and_flies_a_placeholder_at_the_deadline() {
            let mut rng = Gen(91);
            for _ in 0..RUNS {
                let small = rng.on_screen();
                let to = rect(
                    small.origin.x,
                    32.0,
                    small.size.width + rng.pt(20.0, 800.0),
                    small.size.height,
                );
                let snapshot = test_snapshot(small.size);
                assert!(outgrows(snapshot.coverage.covered, to.size), "seed 91: a grow");
                // `start`: the outgrown picture puts the window in `awaiting` at its destination size.
                let awaiting = vec![(wid(1), to.size)];
                let (holding, chase, _) = frame_zero_work(&awaiting, &[], &[], &[]);
                assert!(holding, "seed 91");
                assert_eq!(chase, awaiting, "seed 91");

                let duration = Duration::from_millis(rng.pt(100.0, 600.0) as u64);
                let limit = reveal_hold_limit(duration);
                let now = Instant::now();
                let mut running = flight(None);
                let mut tile = stacked(wid(1), small, to, Some(0), false);
                tile.snapshot = snapshot.clone();
                running.tiles.push(tile);
                running.awaiting = awaiting.clone();
                running.frames_applied = holding;
                running.hold_deadline = Some(now + limit);
                assert_eq!(running.phase(), FlightPhase::Holding, "seed 91");
                assert_eq!(
                    hold_wait(running.hold_deadline, now),
                    Some(limit.max(Duration::from_millis(10))),
                    "seed 91: waits out the hold"
                );
                assert_eq!(
                    hold_wait(running.hold_deadline, now + limit),
                    None,
                    "seed 91: at the deadline the placeholder flies"
                );
                assert_eq!(running.claim(wid(1), &snapshot), None, "seed 91: too small to claim");
                assert_eq!(running.awaiting, awaiting, "seed 91: the hold stands");
                assert_eq!(
                    running.claim(wid(1), &test_snapshot(to.size)),
                    Some(Claimed::Released),
                    "seed 91: the reveal is claimed"
                );
                assert!(running.tiles[0].snapshot.fits(to.size), "seed 91: drawn from the reveal");
                assert_eq!(running.phase(), FlightPhase::FrameZero, "seed 91: released, not moving");
            }
            assert_eq!(hold_wait(None, Instant::now()), None, "no deadline, no wait");
        }

        /// P-3.14.
        #[test]
        fn a_strip_window_with_no_usable_picture_is_placed_but_not_drawn() {
            let mut rng = Gen(95);
            for _ in 0..RUNS {
                let slot = rng.on_screen();
                let mut cache: SnapshotCache = SnapshotCache::new();
                cache.insert(wid(2), clipped(slot.size));
                assert!(cache.usable(wid(1)).is_none(), "seed 95: never captured");
                assert!(cache.usable(wid(2)).is_none(), "seed 95: a sliver");
                assert!(cache.usable(wid(3)).is_none());
                cache.insert(wid(3), test_snapshot(slot.size));
                assert!(cache.usable(wid(3)).is_some(), "seed 95: the drawn neighbour");

                let (from, to) = surface_travel(slot, CGPoint::new(0.0, 0.0), CGPoint::new(861.0, 0.0), false);
                let mut running = flight(None);
                running.tiles.push(stacked(wid(3), from, to, Some(0), false));
                running.final_frames = vec![(wid(1), to), (wid(2), to), (wid(3), to)];
                let tiled: Vec<WindowId> = running.tiles.iter().map(|t| t.window).collect();
                let real: HashMap<WindowId, CGRect> =
                    running.final_frames.iter().copied().collect();
                let report = handover_report(&running.final_frames, &tiled, &real, DISPLAY);
                // A destination past the edge is a park, which the report excludes (2.3).
                let measured = usize::from(!rini_geometry::is_off_screen(DISPLAY, to));
                assert_eq!(report.total, measured, "seed 95: only the drawn window is measured");
                assert_eq!(report.count_over, 0);

                let size = (slot.size.width, slot.size.height);
                assert!(needs_capture(None, size), "seed 95: warmed after the movement");
                assert!(needs_capture(Some(clipped(slot.size).coverage), size));
                assert!(!needs_capture(Some(test_snapshot(slot.size).coverage), size));
            }
        }

        /// P-3.16.
        #[test]
        fn a_pan_holds_for_nothing() {
            assert_eq!(frame_zero_work(&[], &[], &[], &[]), (false, Vec::new(), Vec::new()));
            let mut rng = Gen(96);
            for _ in 0..RUNS {
                let count = rng.below(4) as u32 + 1;
                let travel = CGPoint::new(rng.pt(-1720.0, 1720.0), 0.0);
                let mut running = flight(None);
                running.apply_at = apply_frames_at(FlightKind::Pan, false);
                for i in 1..=count {
                    let frame = rng.on_screen();
                    let (from, to) = surface_travel(frame, CGPoint::new(0.0, 0.0), travel, false);
                    running.tiles.push(stacked(wid(i), from, to, Some(i as usize), false));
                    running.final_frames.push((wid(i), to));
                }
                assert!(running.awaiting.is_empty() && running.entrances.is_empty(), "seed 96");
                assert_eq!(running.phase(), FlightPhase::FrameZero, "seed 96");
                assert!(!running.frames_applied, "seed 96: nothing applied at frame zero");
                assert_eq!(hold_wait(running.hold_deadline, Instant::now()), None, "seed 96");
                assert_eq!(running.claim(wid(1), &test_snapshot(CGSize::new(859.0, 1081.0))), None);
            }
        }
    }

    /// Preservation for `.kiro/specs/exit-entrance-animation-regressions` bugfix.md 3.x: flights
    /// with no open or close.
    mod preservation {
        use super::*;
                use crate::motion::z_group::{GROUP_STRIDE, MAX_TILE_DEPTH};
        use crate::window_snapshot::{SnapshotCache, test_snapshot};

        pub(super) const DISPLAY: CGRect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width: 1728.0, height: 1117.0 },
        };
        pub(super) const RUNS: usize = 200;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        /// A small deterministic generator, so a failure names its seed and replays.
        pub(super) struct Gen(pub(super) u64);

        impl Gen {
            fn next(&mut self) -> u64 {
                self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                self.0 >> 11
            }

            pub(super) fn below(&mut self, n: u64) -> u64 {
                self.next() % n
            }

            pub(super) fn pt(&mut self, lo: f64, hi: f64) -> f64 {
                lo + self.below((hi - lo) as u64 + 1) as f64
            }

            pub(super) fn coin(&mut self) -> bool {
                self.below(2) == 0
            }

            /// A window frame with a real share of the display showing: a column on the strip.
            pub(super) fn on_screen(&mut self) -> CGRect {
                let w = self.pt(400.0, 1720.0);
                let h = self.pt(600.0, 1081.0);
                let x = self.pt(-w / 4.0, DISPLAY.size.width - w * 0.75);
                rect(x, 32.0, w, h)
            }

            /// The layout's park for a scrolled-off window: 1pt showing in a bottom corner.
            pub(super) fn park(&mut self, size: CGSize) -> CGRect {
                let x = if self.coin() {
                    DISPLAY.size.width - 1.0
                } else {
                    DISPLAY.origin.x - size.width + 1.0
                };
                rect(x, DISPLAY.size.height - 1.0, size.width, size.height)
            }
        }

        pub(super) fn stacked(
            window: WindowId,
            from: CGRect,
            to: CGRect,
            server_order: Option<usize>,
            floating: bool,
        ) -> OverlayTile {
            OverlayTile {
                window,
                from,
                to,
                snapshot: test_snapshot(to.size),
                floating,
                server_order,
                depth: 0,
                companion: false,
                focused: false,
            }
        }

        fn running(final_frames: Vec<(WindowId, CGRect)>) -> RunningAnimation {
            RunningAnimation {
                tiles: Vec::new(),
                final_frames,
                frames_applied: false,
                started: None,
                duration: Duration::from_millis(300),
                apply_at: APPLY_FRAMES_AT,
                entrances: Vec::new(),
                awaiting: Vec::new(),
                hold_deadline: None,
                destination_refreshed: false,
                refresh_targets: Vec::new(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                _clock: None,
            }
        }

        /// P-3.1/3.7.
        #[test]
        fn a_plain_move_starts_from_the_window_servers_frame() {
            let mut rng = Gen(31);
            let mut checked = 0;
            for _ in 0..RUNS {
                let from = rng.on_screen();
                let to = rect(rng.pt(0.0, 900.0), 32.0, from.size.width, from.size.height);
                let real = rect(
                    from.origin.x + rng.pt(20.0, 300.0),
                    32.0,
                    from.size.width,
                    from.size.height,
                );
                if real.same_as(to) || rini_geometry::is_off_screen(DISPLAY, real) {
                    continue;
                }
                checked += 1;
                assert_eq!(
                    resolve_start(Some(real), from, to, DISPLAY, None),
                    real,
                    "seed 31: {from:?} -> {to:?}"
                );
            }
            assert!(checked > RUNS / 2, "generator sanity: {checked} of {RUNS} in scope");
            assert_eq!(apply_frames_at(FlightKind::Layout, false), APPLY_FRAMES_AT);
        }

        /// P-3.7.
        #[test]
        fn a_missing_or_synthetic_start_falls_back_to_the_request() {
            let from = rect(4.0, 32.0, 859.0, 1081.0);
            let to = rect(867.0, 32.0, 859.0, 1081.0);
            assert_eq!(resolve_start(None, from, to, DISPLAY, None), from);
            assert_eq!(resolve_start(Some(to), from, to, DISPLAY, None), from);
        }

        /// P-3.1/3.5.
        #[test]
        fn a_merge_before_frames_were_applied_re_requests_nothing() {
            let mut rng = Gen(35);
            for _ in 0..RUNS {
                let current = rng.on_screen();
                let mut flight = running(vec![(wid(1), current)]);
                let incoming = if rng.coin() { rng.on_screen() } else { current };
                let action = merge_action(Some(current), incoming);
                let expected =
                    if current.same_as(incoming) { Admitted::Redundant } else { Admitted::Retargeted };
                assert_eq!(action, expected);
                let changed = merge_final_frames(&mut flight.final_frames, vec![(wid(1), incoming)]);
                assert_eq!(changed, action == Admitted::Retargeted);
                assert_eq!(flight.final_frames, vec![(wid(1), incoming)], "latest frame wins");
                for in_flight in [false, true] {
                    assert_eq!(reapply_set(false, in_flight, changed, &flight.final_frames), None);
                }
            }
            assert_eq!(merge_action(None, rect(4.0, 32.0, 859.0, 1081.0)), Admitted::Joined);
        }

        /// P-3.2/3.6.
        #[test]
        fn a_held_merge_re_requests_the_flights_frames() {
            let mut rng = Gen(36);
            let mut checked = 0;
            for _ in 0..RUNS {
                let count = rng.below(3) as u32 + 1;
                let frames: Vec<(WindowId, CGRect)> =
                    (1..=count).map(|i| (wid(i), rng.on_screen())).collect();
                let mut flight = running(frames.clone());
                if rng.coin() {
                    flight.awaiting.push((wid(1), CGSize::new(100.0, 100.0)));
                }
                let mut incoming: Vec<(WindowId, CGSize)> = Vec::new();
                for i in 1..=count {
                    if rng.coin() {
                        incoming.push((wid(i), CGSize::new(rng.pt(200.0, 1720.0), 1081.0)));
                    }
                }
                if incoming.is_empty() {
                    continue;
                }
                checked += 1;
                let now = Instant::now();
                let duration = Duration::from_millis(rng.pt(100.0, 600.0) as u64);
                let requested = flight.extend_hold(&incoming, false, duration, now);
                assert_eq!(requested, Some(frames.clone()));
                assert!(flight.frames_applied);
                assert_eq!(flight.hold_deadline, Some(now + reveal_hold_limit(duration)));
                for (window, size) in &incoming {
                    let held: Vec<_> =
                        flight.awaiting.iter().filter(|(w, _)| w == window).collect();
                    assert_eq!(held.len(), 1, "one hold per window");
                    assert_eq!(held[0].1, *size, "latest size wins");
                }
            }
            assert!(checked > RUNS / 2, "generator sanity: {checked} of {RUNS} in scope");
        }

        /// P-3.2.
        #[test]
        fn a_hold_never_stops_a_flight_in_motion() {
            let now = Instant::now();
            let duration = Duration::from_millis(300);
            let mut flight = running(vec![(wid(1), rect(4.0, 32.0, 859.0, 1081.0))]);
            flight.started = Some(now);
            let grow = [(wid(1), CGSize::new(1720.0, 1081.0))];
            assert_eq!(flight.extend_hold(&grow, true, duration, now), None);
            assert!(!flight.frames_applied);
            assert!(flight.awaiting.is_empty());
            assert!(flight.hold_deadline.is_none());
            flight.started = None;
            assert_eq!(flight.extend_hold(&[], false, duration, now), None);
            assert!(!flight.frames_applied);
        }

        /// P-3.3/3.4.
        #[test]
        fn a_pan_translates_a_parked_window_without_remapping_it() {
            let mut rng = Gen(33);
            for _ in 0..RUNS {
                let size = CGSize::new(rng.pt(400.0, 1720.0), 1081.0);
                let frame = rng.park(size);
                let from_offset = CGPoint::new(rng.pt(-4000.0, 4000.0), 0.0);
                let to_offset = CGPoint::new(rng.pt(-4000.0, 4000.0), 0.0);
                let (from, to) = surface_travel(frame, from_offset, to_offset, false);
                assert_eq!(from.origin.x, frame.origin.x - from_offset.x);
                assert_eq!(to.origin.x, frame.origin.x - to_offset.x);
                assert_eq!(from.size, frame.size);
                assert_eq!(to.origin.x - from.origin.x, from_offset.x - to_offset.x);
                assert_eq!(from.origin.y, frame.origin.y, "a pan keeps the park's row");
                let entry = rini_geometry::park_entry_frame(frame, to, DISPLAY);
                if from_offset.x.abs() != 1.0 {
                    assert_ne!(from, entry, "the pan path does not consult the park remap");
                }
            }
        }

        /// P-3.8. Seed 38, 200 runs.
        #[test]
        fn a_restack_bands_the_focused_group_in_front() {
            let mut rng = Gen(38);
            for _ in 0..RUNS {
                let count = rng.below(6) as u32 + 1;
                let mut tiles: Vec<OverlayTile> = (1..=count)
                    .map(|i| {
                        let known = rng.coin();
                        let server_order = known.then(|| rng.below(20) as usize);
                        let floating = rng.coin();
                        stacked(wid(i), rng.on_screen(), rng.on_screen(), server_order, floating)
                    })
                    .collect();
                let focus = wid(rng.below(count as u64) as u32 + 1);
                let focus_floating = tiles.iter().find(|t| t.window == focus).unwrap().floating;
                restack(&mut tiles, Some(focus));
                for tile in &tiles {
                    let in_front = tile.floating == focus_floating;
                    let band = if in_front { 0..GROUP_STRIDE } else { GROUP_STRIDE..2 * GROUP_STRIDE };
                    assert!(
                        band.contains(&tile.depth),
                        "seed 38: {:?} floating={} depth={} focus floating={focus_floating}",
                        tile.window,
                        tile.floating,
                        tile.depth
                    );
                    if tile.window == focus {
                        assert_eq!(tile.depth, 0, "seed 38: the focused window leads");
                    }
                }
            }
        }

        #[test]
        fn a_focus_off_the_pass_puts_the_strip_in_front() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut tiles = vec![
                stacked(wid(1), slot, slot, Some(4), false),
                stacked(wid(2), slot, slot, Some(0), true),
                stacked(wid(3), slot, slot, None, false),
            ];
            restack(&mut tiles, Some(wid(99)));
            let depths: Vec<usize> = tiles.iter().map(|t| t.depth).collect();
            assert_eq!(depths, vec![5, GROUP_STRIDE + 1, GROUP_STRIDE - 1]);
            restack(&mut tiles, None);
            let no_focus: Vec<usize> = tiles.iter().map(|t| t.depth).collect();
            assert_eq!(no_focus, depths);
        }

        /// The server's order is untrusted input.
        #[test]
        fn an_absurd_server_order_stays_inside_its_band() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut tiles = vec![
                stacked(wid(1), slot, slot, Some(usize::MAX), false),
                stacked(wid(2), slot, slot, Some(usize::MAX), true),
            ];
            restack(&mut tiles, None);
            assert_eq!(tiles[0].depth, GROUP_STRIDE - 1);
            assert_eq!(tiles[1].depth, MAX_TILE_DEPTH);
        }

        /// P-3.9.
        #[test]
        fn a_parked_or_scrolled_off_close_is_not_worth_animating() {
            let mut rng = Gen(39);
            for _ in 0..RUNS {
                let size = CGSize::new(rng.pt(400.0, 1720.0), 1081.0);
                let park = rng.park(size);
                assert!(!worth_animating(park, entrance_from(park), DISPLAY), "park {park:?}");
                let off = rect(DISPLAY.size.width + rng.pt(1.0, 9000.0), 32.0, size.width, size.height);
                assert!(!worth_animating(off, entrance_from(off), DISPLAY), "off strip {off:?}");
                let on = rng.on_screen();
                assert!(worth_animating(on, entrance_from(on), DISPLAY), "on screen {on:?}");
            }
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            assert!(!worth_animating(rect(4.0, 32.0, 0.0, 0.0), slot, DISPLAY), "zero area");
            // Entering from a park is still worth it: the path crosses the display.
            assert!(worth_animating(rect(1727.0, 1116.0, 859.0, 1081.0), slot, DISPLAY));
        }

        /// P-3.10.
        #[test]
        fn forgetting_a_window_empties_the_cache_even_while_a_clone_is_held() {
            let mut cache: SnapshotCache = SnapshotCache::new();
            let size = CGSize::new(859.0, 1081.0);
            cache.insert(wid(1), test_snapshot(size));
            let held = cache.usable(wid(1)).cloned().expect("inserted");
            cache.forget(wid(1));
            assert!(cache.usable(wid(1)).is_none());
            assert!(held.is_usable());
            assert!(held.fits(size));
        }

        /// P-3.11.
        #[test]
        fn a_fresh_flight_with_an_entrance_applies_and_chases_at_frame_zero() {
            let mut rng = Gen(311);
            for _ in 0..RUNS {
                let to = rng.on_screen();
                let (entrance, waiting) = entrance_reservation(wid(3), to, false);
                assert_eq!(entrance.window, wid(3));
                assert_eq!(entrance.to, to);
                let awaiting: Vec<(WindowId, CGSize)> = waiting.into_iter().collect();
                let (apply_now, chase, _) = frame_zero_work(&awaiting, &[], &[], &[]);
                assert!(apply_now);
                assert!(chase.contains(&(wid(3), to.size)), "the entrance is chased: {chase:?}");

                let awaiting = vec![(wid(1), rng.on_screen().size)];
                let (apply_now, chase, _) = frame_zero_work(&awaiting, &[], &[], &[]);
                assert!(apply_now);
                assert_eq!(chase, awaiting);
            }
            assert_eq!(frame_zero_work(&[], &[], &[], &[]), (false, Vec::new(), Vec::new()));
        }
    }

    /// Change 3 of `.kiro/specs/exit-entrance-animation-regressions`: depth is banded once per flight
    /// from its latest focus. See "Mid-flight passes" in `docs/animation-smoothness.md`.
    mod flight_restack {
        use super::preservation::{Gen, RUNS, stacked};
        use super::*;
        use crate::motion::z_group::{GROUP_STRIDE, MAX_TILE_DEPTH};

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        pub(super) fn flight() -> RunningAnimation {
            RunningAnimation {
                tiles: Vec::new(),
                final_frames: Vec::new(),
                frames_applied: false,
                started: None,
                duration: Duration::from_millis(300),
                apply_at: APPLY_FRAMES_AT,
                entrances: Vec::new(),
                awaiting: Vec::new(),
                hold_deadline: None,
                destination_refreshed: false,
                refresh_targets: Vec::new(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                _clock: None,
            }
        }

        fn depth(flight: &RunningAnimation, window: WindowId) -> usize {
            flight.tiles.iter().find(|t| t.window == window).unwrap().depth
        }

        /// The 1.1 scenario.
        #[test]
        fn a_later_focus_rebands_every_tile_in_the_flight() {
            let (s1, s2, f) = (wid(1), wid(2), wid(3));
            let slot_a = rect(4.0, 32.0, 859.0, 1081.0);
            let slot_b = rect(867.0, 32.0, 859.0, 1081.0);
            let slot_c = rect(1730.0, 32.0, 859.0, 1081.0);
            let zoom = rect(224.0, 95.0, 1280.0, 960.0);
            let mut flight = flight();

            flight.merge_pass(
                vec![
                    stacked(s1, slot_a, slot_a, Some(0), false),
                    stacked(s2, slot_c, slot_b, Some(2), false),
                    stacked(f, zoom, zoom, Some(1), true),
                ],
                None,
            );
            assert_eq!(depth(&flight, s1), 1, "pass 1: the strip leads, server order within");
            assert_eq!(depth(&flight, s2), 3);
            assert_eq!(depth(&flight, f), GROUP_STRIDE + 2, "the floating window behind the strip");

            flight.merge_pass(
                vec![
                    stacked(s1, slot_a, slot_a, Some(0), false),
                    stacked(s2, slot_c, slot_c, Some(2), false),
                    stacked(f, zoom, zoom, Some(1), true),
                ],
                Some(f),
            );
            assert_eq!(flight.focus, Some(f), "latest focus is recorded");
            assert_eq!(depth(&flight, f), 0, "the focused floating window leads");
            assert_eq!(depth(&flight, s1), GROUP_STRIDE + 1, "redundant tile: rebanded anyway");
            assert_eq!(depth(&flight, s2), GROUP_STRIDE + 3, "retargeted tile: rebanded");
        }

        /// A pass that names no focus leaves the flight's focus alone, and the banding with it.
        #[test]
        fn a_pass_without_a_focus_keeps_the_flights_focus() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let zoom = rect(224.0, 95.0, 1280.0, 960.0);
            let mut flight = flight();
            flight.merge_pass(
                vec![
                    stacked(wid(1), slot, slot, Some(1), false),
                    stacked(wid(2), zoom, zoom, Some(0), true),
                ],
                Some(wid(2)),
            );
            flight.merge_pass(vec![stacked(wid(1), slot, slot, Some(1), false)], None);
            assert_eq!(flight.focus, Some(wid(2)));
            assert_eq!(depth(&flight, wid(2)), 0);
            assert_eq!(depth(&flight, wid(1)), GROUP_STRIDE + 2, "the floating focus still leads");
        }

        /// A redundant tile is untouched by `merge`, order included.
        #[test]
        fn a_new_server_order_on_a_later_pass_moves_the_tile_within_its_band() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let slot_b = rect(867.0, 32.0, 859.0, 1081.0);
            let zoom = rect(224.0, 95.0, 1280.0, 960.0);
            let mut flight = flight();
            flight.merge_pass(
                vec![
                    stacked(wid(1), slot, slot, Some(0), false),
                    stacked(wid(2), zoom, zoom, Some(1), true),
                    stacked(wid(3), slot_b, slot_b, Some(2), false),
                ],
                None,
            );
            flight.merge_pass(
                vec![
                    stacked(wid(1), slot, slot_b, Some(4), false),
                    stacked(wid(2), zoom, entrance_from(zoom), Some(0), true),
                    stacked(wid(3), slot_b, slot_b, Some(5), false),
                ],
                Some(wid(2)),
            );
            assert_eq!(depth(&flight, wid(2)), 0, "the floating focus leads");
            assert_eq!(depth(&flight, wid(3)), GROUP_STRIDE + 3, "redundant: old order kept");
            assert_eq!(depth(&flight, wid(1)), GROUP_STRIDE + 5, "retargeted: the new order");
        }

        #[test]
        fn an_entrance_tile_leads_its_own_band() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let zoom = rect(224.0, 95.0, 1280.0, 960.0);
            let mut flight = flight();
            flight.merge_pass(
                vec![
                    stacked(wid(1), slot, slot, Some(3), false),
                    stacked(wid(2), zoom, zoom, Some(7), true),
                ],
                Some(wid(2)),
            );
            let entering = stacked(wid(5), entrance_from(slot), slot, Some(0), false);
            flight.merge_pass(vec![entering], None);
            assert_eq!(depth(&flight, wid(2)), 0, "the floating focus leads");
            assert_eq!(depth(&flight, wid(5)), GROUP_STRIDE + 1, "the entrance leads the strip");
            assert_eq!(depth(&flight, wid(1)), GROUP_STRIDE + 4);
        }

        /// Companions ride their window's depth and are not restacked on their own.
        #[test]
        fn a_companion_keeps_the_depth_it_was_given() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut companion = stacked(wid(8), slot, slot, None, false);
            companion.companion = true;
            companion.depth = 3;
            let mut tiles = vec![stacked(wid(1), slot, slot, Some(2), false), companion];
            restack(&mut tiles, None);
            assert_eq!(tiles[0].depth, 3);
            assert_eq!(tiles[1].depth, 3, "not sent to the back for its `None` order");
        }

        /// Seed 33, 200 runs.
        #[test]
        fn the_unfocused_group_is_never_in_front_of_the_focused_group() {
            let mut rng = Gen(33);
            for _ in 0..RUNS {
                let count = rng.below(6) as u32 + 1;
                let mut flight = flight();
                let passes = rng.below(3) + 1;
                let mut focus = None;
                for _ in 0..passes {
                    let mut tiles: Vec<OverlayTile> = Vec::new();
                    for i in 1..=count {
                        if rng.below(4) == 0 {
                            continue;
                        }
                        let known = rng.coin();
                        let server_order = known.then(|| rng.below(20) as usize);
                        let floating = i % 2 == 0;
                        let to = rng.on_screen();
                        tiles.push(stacked(wid(i), rng.on_screen(), to, server_order, floating));
                    }
                    let pass_focus = match rng.below(4) {
                        0 => None,
                        1 => Some(wid(99)),
                        _ => Some(wid(rng.below(count as u64) as u32 + 1)),
                    };
                    if pass_focus.is_some() {
                        focus = pass_focus;
                    }
                    flight.merge_pass(tiles, pass_focus);
                }
                assert_eq!(flight.focus, focus, "seed 33: latest named focus wins");
                let focus_floating = focus
                    .and_then(|f| flight.tiles.iter().find(|t| t.window == f))
                    .is_some_and(|t| t.floating);
                for tile in &flight.tiles {
                    assert!(tile.depth <= MAX_TILE_DEPTH, "seed 33: in front of the backdrop");
                    if Some(tile.window) == focus {
                        assert_eq!(tile.depth, 0, "seed 33: the focused tile leads");
                    }
                }
                for front in flight.tiles.iter().filter(|t| t.floating == focus_floating) {
                    for back in flight.tiles.iter().filter(|t| t.floating != focus_floating) {
                        assert!(
                            front.depth < back.depth,
                            "seed 33: {:?} (floating={}) behind {:?} (floating={}) with focus {focus:?}",
                            front.window,
                            front.floating,
                            back.window,
                            back.floating
                        );
                    }
                }
            }
        }
    }




    /// Change 1 of `.kiro/specs/exit-entrance-animation-regressions`: an entrance is a hold.
    mod entrance_hold {
        use super::preservation::{Gen, RUNS, stacked};
        use super::*;
        use crate::window_snapshot::test_snapshot;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        fn flight(started: Option<Instant>) -> RunningAnimation {
            RunningAnimation {
                tiles: Vec::new(),
                final_frames: Vec::new(),
                frames_applied: false,
                started,
                duration: Duration::from_millis(300),
                apply_at: APPLY_FRAMES_AT,
                entrances: Vec::new(),
                awaiting: Vec::new(),
                hold_deadline: None,
                destination_refreshed: false,
                refresh_targets: Vec::new(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                _clock: None,
            }
        }

        /// A flight holding for one entrance at `to`, composed the way `start` does it.
        fn holding_for(window: WindowId, to: CGRect, floating: bool) -> RunningAnimation {
            let mut flight = flight(None);
            let (entrance, waiting) = entrance_reservation(window, to, floating);
            flight.entrances.push(entrance);
            flight.awaiting.extend(waiting);
            flight.frames_applied = true;
            flight
        }

        #[test]
        fn reapply_set_truth_table() {
            let frames = vec![(wid(1), rect(4.0, 32.0, 859.0, 1081.0))];
            for frames_applied in [false, true] {
                for in_flight in [false, true] {
                    for changed in [false, true] {
                        let expected = (frames_applied && !in_flight && changed)
                            .then(|| frames.clone());
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
                let (entrance, waiting) = entrance_reservation(wid(2), to, floating);
                assert_eq!(waiting, Some((wid(2), to.size)), "seed 61: {to:?}");
                assert_eq!(entrance.to, to);
                assert_eq!(entrance.floating, floating);
            }
        }

        #[test]
        fn a_claimed_entrance_joins_the_frame_zero_composition() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let neighbour = rect(4.0, 32.0, 859.0, 1081.0);
            let mut flight = holding_for(wid(2), slot, false);
            flight.tiles.push(stacked(wid(1), neighbour, neighbour, Some(1), false));
            flight.awaiting.push((wid(1), neighbour.size));
            flight.focus = Some(wid(2));

            let claimed = flight.claim(wid(2), &test_snapshot(slot.size));

            assert_eq!(claimed, Some(Claimed::Held), "the grow's hold remains");
            assert!(flight.started.is_none(), "still at frame zero");
            assert_eq!(flight.awaiting, vec![(wid(1), neighbour.size)]);
            assert!(flight.entrances.is_empty(), "the reservation is consumed");
            let tile = flight.tiles.iter().find(|t| t.window == wid(2)).expect("tile composed");
            assert_eq!(tile.from, entrance_from(slot), "zero width at its left edge");
            assert_eq!(tile.to, slot);
            assert!(tile.focused);
            assert_eq!(tile.depth, 0, "the entrance leads: raised on open");
            let other = flight.tiles.iter().find(|t| t.window == wid(1)).unwrap();
            assert_eq!(other.depth, 2, "its neighbour keeps the server's order, within the band");
        }

        #[test]
        fn the_last_claim_releases_the_flight_and_a_repeat_is_not_a_hold() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let mut flight = holding_for(wid(2), slot, true);
            assert_eq!(flight.claim(wid(2), &test_snapshot(slot.size)), Some(Claimed::Released));
            assert!(flight.awaiting.is_empty());
            assert_eq!(flight.tiles.len(), 1);
            assert_eq!(
                flight.claim(wid(2), &test_snapshot(slot.size)),
                Some(Claimed::Refreshed),
                "a repeat refreshes the composed tile's picture and releases nothing"
            );
            assert_eq!(flight.tiles.len(), 1, "no second tile");
        }

        #[test]
        fn a_small_picture_is_not_claimed_nor_is_a_moving_flight() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let mut flight = holding_for(wid(2), slot, false);
            assert_eq!(flight.claim(wid(2), &test_snapshot(CGSize::new(200.0, 200.0))), None);
            assert_eq!(flight.awaiting.len(), 1, "the hold goes on");
            assert_eq!(flight.entrances.len(), 1);
            assert!(flight.tiles.is_empty(), "nothing composed from the small picture");

            let mut flight = holding_for(wid(2), slot, false);
            flight.started = Some(Instant::now());
            assert_eq!(flight.claim(wid(2), &test_snapshot(slot.size)), None);
            assert!(flight.tiles.is_empty());
        }

        /// A grow's reveal picture still lands on its tile: the pre-existing hold path.
        #[test]
        fn a_grows_reveal_picture_replaces_its_tiles_snapshot() {
            let small = rect(4.0, 32.0, 400.0, 1081.0);
            let big = rect(4.0, 32.0, 1720.0, 1081.0);
            let mut flight = flight(None);
            let mut tile = stacked(wid(1), small, big, Some(0), false);
            tile.snapshot = test_snapshot(small.size);
            flight.tiles.push(tile);
            flight.awaiting.push((wid(1), big.size));
            assert_eq!(flight.claim(wid(1), &test_snapshot(big.size)), Some(Claimed::Released));
            assert!(flight.tiles[0].snapshot.fits(big.size));
            assert_eq!(flight.tiles.len(), 1);
        }

        /// Before the flight starts, `admit` does nothing and leaves the reservation to `claim`.
        #[test]
        fn a_late_entrance_travels_for_the_remaining_flight() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let mut flight = holding_for(wid(2), slot, false);
            assert!(flight.admit(wid(2), &test_snapshot(slot.size)).is_none(), "still holding");
            assert_eq!(flight.entrances.len(), 1);

            // The deadline passed: `start_moving` flew with the placeholder and cleared the hold.
            flight.awaiting.clear();
            flight.started = Some(Instant::now() - flight.duration.mul_f64(0.6));
            let remaining = flight.remaining();
            let (tile, travel) = flight.admit(wid(2), &test_snapshot(slot.size)).expect("admitted");
            assert_eq!(tile.from, entrance_from(slot));
            assert_eq!(tile.to, slot);
            assert!(travel <= remaining, "{travel:?} outlives {remaining:?}");
            assert!(travel < flight.duration.mul_f64(0.5), "not the full duration: {travel:?}");
            assert!(flight.entrances.is_empty());
            assert!(flight.admit(wid(2), &test_snapshot(slot.size)).is_none(), "taken once");
        }

        #[test]
        fn a_late_joiner_never_outlives_the_flight() {
            let mut rng = Gen(62);
            for _ in 0..RUNS {
                let duration = Duration::from_millis(rng.pt(100.0, 600.0) as u64);
                let progress = rng.below(1000) as f64 / 1000.0;
                let travel = late_join_duration(duration, progress);
                let remaining = duration.mul_f64(1.0 - progress);
                assert!(
                    travel <= remaining + Duration::from_nanos(1),
                    "seed 62: {travel:?} > {remaining:?} at {progress}"
                );
                let mut flight = flight(Some(Instant::now() - duration.mul_f64(progress)));
                flight.duration = duration;
                flight.entrances.push(PendingEntrance {
                    window: wid(2),
                    to: rng.on_screen(),
                    floating: false,
                });
                let to = flight.entrances[0].to;
                let (_, admitted) = flight.admit(wid(2), &test_snapshot(to.size)).unwrap();
                assert!(admitted <= flight.duration, "seed 62: {admitted:?}");
            }
            assert_eq!(late_join_duration(Duration::from_millis(300), 1.0), Duration::ZERO);
            assert_eq!(late_join_duration(Duration::from_millis(300), 1.5), Duration::ZERO);
        }

        #[test]
        fn a_coalescing_merge_re_requests_exactly_the_merged_frames() {
            let mut rng = Gen(63);
            let mut re_requested = 0;
            for _ in 0..RUNS {
                let count = rng.below(4) as u32 + 1;
                let mut flight = flight(None);
                flight.frames_applied = true;
                flight.final_frames = (1..=count).map(|i| (wid(i), rng.on_screen())).collect();
                let passes = rng.below(3) + 1;
                let mut expected: Vec<(WindowId, CGRect)> = flight.final_frames.clone();
                for _ in 0..passes {
                    let mut incoming: Vec<(WindowId, CGRect)> = Vec::new();
                    for i in 1..=count + 1 {
                        match rng.below(3) {
                            0 => continue,
                            1 => incoming.push((wid(i), rng.on_screen())),
                            _ => {
                                if let Some(&(_, current)) = expected.iter().find(|(w, _)| *w == wid(i)) {
                                    incoming.push((wid(i), current));
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
                    let changed = merge_final_frames(&mut flight.final_frames, incoming);
                    assert_eq!(changed, expected_changed, "seed 63");
                    let reapply =
                        reapply_set(flight.frames_applied, flight.started.is_some(), changed, &flight.final_frames);
                    if changed {
                        re_requested += 1;
                        assert_eq!(reapply, Some(expected.clone()), "seed 63: merged set, latest wins");
                    } else {
                        assert_eq!(reapply, None, "seed 63: nothing changed, nothing re-requested");
                    }
                }
            }
            assert!(re_requested > RUNS / 2, "generator sanity: {re_requested} re-requests");
        }
    }


    /// Fix checking for Change 6 (bugfix.md 1.8, 2.8, 2.10): a pass flies only when something
    /// drawable moves or a flight is running.
    mod still_passes {
        use super::preservation::{Gen, RUNS, stacked};
        use super::*;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        fn moving_drawable(tiles: &[OverlayTile]) -> bool {
            tiles.iter().any(|tile| is_moving(tile.from, tile.to))
        }

        /// 1.8: a window with no picture is an entrance, not a tile; nothing drawable moves.
        #[test]
        fn a_floating_open_over_a_still_strip_does_not_fly() {
            let s1 = rect(0.0, 32.0, 860.0, 1081.0);
            let s2 = rect(867.0, 32.0, 859.0, 1081.0);
            let tiles = vec![
                stacked(wid(1), s1, s1, Some(1), false),
                stacked(wid(2), s2, s2, Some(2), false),
            ];
            let zoom = rect(224.0, 95.0, 1280.0, 960.0);
            let (entrance, waiting) = entrance_reservation(wid(3), zoom, true);
            assert_eq!(entrance.window, wid(3));
            assert!(waiting.is_some(), "the entrance is reserved but never drawn here");
            assert!(!worth_flying(moving_drawable(&tiles), false));
        }

        #[test]
        fn a_strip_open_that_moves_a_neighbour_flies() {
            let before = rect(0.0, 32.0, 1720.0, 1081.0);
            let after = rect(0.0, 32.0, 860.0, 1081.0);
            let tiles = vec![stacked(wid(1), before, after, Some(1), false)];
            let (_, waiting) = entrance_reservation(wid(2), rect(867.0, 32.0, 859.0, 1081.0), false);
            assert!(waiting.is_some());
            assert!(worth_flying(moving_drawable(&tiles), false));
        }

        #[test]
        fn a_pan_flies() {
            let frame = rect(867.0, 32.0, 859.0, 1081.0);
            let (from, to) =
                surface_travel(frame, CGPoint::new(0.0, 0.0), CGPoint::new(867.0, 0.0), false);
            let tiles = vec![stacked(wid(1), from, to, Some(1), false)];
            assert!(worth_flying(moving_drawable(&tiles), false));
        }

        #[test]
        fn a_still_pass_joining_a_running_flight_flies() {
            let s1 = rect(0.0, 32.0, 860.0, 1081.0);
            let tiles = vec![stacked(wid(1), s1, s1, Some(1), false)];
            assert!(worth_flying(moving_drawable(&tiles), true));
            assert!(worth_flying(false, true), "even with nothing drawable at all");
        }

        /// Property (P-3.3).
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
                for i in 0..count {
                    let from = rng.on_screen();
                    let to = if all_still || rng.coin() {
                        from
                    } else {
                        rect(from.origin.x + rng.pt(1.0, 400.0), 32.0, from.size.width, from.size.height)
                    };
                    any_moving |= is_moving(from, to);
                    tiles.push(stacked(wid(i as u32 + 1), from, to, Some(i), rng.coin()));
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
    }

    #[test]
    fn overlay_space_subtracts_the_overlay_origin() {
        // A display inset by a 32pt menu bar: a window at y = 32 lands at y = 0 in the overlay.
        let overlay = rect(0.0, 32.0, 1728.0, 1085.0);
        let window = rect(865.0, 32.0, 859.0, 1081.0);
        assert_eq!(to_overlay_space(window, overlay), rect(865.0, 0.0, 859.0, 1081.0));
    }


    #[test]
    fn a_bounce_extends_the_clock_to_cover_its_return() {
        let bounce = Duration::from_millis(350);
        assert_eq!(clock_for_bounce(None, Duration::from_millis(100), bounce), bounce);
        assert_eq!(
            clock_for_bounce(None, Duration::from_millis(900), bounce),
            Duration::from_millis(900)
        );
        let started = Instant::now() - Duration::from_millis(300);
        let extended = clock_for_bounce(Some(started), Duration::from_millis(350), bounce);
        assert!(extended >= Duration::from_millis(650), "{extended:?}");
        let long = clock_for_bounce(Some(started), Duration::from_secs(5), bounce);
        assert_eq!(long, Duration::from_secs(5));
    }

    #[test]
    fn overlay_space_keeps_negative_offsets_negative() {
        // Off-strip windows at negative x stay to the left of the overlay, not clamped into it.
        let overlay = rect(0.0, 32.0, 1728.0, 1085.0);
        assert_eq!(
            to_overlay_space(rect(-1680.0, 32.0, 1720.0, 1081.0), overlay),
            rect(-1680.0, 0.0, 1720.0, 1081.0)
        );
    }

    #[test]
    fn overlay_space_handles_a_second_display_at_an_offset() {
        // A display to the right: windows at large positive x are drawn relative to its own overlay.
        let overlay = rect(1728.0, 32.0, 1728.0, 1085.0);
        assert_eq!(
            to_overlay_space(rect(1728.0, 32.0, 859.0, 1081.0), overlay),
            rect(0.0, 0.0, 859.0, 1081.0)
        );
    }

    #[test]
    fn the_overlay_lifts_when_the_clock_is_done_and_the_tiles_are_presented_there() {
        assert!(!lift_now(false, true, true, false), "the clock has not run out");
        assert!(!lift_now(false, true, true, true), "overdue is meaningless before the clock is done");
        assert!(!lift_now(true, false, true, false), "clock done, render server a frame behind: wait");
        assert!(!lift_now(true, true, false, false), "clock done, a real window still travelling: wait");
        assert!(lift_now(true, true, true, false));
        assert!(lift_now(true, false, false, true), "the grace ran out: lift anyway");
        assert!(LIFT_GRACE < Duration::from_millis(500), "a stall is a hold, not a hang");
    }

    #[test]
    fn progress_is_complete_for_a_zero_length_animation() {
        // Zero durations resolve at once, and the division is guarded.
        let running = RunningAnimation {
            tiles: Vec::new(),
            final_frames: Vec::new(),
            frames_applied: false,
            started: Some(Instant::now()),
            duration: Duration::ZERO,
            apply_at: APPLY_FRAMES_AT,
            entrances: Vec::new(),
            awaiting: Vec::new(),
            hold_deadline: None,
            destination_refreshed: false,
            refresh_targets: Vec::new(),
            harvested: HashSet::new(),
            focus: None,
            plan: plan::FlightPlan::empty(),
            _clock: None,
        };
        assert_eq!(running.progress(), 1.0);
        assert!(running.is_done());
    }

    #[test]
    fn progress_starts_near_zero_and_is_clamped_to_one() {
        let running = RunningAnimation {
            tiles: Vec::new(),
            final_frames: Vec::new(),
            frames_applied: false,
            started: Some(Instant::now()),
            duration: Duration::from_millis(180),
            apply_at: APPLY_FRAMES_AT,
            entrances: Vec::new(),
            awaiting: Vec::new(),
            hold_deadline: None,
            destination_refreshed: false,
            refresh_targets: Vec::new(),
            harvested: HashSet::new(),
            focus: None,
            plan: plan::FlightPlan::empty(),
            _clock: None,
        };
        assert!(running.progress() < 0.2, "just started");

        let finished = RunningAnimation {
            tiles: Vec::new(),
            final_frames: Vec::new(),
            frames_applied: false,
            started: Some(Instant::now() - Duration::from_secs(5)),
            duration: Duration::from_millis(180),
            apply_at: APPLY_FRAMES_AT,
            entrances: Vec::new(),
            awaiting: Vec::new(),
            hold_deadline: None,
            destination_refreshed: false,
            refresh_targets: Vec::new(),
            harvested: HashSet::new(),
            focus: None,
            plan: plan::FlightPlan::empty(),
            _clock: None,
        };
        // Clamped, or the easing overshoots when a frame arrives late.
        assert_eq!(finished.progress(), 1.0);
        assert!(finished.is_done());
    }

    /// A pass merging into a flight in progress (`merge_plans`). The 3:27:20 tear is the case it
    /// exists for. See "Mid-flight passes" in `docs/animation-smoothness.md`.
    mod rigid_strip {
        use super::preservation::{DISPLAY, Gen, RUNS};
        use super::rigid_groups::random_requests;
        use super::*;
        use crate::engine::plan::*;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

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

        fn column(i: f64) -> CGRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        fn flight_of(requests: &[(WindowId, CGRect, CGRect, bool)]) -> FlightPlan {
            FlightPlan::from(reflow_plan(requests, DISPLAY))
        }

        /// A window's destination in overlay space, whatever it rides.
        fn dest(plan: &FlightPlan, window: WindowId) -> Option<CGRect> {
            Some(match plan.member(window)? {
                Member::Rigid { key, rel } => overlay_of(rel, plan.position_of(key)),
                Member::Changing { to, .. } | Member::Entrance { to, .. } => to,
                Member::Floating { to, .. } => overlay_of(to, plan.position_of(GroupKey::Floating)),
            })
        }

        fn key_of(plan: &FlightPlan, window: WindowId) -> Option<GroupKey> {
            match plan.member(window)? {
                Member::Rigid { key, .. } => Some(key),
                Member::Changing { .. } | Member::Entrance { .. } => Some(GroupKey::Loose),
                Member::Floating { .. } => Some(GroupKey::Floating),
            }
        }

        /// Model positions as the presented ones: a flight that has not started drawing.
        fn at_model(plan: &FlightPlan) -> HashMap<GroupKey, CGPoint> {
            plan.positions.clone()
        }

        /// Halfway between install (`p - travel`) and destination, per container.
        fn midway(plan: &FlightPlan) -> HashMap<GroupKey, CGPoint> {
            plan.positions
                .iter()
                .map(|(key, p)| {
                    let travel = match key {
                        GroupKey::Floating => plan.floating_travel,
                        GroupKey::Loose => CGPoint::new(0.0, 0.0),
                        key => plan.groups.iter().find(|g| g.key == *key).map(|g| g.travel).unwrap_or(CGPoint::new(0.0, 0.0)),
                    };
                    (*key, CGPoint::new(p.x - travel.x / 2.0, p.y - travel.y / 2.0))
                })
                .collect()
        }

        #[test]
        fn a_later_pass_retargets_a_reserved_entrance() {
            let slot = rect(EXTERNAL.origin.x + 867.0, EXTERNAL.origin.y + 32.0, 859.0, 1081.0);
            let pan = CGPoint::new(-574.0, 0.0);
            let (newcomer, _) =
                entrance_reservation(wid(51462), to_overlay_space(slot, EXTERNAL), false);
            let (untouched, _) = entrance_reservation(
                wid(51463),
                to_overlay_space(rect(2000.0, 32.0, 400.0, 1081.0), EXTERNAL),
                false,
            );
            let mut entrances = vec![newcomer, untouched.clone()];
            let frames = vec![(wid(1), rect(4.0, 32.0, 859.0, 1081.0)), (wid(51462), shifted(slot, pan))];

            assert_eq!(retarget_entrances(&mut entrances, &frames, EXTERNAL), 1);
            assert_eq!(entrances[0].to, to_overlay_space(shifted(slot, pan), EXTERNAL));
            assert_eq!(entrances[1].to, untouched.to, "no frame for it: left alone");
            assert_eq!(retarget_entrances(&mut entrances, &frames, EXTERNAL), 0, "already there");
        }

        #[test]
        fn a_pan_merging_into_an_open_shifts_every_group_and_reparents_nothing() {
            let (a, b, c) = (column(0.0), column(1.0), column(2.0));
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let mut current = flight_of(&[
                (wid(1), a, a, false),
                (wid(2), b, shifted(b, CGPoint::new(859.0, 0.0)), false),
                (wid(3), c, shifted(c, CGPoint::new(859.0, 0.0)), false),
            ]);
            current.entrances.push((wid(9), entrance_from(slot), slot));
            let before = current.clone();

            let d = CGPoint::new(-574.0, 0.0);
            let pan = plan::surface_plan(
                &[
                    SurfaceWindow { window: wid(1), server_id: WindowServerId::new(1), frame: a, pinned: false, floating: false },
                    SurfaceWindow { window: wid(2), server_id: WindowServerId::new(2), frame: shifted(b, CGPoint::new(859.0, 0.0)), pinned: false, floating: false },
                ],
                CGPoint::new(-574.0, 0.0),
                CGPoint::new(0.0, 0.0),
            );
            let (merged, delta) = merge_plans(&current, &pan, Some(d), &at_model(&current), None, DISPLAY);

            for group in before.groups.iter().filter(|g| !g.members.is_empty()) {
                let p = merged.position_of(group.key);
                assert_eq!(p, CGPoint::new(before.position_of(group.key).x + d.x, before.position_of(group.key).y + d.y));
                assert!(delta.retargeted_groups.contains(&(group.key, p)), "{:?} retargeted", group.key);
                let after = merged.groups.iter().find(|g| g.key == group.key).unwrap();
                assert_eq!(after.members, group.members, "membership and rel untouched");
            }
            assert!(delta.reparented.is_empty());
            assert!(delta.new_groups.is_empty(), "the pan names known windows only");
            assert_eq!(merged.entrances[0].2, shifted(slot, d));
            assert_eq!(delta.retargeted_tiles, vec![(wid(9), shifted(slot, d))]);
            assert_eq!(dest(&merged, wid(3)), Some(shifted(c, CGPoint::new(859.0 + d.x, 0.0))), "a member the pan did not compose rides its group");
        }

        #[test]
        fn a_redundant_pass_is_an_empty_delta() {
            let (a, b) = (column(0.0), column(1.0));
            let requests = [(wid(1), a, a, false), (wid(2), b, shifted(b, CGPoint::new(-300.0, 0.0)), false)];
            let current = flight_of(&requests);
            let again = reflow_plan(&requests, DISPLAY);
            let (merged, delta) = merge_plans(&current, &again, None, &midway(&current), None, DISPLAY);
            assert!(delta.is_empty(), "{delta:?}");
            assert_eq!(merged, current);
        }

        #[test]
        fn a_pass_moving_one_member_elsewhere_reparents_it_and_keeps_the_other() {
            let (a, b) = (column(0.0), column(1.0));
            let v = CGPoint::new(-300.0, 0.0);
            let current = flight_of(&[(wid(1), a, shifted(a, v), false), (wid(2), b, shifted(b, v), false)]);
            let group = current.groups[1].key;
            let presented = midway(&current);
            let elsewhere = shifted(b, CGPoint::new(500.0, 0.0));
            let pass = reflow_plan(&[(wid(1), a, shifted(a, v), false), (wid(2), b, elsewhere, false)], DISPLAY);
            let (merged, delta) = merge_plans(&current, &pass, None, &presented, None, DISPLAY);
            assert_eq!(delta.reparented.len(), 1);
            let (window, from, to) = delta.reparented[0];
            assert_eq!((window, from), (wid(2), group));
            assert_ne!(to, group);
            assert_eq!(key_of(&merged, wid(1)), Some(group), "the other member keeps the container");
            assert!(delta.retargeted_groups.is_empty(), "the winning cluster confirmed the destination");
            assert!(dest(&merged, wid(2)).unwrap().same_as(elsewhere), "{:?}", dest(&merged, wid(2)));
            assert!(dest(&merged, wid(1)).unwrap().same_as(shifted(a, v)));
            assert_eq!(delta.new_groups.len(), 1, "no group had that remaining travel");
            assert_eq!(delta.new_groups[0].1, presented[&group], "installs where the old container is drawn");
        }

        /// The 1:07 zig-zag: members parked by a later pass ride their moving container out; a still
        /// container's member leaves on its own.
        #[test]
        fn a_member_parked_by_a_later_pass_rides_its_moving_container_out() {
            let (a, b, c) = (column(0.0), column(1.0), column(2.0));
            let up = CGPoint::new(0.0, -DISPLAY.size.height);
            let current = flight_of(&[
                (wid(1), a, shifted(a, up), false),
                (wid(2), b, shifted(b, up), false),
                (wid(3), c, shifted(c, up), false),
            ]);
            let group = key_of(&current, wid(1)).unwrap();
            let past_right = rect(DISPLAY.size.width + 1.0, b.origin.y, b.size.width, b.size.height);
            let past_left = rect(-c.size.width - 1.0, c.origin.y, c.size.width, c.size.height);
            let pass = reflow_plan(
                &[(wid(1), a, shifted(a, up), false), (wid(2), b, past_right, false), (wid(3), c, past_left, false)],
                DISPLAY,
            );
            let (merged, delta) = merge_plans(&current, &pass, None, &midway(&current), None, DISPLAY);
            assert!(delta.is_empty(), "{delta:?}");
            assert_eq!(merged, current);
            for w in [wid(1), wid(2), wid(3)] {
                assert_eq!(key_of(&merged, w), Some(group));
            }

            // A still container lends no motion: the member leaves on its own vector.
            let still = flight_of(&[(wid(1), a, a, false), (wid(2), b, b, false)]);
            let pass = reflow_plan(&[(wid(1), a, a, false), (wid(2), b, past_right, false)], DISPLAY);
            let (merged, delta) = merge_plans(&still, &pass, None, &midway(&still), None, DISPLAY);
            assert_eq!(delta.reparented.len(), 1, "{delta:?}");
            assert!(dest(&merged, wid(2)).unwrap().same_as(past_right));
        }

        #[test]
        fn a_rigid_member_turning_into_a_resize_goes_loose() {
            let (a, b) = (column(0.0), column(1.0));
            let v = CGPoint::new(-300.0, 0.0);
            let current = flight_of(&[(wid(1), a, shifted(a, v), false), (wid(2), b, shifted(b, v), false)]);
            let group = current.groups[1].key;
            let presented = midway(&current);
            let grown = rect(b.origin.x + v.x, b.origin.y, b.size.width + 400.0, b.size.height);
            let pass = reflow_plan(&[(wid(2), shifted(b, v), grown, false)], DISPLAY);
            let (merged, delta) = merge_plans(&current, &pass, None, &presented, None, DISPLAY);
            assert_eq!(delta.reparented, vec![(wid(2), group, GroupKey::Loose)]);
            assert_eq!(delta.retargeted_tiles, vec![(wid(2), grown)]);
            let Some(Member::Changing { from, to }) = merged.member(wid(2)) else { panic!("loose") };
            assert_eq!(from, overlay_of(b, presented[&group]), "leaves at the presented frame");
            assert_eq!(to, grown);
            assert_eq!(key_of(&merged, wid(1)), Some(group));
        }

        /// `rel` is taken from the container's presented position.
        #[test]
        fn a_join_matching_a_groups_remaining_travel_joins_it() {
            let (a, b) = (column(0.0), column(1.0));
            let v = CGPoint::new(-400.0, 0.0);
            let current = flight_of(&[(wid(1), a, shifted(a, v), false)]);
            let group = current.groups[1].key;
            let presented = midway(&current);
            let remaining = CGPoint::new(v.x - presented[&group].x, 0.0);
            let pass = reflow_plan(&[(wid(2), b, shifted(b, remaining), false)], DISPLAY);
            let (merged, delta) = merge_plans(&current, &pass, None, &presented, None, DISPLAY);
            assert_eq!(delta.joined_tiles, vec![(wid(2), group)]);
            assert!(delta.new_groups.is_empty());
            let Some(Member::Rigid { key, rel }) = merged.member(wid(2)) else { panic!("rigid") };
            assert_eq!(key, group);
            assert_eq!(rel, group_relative(b, presented[&group]));
            assert!(dest(&merged, wid(2)).unwrap().same_as(shifted(b, remaining)));
        }

        #[test]
        fn a_join_with_a_new_vector_opens_a_group() {
            let (a, b) = (column(0.0), column(1.0));
            let current = flight_of(&[(wid(1), a, shifted(a, CGPoint::new(-400.0, 0.0)), false)]);
            let pass = reflow_plan(&[(wid(2), b, shifted(b, CGPoint::new(120.0, 0.0)), false)], DISPLAY);
            let (merged, delta) = merge_plans(&current, &pass, None, &midway(&current), None, DISPLAY);
            assert!(delta.joined_tiles.is_empty());
            assert_eq!(delta.new_groups.len(), 1);
            let (key, install) = delta.new_groups[0];
            assert_eq!(install, CGPoint::new(0.0, 0.0));
            assert_eq!(key_of(&merged, wid(2)), Some(key));
            assert_eq!(merged.position_of(key), CGPoint::new(120.0, 0.0));
            assert_eq!(merged.next_key, current.next_key + 1);
        }


        /// The 3:27:20 case: an open with 22 survivors and one entrance, then a 574pt pan 56ms later.
        #[test]
        fn the_3_27_20_open_then_pan_ends_everything_at_the_pans_frames() {
            let col = |i: f64| rect(EXTERNAL.origin.x + 4.0 + i * 863.0, EXTERNAL.origin.y + 32.0, 859.0, 1081.0);
            let push = CGPoint::new(859.0, 0.0);
            // The open: columns from index 1 shift right by the newcomer's width.
            let requests: Vec<(WindowId, CGRect, CGRect, bool)> = (0..22)
                .map(|i| {
                    let f = col(i as f64);
                    let to = if i == 0 { f } else { shifted(f, push) };
                    (wid(i + 1), f, to, false)
                })
                .collect();
            let slot = col(1.0);
            let mut current = FlightPlan::from(reflow_plan(&requests, EXTERNAL));
            let slot_o = to_overlay_space(slot, EXTERNAL);
            current.entrances.push((wid(100), entrance_from(slot_o), slot_o));

            let d = CGPoint::new(-574.0, 0.0);
            let windows: Vec<SurfaceWindow> = requests
                .iter()
                .map(|(w, _, to, _)| SurfaceWindow {
                    window: *w,
                    server_id: WindowServerId::new(w.idx.get()),
                    frame: to_overlay_space(*to, EXTERNAL),
                    pinned: false,
                    floating: false,
                })
                .collect();
            let pan = plan::surface_plan(&windows, CGPoint::new(-574.0, 0.0), CGPoint::new(0.0, 0.0));
            let (merged, delta) = merge_plans(&current, &pan, Some(d), &midway(&current), None, DISPLAY);

            for (w, _, to, _) in &requests {
                let expected = shifted(to_overlay_space(*to, EXTERNAL), d);
                assert!(dest(&merged, *w).unwrap().same_as(expected), "{w:?}: {:?} vs {expected:?}", dest(&merged, *w));
            }
            assert_eq!(merged.entrances[0].2, shifted(slot_o, d));
            assert!(delta.reparented.is_empty());
            assert_eq!(delta.retargeted_groups.len(), 2, "the still group and the pushed group");
            assert!(delta.moves_anything());
        }

        #[test]
        fn a_member_the_pass_names_by_frame_only_rides_its_group() {
            let (a, b, c) = (column(0.0), column(1.0), column(2.0));
            let v = CGPoint::new(-300.0, 0.0);
            let current = flight_of(&[
                (wid(1), a, shifted(a, v), false),
                (wid(2), b, shifted(b, v), false),
                (wid(3), c, shifted(c, v), false),
            ]);
            let group = current.groups[1].key;
            // The pass composes 1 and 3 with a further 100pt; 2 has no picture.
            let further = CGPoint::new(-400.0, 0.0);
            let pass = reflow_plan(&[(wid(1), a, shifted(a, further), false), (wid(3), c, shifted(c, further), false)], DISPLAY);
            let (merged, delta) = merge_plans(&current, &pass, None, &midway(&current), None, DISPLAY);
            assert_eq!(delta.retargeted_groups, vec![(group, further)]);
            assert!(delta.reparented.is_empty() && delta.joined_tiles.is_empty());
            assert!(dest(&merged, wid(2)).unwrap().same_as(shifted(b, further)), "rides along");
        }

        fn is_zero(p: CGPoint) -> bool {
            p.x == 0.0 && p.y == 0.0
        }

        /// Property P1 (seed 149, 200 runs): every named window ends within 2pt of its destination in
        /// the key the delta says (P4); no window is in two groups; a pan changes no membership.
        #[test]
        fn merges_honour_every_destination_and_keep_the_partition() {
            let mut rng = Gen(149);
            for run in 0..RUNS {
                let mut plan = flight_of(&random_requests(&mut rng));
                let steps = 1 + rng.below(4) as usize;
                for step in 0..steps {
                    let tag = format!("seed 149 run {run} step {step}");
                    let presented = if rng.coin() { at_model(&plan) } else { midway(&plan) };
                    let windows = plan.windows();
                    let before = plan.clone();
                    let mut pan: Option<CGPoint> = None;
                    let incoming: ReflowPlan = match rng.below(4) {
                        // A pan.
                        0 => {
                            let mut d = CGPoint::new(rng.pt(-9.0, 9.0) * 100.0, 0.0);
                            if is_zero(d) {
                                d.x = 100.0;
                            }
                            pan = Some(d);
                            let mut p = ReflowPlan::empty();
                            for w in &windows {
                                let Some(from) = dest(&plan, *w) else { continue };
                                match plan.member(*w) {
                                    Some(Member::Rigid { .. }) => {
                                        p.adopt(&super::preservation::stacked(*w, from, shifted(from, d), None, false));
                                    }
                                    _ => {}
                                }
                            }
                            p
                        }
                        // A layout pass over a subset with a fresh vector or two.
                        1 | 2 => {
                            let v1 = CGPoint::new(rng.pt(-9.0, 9.0) * 50.0, 0.0);
                            let v2 = CGPoint::new(rng.pt(-9.0, 9.0) * 50.0, 0.0);
                            let mut reqs: Vec<(WindowId, CGRect, CGRect, bool)> = Vec::new();
                            for w in &windows {
                                if rng.coin() {
                                    continue;
                                }
                                let Some(Member::Rigid { key, rel }) = plan.member(*w) else { continue };
                                let from = overlay_of(rel, presented[&key]);
                                let v = if rng.coin() { v1 } else { v2 };
                                reqs.push((*w, from, shifted(from, v), false));
                            }
                            // Sometimes a newcomer.
                            if rng.coin() {
                                let from = rng.on_screen();
                                reqs.push((wid(500 + step as u32), from, shifted(from, v1), false));
                            }
                            reflow_plan(&reqs, DISPLAY)
                        }
                        // A resize of a random rigid member.
                        _ => {
                            let rigid: Vec<WindowId> = windows.iter().copied().filter(|w| matches!(plan.member(*w), Some(Member::Rigid { .. }))).collect();
                            let mut reqs: Vec<(WindowId, CGRect, CGRect, bool)> = Vec::new();
                            if let Some(w) = rigid.first() {
                                let Some(Member::Rigid { key, rel }) = plan.member(*w) else { unreachable!() };
                                let from = overlay_of(rel, presented[&key]);
                                let to = rect(from.origin.x, from.origin.y, from.size.width + 300.0, from.size.height);
                                reqs.push((*w, from, to, false));
                            }
                            reflow_plan(&reqs, DISPLAY)
                        }
                    };
                    let (merged, delta) = merge_plans(&plan, &incoming, pan, &presented, None, DISPLAY);

                    // Partition: no window in two groups, none lost.
                    let mut named = merged.windows();
                    let count = named.len();
                    named.sort();
                    named.dedup();
                    assert_eq!(named.len(), count, "{tag}: a window in two places");
                    for w in &windows {
                        assert!(merged.member(*w).is_some(), "{tag}: {w:?} dropped");
                    }

                    // P4: every named window ends within 2pt of its destination, in the key the delta says.
                    for w in incoming.windows() {
                        let want = match incoming.member(w).unwrap() {
                            Member::Rigid { key, rel } => overlay_of(rel, incoming.groups.iter().find(|g| g.key == key).unwrap().travel),
                            Member::Changing { to, .. } | Member::Entrance { to, .. } | Member::Floating { to, .. } => to,
                        };
                        let got = dest(&merged, w).unwrap_or_else(|| panic!("{tag}: {w:?} unnamed after merge"));
                        // The one P4 exception: a member sent off the viewport rides its moving container (`rides_out`).
                        let rode_out = pan.is_none()
                            && rini_geometry::is_off_screen(DISPLAY, want)
                            && matches!(before.member(w), Some(Member::Rigid { key, .. })
                                if before.groups.iter().any(|g| g.key == key && !g.is_still()));
                        if rode_out {
                            assert_eq!(key_of(&merged, w), key_of(&before, w), "{tag}: rides its container");
                            continue;
                        }
                        assert!(
                            (got.origin.x - want.origin.x).abs() <= GROUP_TOLERANCE + 0.01
                                && (got.origin.y - want.origin.y).abs() <= GROUP_TOLERANCE + 0.01,
                            "{tag}: {w:?} ends at {got:?}, pass wants {want:?}"
                        );
                        if let Some((_, _, to_key)) = delta.reparented.iter().find(|(x, _, _)| *x == w) {
                            assert_eq!(key_of(&merged, w), Some(*to_key), "{tag}");
                        }
                        if let Some((_, key)) = delta.joined_tiles.iter().find(|(x, _)| *x == w) {
                            assert_eq!(key_of(&merged, w), Some(*key), "{tag}");
                        }
                    }

                    // A pan: no membership change, every occupied position shifted by d.
                    if let Some(d) = pan {
                        assert!(delta.reparented.is_empty(), "{tag}");
                        for g in before.groups.iter().filter(|g| !g.members.is_empty()) {
                            let after = merged.groups.iter().find(|x| x.key == g.key).unwrap();
                            assert_eq!(after.members, g.members, "{tag}");
                            let (p0, p1) = (before.position_of(g.key), merged.position_of(g.key));
                            assert_eq!((p1.x - p0.x, p1.y - p0.y), (d.x, d.y), "{tag}");
                        }
                        assert_eq!(delta.retargeted_groups.is_empty(), is_zero(d) || before.groups.iter().all(|g| g.members.is_empty()), "{tag}");
                    }
                    plan = merged;
                }
            }
        }

        /// Property P5 (seed 151, 200 runs).
        #[test]
        fn a_pass_with_the_same_destinations_is_an_empty_delta() {
            let mut rng = Gen(151);
            for run in 0..RUNS {
                let requests = random_requests(&mut rng);
                let current = flight_of(&requests);
                let presented = if rng.coin() { at_model(&current) } else { midway(&current) };
                // The same destinations, expressed from the presented frames.
                let mut reqs: Vec<(WindowId, CGRect, CGRect, bool)> = Vec::new();
                for &(w, _, _, floating) in &requests {
                    let (from, to) = match current.member(w).unwrap() {
                        Member::Rigid { key, rel } => (overlay_of(rel, presented[&key]), overlay_of(rel, current.position_of(key))),
                        Member::Changing { from, to } | Member::Entrance { from, to } => (from, to),
                        Member::Floating { from, to } => (overlay_of(from, presented[&GroupKey::Floating]), overlay_of(to, current.position_of(GroupKey::Floating))),
                    };
                    reqs.push((w, from, to, floating));
                }
                let same = reflow_plan(&reqs, DISPLAY);
                let (merged, delta) = merge_plans(&current, &same, None, &presented, None, DISPLAY);
                assert!(delta.is_empty(), "seed 151 run {run}: {delta:?}");
                assert_eq!(merged, current, "seed 151 run {run}");
            }
        }
    }

    /// Task 1 of `.kiro/specs/rigid-strip-groups`: a pass as rigid pieces. See "Layout changes" and
    /// "Strip movements" in `docs/animation-smoothness.md`.
    mod rigid_groups {
        use super::preservation::{DISPLAY, Gen, RUNS, stacked};
        use super::*;
        use crate::engine::plan::*;
                use crate::motion::z_group::StackGroup;
        use crate::window_snapshot::is_a_resize;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        fn shifted(frame: CGRect, dx: f64, dy: f64) -> CGRect {
            CGRect::new(CGPoint::new(frame.origin.x + dx, frame.origin.y + dy), frame.size)
        }

        fn column(i: f64) -> CGRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        fn moving(plan: &ReflowPlan) -> Vec<&RigidGroup> {
            plan.groups.iter().filter(|g| !g.members.is_empty() && g.key != GroupKey::STILL).collect()
        }

        fn members(group: &RigidGroup) -> Vec<WindowId> {
            group.members.iter().map(|m| m.window).collect()
        }

        #[test]
        fn two_columns_shifting_by_the_same_vector_are_one_group() {
            let a = column(0.0);
            let b = column(1.0);
            let plan = reflow_plan(
                &[(wid(1), a, shifted(a, -859.0, 0.0), false), (wid(2), b, shifted(b, -859.0, 0.0), false)],
                DISPLAY,
            );
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].travel, CGPoint::new(-859.0, 0.0));
            assert_eq!(members(groups[0]), vec![wid(1), wid(2)]);
            assert_eq!(groups[0].members[0].rel, a, "a container installs at (0,0), so rel is from");
            assert!(plan.groups[0].members.is_empty(), "nothing stands still");
            assert!(plan.changing.is_empty() && plan.floating.is_empty() && plan.entrances.is_empty());
        }

        #[test]
        fn a_column_two_points_off_shares_the_group_and_three_points_off_opens_one() {
            let a = column(0.0);
            let b = column(1.0);
            let same = reflow_plan(
                &[(wid(1), a, shifted(a, 300.0, 0.0), false), (wid(2), b, shifted(b, 302.0, 0.0), false)],
                DISPLAY,
            );
            assert_eq!(moving(&same).len(), 1, "2pt apart: one group");
            assert_eq!(members(moving(&same)[0]), vec![wid(1), wid(2)]);

            let split = reflow_plan(
                &[(wid(1), a, shifted(a, 300.0, 0.0), false), (wid(2), b, shifted(b, 303.0, 0.0), false)],
                DISPLAY,
            );
            let groups = moving(&split);
            assert_eq!(groups.len(), 2, "3pt apart: two groups");
            assert_eq!(members(groups[0]), vec![wid(1)]);
            assert_eq!(members(groups[1]), vec![wid(2)]);
            assert_eq!(groups[1].travel, CGPoint::new(303.0, 0.0));
            assert_eq!(groups[0].key, GroupKey::Rigid(1));
            assert_eq!(groups[1].key, GroupKey::Rigid(2));
        }

        #[test]
        fn a_resizing_column_is_changing_and_in_no_group() {
            let a = column(0.0);
            let grown = rect(a.origin.x, a.origin.y, a.size.width + 400.0, a.size.height);
            let plan = reflow_plan(&[(wid(1), a, grown, false)], DISPLAY);
            assert_eq!(plan.changing, vec![(wid(1), a, grown)]);
            assert!(plan.group_of(wid(1)).is_none());
            assert!(moving(&plan).is_empty());
            assert_eq!(plan.member(wid(1)), Some(Member::Changing { from: a, to: grown }));
        }

        #[test]
        fn a_floating_window_is_floating_and_never_grouped() {
            let a = column(0.0);
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let plan = reflow_plan(
                &[
                    (wid(1), a, shifted(a, -100.0, 0.0), false),
                    (wid(2), settings, shifted(settings, -100.0, 0.0), true),
                    (wid(3), settings, settings, true),
                ],
                DISPLAY,
            );
            assert_eq!(members(moving(&plan)[0]), vec![wid(1)], "the same vector does not pull it in");
            assert_eq!(
                plan.floating,
                vec![(wid(2), settings, shifted(settings, -100.0, 0.0)), (wid(3), settings, settings)]
            );
            assert!(plan.group_of(wid(2)).is_none() && plan.group_of(wid(3)).is_none());
            assert_eq!(plan.floating_travel, CGPoint::new(0.0, 0.0), "a layout pass never moves the container");
        }

        #[test]
        fn the_still_window_is_in_the_still_group() {
            let a = column(0.0);
            let b = column(1.0);
            let plan = reflow_plan(&[(wid(1), a, a, false), (wid(2), b, shifted(b, 40.0, 0.0), false)], DISPLAY);
            assert_eq!(plan.groups[0].key, GroupKey::STILL);
            assert_eq!(plan.groups[0].travel, CGPoint::new(0.0, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![wid(1)]);
            assert_eq!(plan.member(wid(1)), Some(Member::Rigid { key: GroupKey::STILL, rel: a }));
            assert_eq!(members(moving(&plan)[0]), vec![wid(2)]);
        }

        #[test]
        fn a_window_leaving_for_a_park_rides_its_moving_neighbours_group() {
            let a = column(0.0);
            let b = column(1.0);
            let park = Gen(5).park(a.size);
            assert!(rini_geometry::is_off_screen(DISPLAY, park));
            let b_to = shifted(b, -863.0, 0.0);
            let others = [(b, b_to, false)];
            let travel = neighbour_travel(travel_subject(a, park, DISPLAY), &others, DISPLAY);
            assert_eq!(travel, Some(CGPoint::new(-863.0, 0.0)));
            let a_end = resolve_end(a, park, DISPLAY, travel);
            assert_eq!(a_end, shifted(a, -863.0, 0.0));

            let plan = reflow_plan(&[(wid(1), a, a_end, false), (wid(2), b, b_to, false)], DISPLAY);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![wid(1), wid(2)]);
            assert_eq!(groups[0].travel, CGPoint::new(-863.0, 0.0));
        }

        #[test]
        fn a_window_leaving_for_a_park_alone_is_a_group_of_one() {
            let a = column(1.0);
            let park = rect(DISPLAY.size.width - 1.0, DISPLAY.size.height - 1.0, a.size.width, a.size.height);
            let travel = neighbour_travel(travel_subject(a, park, DISPLAY), &[], DISPLAY);
            assert_eq!(travel, None);
            let a_end = resolve_end(a, park, DISPLAY, travel);
            assert_eq!(a_end, rini_geometry::park_entry_frame(park, a, DISPLAY));
            let still = column(0.0);

            let plan = reflow_plan(&[(wid(1), a, a_end, false), (wid(2), still, still, false)], DISPLAY);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![wid(1)]);
            assert_eq!(groups[0].travel, CGPoint::new(a_end.origin.x - a.origin.x, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![wid(2)]);
        }

        fn strip_window(idx: u32, frame: CGRect, pinned: bool, floating: bool) -> SurfaceWindow {
            SurfaceWindow { window: wid(idx), server_id: WindowServerId::new(idx), frame, pinned, floating }
        }

        /// The 3:27:20 pan: 22 survivors scrolled 574pt. One group, 22 members, one travel.
        #[test]
        fn the_3_27_20_pan_is_one_group_of_twenty_two() {
            let windows: Vec<SurfaceWindow> =
                (0..22).map(|i| strip_window(i + 1, column(i as f64), false, false)).collect();
            let from_offset = CGPoint::new(-574.0, 0.0);
            let to_offset = CGPoint::new(0.0, 0.0);
            let plan = plan::surface_plan(&windows, from_offset, to_offset);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].members.len(), 22);
            assert_eq!(groups[0].travel, CGPoint::new(-574.0, 0.0));
            assert_eq!(groups[0].travel, pan_travel(from_offset, to_offset));
            for (window, member) in windows.iter().zip(&groups[0].members) {
                let (from, to) = surface_travel(window.frame, from_offset, to_offset, false);
                assert_eq!(member.window, window.window);
                assert_eq!(member.rel, from);
                assert_eq!(overlay_of(member.rel, groups[0].travel), to, "rel plus travel is the destination");
            }
            assert!(plan.groups[0].members.is_empty());
            assert!(plan.floating.is_empty() && plan.changing.is_empty());
            assert_eq!(plan.floating_travel, CGPoint::new(0.0, 0.0));
        }

        #[test]
        fn pinned_windows_are_floating_with_zero_travel() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let windows = vec![
                strip_window(1, column(0.0), false, false),
                strip_window(2, settings, true, true),
            ];
            let plan = plan::surface_plan(&windows, CGPoint::new(-574.0, 0.0), CGPoint::new(0.0, 0.0));
            assert_eq!(plan.floating, vec![(wid(2), settings, settings)]);
            assert_eq!(plan.floating_travel, CGPoint::new(0.0, 0.0));
            assert_eq!(plan.member(wid(2)), Some(Member::Floating { from: settings, to: settings }));
            assert_eq!(members(moving(&plan)[0]), vec![wid(1)]);
        }

        #[test]
        fn a_switch_moves_the_floating_container_by_the_surface_travel() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let windows = vec![
                strip_window(1, column(0.0), false, false),
                strip_window(2, settings, false, true),
            ];
            let from_offset = CGPoint::new(0.0, 0.0);
            let to_offset = CGPoint::new(0.0, 1117.0);
            let plan = plan::surface_plan(&windows, from_offset, to_offset);
            let travel = pan_travel(from_offset, to_offset);
            assert_eq!(travel, CGPoint::new(0.0, -1117.0));
            assert_eq!(moving(&plan)[0].travel, travel);
            assert_eq!(plan.floating_travel, travel);
            assert_eq!(plan.floating, vec![(wid(2), settings, settings)], "the tile itself stands in its container");
            let (_, to) = surface_travel(settings, from_offset, to_offset, false);
            assert_eq!(overlay_of(settings, plan.floating_travel), to);
        }

        #[test]
        fn group_relative_and_overlay_of_are_inverses() {
            let f = rect(867.0, 32.0, 859.0, 1081.0);
            let p = CGPoint::new(-574.0, 12.0);
            assert_eq!(overlay_of(group_relative(f, p), p), f);
            assert_eq!(group_relative(f, p), rect(1441.0, 20.0, 859.0, 1081.0));
            assert_eq!(group_relative(f, CGPoint::new(0.0, 0.0)), f, "at install rel is from");
            let mut rng = Gen(3);
            for _ in 0..RUNS {
                let f = rng.on_screen();
                let p = CGPoint::new(rng.pt(-2000.0, 2000.0), rng.pt(-2000.0, 2000.0));
                assert_eq!(overlay_of(group_relative(f, p), p), f);
                assert_eq!(group_relative(f, p).size, f.size);
            }
        }

        #[test]
        fn a_fresh_flight_plan_positions_every_container_at_its_travel() {
            let a = column(0.0);
            let plan = reflow_plan(&[(wid(1), a, shifted(a, -100.0, 0.0), false), (wid(2), a, a, false)], DISPLAY);
            let flight = FlightPlan::from(plan.clone());
            assert_eq!(flight.groups, plan.groups);
            assert_eq!(flight.positions[&GroupKey::STILL], CGPoint::new(0.0, 0.0));
            assert_eq!(flight.positions[&GroupKey::Rigid(1)], CGPoint::new(-100.0, 0.0));
            assert_eq!(flight.positions[&GroupKey::Loose], CGPoint::new(0.0, 0.0));
            assert_eq!(flight.positions[&GroupKey::Floating], CGPoint::new(0.0, 0.0));
            assert_eq!(flight.next_key, 2);
            assert!(PlanDelta::default().is_empty());
        }

        /// A random pass: 1-12 windows over 1-4 distinct vectors (4pt apart on some axis, and from zero)
        /// plus ±1pt jitter; some still, resizing, or floating.
        pub(super) fn random_requests(rng: &mut Gen) -> Vec<(WindowId, CGRect, CGRect, bool)> {
            let mut palette: Vec<CGPoint> = Vec::new();
            let wanted = 1 + rng.below(4) as usize;
            while palette.len() < wanted {
                let v = CGPoint::new(rng.pt(-12.0, 12.0) * 50.0, if rng.coin() { 0.0 } else { rng.pt(-6.0, 6.0) * 50.0 });
                if (v.x == 0.0 && v.y == 0.0) || palette.contains(&v) {
                    continue;
                }
                palette.push(v);
            }
            let count = 1 + rng.below(12) as usize;
            (0..count)
                .map(|i| {
                    let from = rng.on_screen();
                    let window = wid(i as u32 + 1);
                    match rng.below(10) {
                        0 => (window, from, from, false),
                        1 => {
                            let to = rect(from.origin.x, from.origin.y, from.size.width + rng.pt(-300.0, 300.0), from.size.height);
                            let to = if is_a_resize(from.size, to.size) { to } else { rect(to.origin.x, to.origin.y, from.size.width + 200.0, to.size.height) };
                            (window, from, to, false)
                        }
                        2 => {
                            let v = palette[rng.below(palette.len() as u64) as usize];
                            (window, from, shifted(from, v.x, v.y), true)
                        }
                        _ => {
                            let v = palette[rng.below(palette.len() as u64) as usize];
                            let (jx, jy) = (rng.pt(-1.0, 1.0), rng.pt(-1.0, 1.0));
                            (window, from, shifted(from, v.x + jx, v.y + jy), false)
                        }
                    }
                })
                .collect()
        }

        /// Property (seed 131, 200 runs).
        #[test]
        fn a_reflow_plan_partitions_the_pass_into_rigid_groups() {
            let mut rng = Gen(131);
            for run in 0..RUNS {
                let requests = random_requests(&mut rng);
                let display = if run % 2 == 0 { DISPLAY } else { rect(1728.0, -300.0, 3008.0, 1692.0) };
                let plan = reflow_plan(&requests, display);
                let tag = format!("seed 131 run {run}");

                let mut named = plan.windows();
                named.sort();
                let mut asked: Vec<WindowId> = requests.iter().map(|r| r.0).collect();
                asked.sort();
                assert_eq!(named, asked, "{tag}: partition");
                assert!(plan.entrances.is_empty(), "{tag}");

                assert_eq!(plan.groups[0].key, GroupKey::STILL, "{tag}");
                assert_eq!(plan.groups[0].travel, CGPoint::new(0.0, 0.0), "{tag}");
                for (i, group) in plan.groups.iter().enumerate() {
                    assert_eq!(group.key, GroupKey::Rigid(i as u16), "{tag}: keys in plan order");
                    assert!(i == 0 || !group.members.is_empty(), "{tag}: an empty moving group");
                }

                for &(window, start, end, floating) in &requests {
                    let from = to_overlay_space(start, display);
                    let to = to_overlay_space(end, display);
                    let v = CGPoint::new(to.origin.x - from.origin.x, to.origin.y - from.origin.y);
                    match plan.member(window).unwrap_or_else(|| panic!("{tag}: {window:?} unnamed")) {
                        Member::Rigid { key, rel } => {
                            assert!(!floating, "{tag}: a floating window grouped");
                            assert!(!is_a_resize(from.size, to.size), "{tag}: a resize grouped");
                            assert_eq!(rel, from, "{tag}");
                            let group = plan.group_of(window).unwrap();
                            assert_eq!(group.key, key, "{tag}");
                            assert!(same_vector(group.travel, v), "{tag}: {v:?} in group at {:?}", group.travel);
                            if !is_moving(from, to) {
                                assert_eq!(key, GroupKey::STILL, "{tag}: a still window off the still group");
                            }
                        }
                        Member::Changing { from: f, to: t } => {
                            assert!(!floating && is_a_resize(from.size, to.size), "{tag}");
                            assert_eq!((f, t), (from, to), "{tag}");
                        }
                        Member::Floating { from: f, to: t } => {
                            assert!(floating, "{tag}");
                            assert_eq!((f, t), (from, to), "{tag}");
                        }
                        Member::Entrance { .. } => panic!("{tag}: no entrance was asked for"),
                    }
                }

                let vector_of = |window: WindowId| {
                    let r = requests.iter().find(|r| r.0 == window).unwrap();
                    CGPoint::new(r.2.origin.x - r.1.origin.x, r.2.origin.y - r.1.origin.y)
                };
                for (i, a) in plan.groups.iter().enumerate() {
                    for b in plan.groups.iter().skip(i + 1) {
                        for ma in &a.members {
                            for mb in &b.members {
                                let (va, vb) = (vector_of(ma.window), vector_of(mb.window));
                                assert!(
                                    (va.x - vb.x).abs() > GROUP_TOLERANCE || (va.y - vb.y).abs() > GROUP_TOLERANCE,
                                    "{tag}: {va:?} and {vb:?} in different groups"
                                );
                            }
                        }
                    }
                }
            }
        }

        /// The 3:28:10 open: the newcomer's slot at x=867, the neighbour shifted right by 859.
        #[test]
        fn an_open_beside_a_column_moves_the_neighbour_as_one_group() {
            let neighbour = rect(867.0, 32.0, 859.0, 1081.0);
            let left = column(0.0);
            let plan = reflow_plan(
                &[(wid(1), left, left, false), (wid(2), neighbour, shifted(neighbour, 859.0, 0.0), false)],
                DISPLAY,
            );
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![wid(2)]);
            assert_eq!(groups[0].travel, CGPoint::new(859.0, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![wid(1)]);
            assert!(plan.changing.is_empty() && plan.entrances.is_empty());
        }

        #[test]
        fn a_preset_resize_changes_the_middle_and_shifts_the_right_neighbour_as_a_group() {
            let (left, middle, right) = (column(0.0), column(1.0), column(2.0));
            let dw = 300.0;
            let grown = rect(middle.origin.x, middle.origin.y, middle.size.width + dw, middle.size.height);
            let plan = reflow_plan(
                &[
                    (wid(1), left, left, false),
                    (wid(2), middle, grown, false),
                    (wid(3), right, shifted(right, dw, 0.0), false),
                ],
                DISPLAY,
            );
            assert_eq!(plan.changing, vec![(wid(2), middle, grown)]);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![wid(3)]);
            assert_eq!(groups[0].travel, CGPoint::new(dw, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![wid(1)]);
        }

        #[test]
        fn a_close_shifts_every_survivor_as_one_group() {
            let w = 863.0;
            let survivors: Vec<(WindowId, CGRect, CGRect, bool)> = (1..=4)
                .map(|i| {
                    let f = column(i as f64);
                    (wid(i as u32), f, shifted(f, -w, 0.0), false)
                })
                .collect();
            let plan = reflow_plan(&survivors, DISPLAY);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), (1..=4).map(wid).collect::<Vec<_>>());
            assert_eq!(groups[0].travel, CGPoint::new(-w, 0.0));
            assert!(plan.groups[0].members.is_empty());
        }

        /// The closed window is not in the pass at all.
        #[test]
        fn a_close_composes_only_the_survivors_and_worth_flying_needs_a_mover() {
            let w = 863.0;
            let closed = wid(2);
            let survivors: Vec<(WindowId, CGRect, CGRect, bool)> = [1u32, 3, 4]
                .iter()
                .map(|&i| {
                    let f = column(i as f64);
                    let to = if i > 2 { shifted(f, -w, 0.0) } else { f };
                    (wid(i), f, to, false)
                })
                .collect();
            let plan = reflow_plan(&survivors, DISPLAY);
            assert!(plan.member(closed).is_none(), "nothing is drawn for the closed window");
            assert_eq!(moving(&plan).len(), 1);
            assert_eq!(members(moving(&plan)[0]), vec![wid(3), wid(4)]);
            assert!(!worth_flying(false, false));
            assert!(worth_flying(true, false));
            assert!(worth_flying(false, true));
        }

        #[test]
        fn a_floating_only_pass_has_no_groups_and_one_floating_member() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let plan = reflow_plan(&[(wid(1), settings, shifted(settings, 40.0, 20.0), true)], DISPLAY);
            assert!(moving(&plan).is_empty());
            assert!(plan.groups[0].members.is_empty());
            assert_eq!(plan.floating, vec![(wid(1), settings, shifted(settings, 40.0, 20.0))]);
            let flight = FlightPlan::from(plan);
            let targets = crate::overlay::animation_targets(&flight);
            assert_eq!(targets.len(), 1, "the floating tile flies on its own");
        }

        #[test]
        fn a_grow_still_enters_awaiting() {
            use crate::window_snapshot::{outgrows, test_snapshot};
            let a = column(0.0);
            let grown = rect(a.origin.x, a.origin.y, a.size.width + 600.0, a.size.height);
            let snapshot = test_snapshot(a.size);
            assert!(outgrows(snapshot.coverage.covered, grown.size));
            let mut running = RunningAnimation {
                tiles: Vec::new(),
                final_frames: vec![(wid(1), grown)],
                frames_applied: false,
                started: None,
                duration: Duration::from_millis(350),
                apply_at: 0.0,
                entrances: Vec::new(),
                awaiting: Vec::new(),
                hold_deadline: None,
                destination_refreshed: false,
                refresh_targets: Vec::new(),
                harvested: HashSet::new(),
                focus: None,
                plan: FlightPlan::empty(),
                _clock: None,
            };
            let frames = running.extend_hold(&[(wid(1), grown.size)], false, running.duration, Instant::now());
            assert_eq!(running.awaiting, vec![(wid(1), grown.size)]);
            assert!(running.hold_deadline.is_some());
            assert_eq!(frames, Some(vec![(wid(1), grown)]), "the held frames go out under the overlay");
        }

        #[test]
        fn entrance_plan_travels_from_spawn_or_reserves_with_a_reason() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let spawn = rect(300.0, 200.0, 640.0, 480.0);
            assert_eq!(
                entrance_plan(Some(spawn), slot, true, true),
                EntranceDecision::Travel { from: spawn, to: slot }
            );
            assert_eq!(entrance_plan(None, slot, true, true), EntranceDecision::Reserve("no server frame"));
            assert_eq!(
                entrance_plan(Some(rect(300.0, 200.0, 0.0, 0.0)), slot, true, true),
                EntranceDecision::Reserve("zero server frame")
            );
            assert_eq!(entrance_plan(Some(spawn), slot, false, true), EntranceDecision::Reserve("capture unusable"));
            assert_eq!(entrance_plan(Some(spawn), slot, true, false), EntranceDecision::Reserve("capture budget"));
        }

        #[test]
        fn frame_zero_work_sends_all_frames_when_holding_and_only_slots_otherwise() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let grown = rect(4.0, 32.0, 1200.0, 1081.0);
            let final_frames = vec![(wid(1), grown), (wid(2), slot)];
            let (holding, chase_set, now) = frame_zero_work(
                &[(wid(1), grown.size)],
                &[(wid(2), slot.size)],
                &final_frames,
                &[(wid(2), slot)],
            );
            assert!(holding);
            assert_eq!(now, final_frames, "holding: every frame goes out under the overlay");
            assert_eq!(chase_set, vec![(wid(1), grown.size), (wid(2), slot.size)]);

            let (holding, chase_set, now) =
                frame_zero_work(&[], &[(wid(2), slot.size)], &final_frames, &[(wid(2), slot)]);
            assert!(!holding);
            assert_eq!(now, vec![(wid(2), slot)], "not holding: the newcomer's slot alone");
            assert_eq!(chase_set, vec![(wid(2), slot.size)]);

            let (holding, chase_set, now) = frame_zero_work(&[], &[], &final_frames, &[]);
            assert!(!holding && chase_set.is_empty() && now.is_empty());
        }

        #[test]
        fn a_spawn_entrance_flies_without_a_hold_and_is_a_reveal_in_waiting() {
            use crate::window_snapshot::test_snapshot;
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let spawn = rect(300.0, 200.0, 400.0, 300.0);
            let EntranceDecision::Travel { from, to } = entrance_plan(Some(spawn), slot, true, true) else {
                panic!("travels");
            };
            let tile = OverlayTile {
                window: wid(9),
                from,
                to,
                snapshot: test_snapshot(spawn.size),
                floating: false,
                server_order: Some(0),
                depth: 0,
                companion: false,
                focused: true,
            };
            let awaiting: Vec<(WindowId, CGSize)> = Vec::new();
            let (holding, chase_set, now) =
                frame_zero_work(&awaiting, &[(wid(9), slot.size)], &[(wid(9), slot)], &[(wid(9), slot)]);
            assert!(!holding);
            assert_eq!(now, vec![(wid(9), slot)]);
            let running = RunningAnimation {
                tiles: vec![tile],
                final_frames: vec![(wid(9), slot)],
                frames_applied: holding,
                started: None,
                duration: Duration::from_millis(350),
                apply_at: 0.0,
                entrances: Vec::new(),
                awaiting: awaiting.clone(),
                hold_deadline: holding.then(|| Instant::now() + reveal_hold_limit(Duration::from_millis(350))),
                destination_refreshed: false,
                refresh_targets: Vec::new(),
                harvested: HashSet::new(),
                focus: Some(wid(9)),
                plan: FlightPlan::empty(),
                _clock: None,
            };
            assert!(running.awaiting.is_empty());
            assert!(!chase_set.is_empty());
            assert_eq!(running.hold_deadline, None);
            assert_eq!(running.phase(), FlightPhase::FrameZero);
            assert_eq!(
                running.tile_state(wid(9), &test_snapshot(spawn.size)),
                TileState::Reveal { fits: false },
                "the spawn picture does not cover the slot"
            );
            assert_eq!(running.tile_state(wid(9), &test_snapshot(slot.size)), TileState::Reveal { fits: true });
            // The plan calls it an entrance; the tile is a loose resize from spawn to slot.
            let mut plan = ReflowPlan::empty();
            plan.entrances.push((wid(9), from, to));
            assert_eq!(plan.member(wid(9)), Some(Member::Entrance { from, to }));
            let targets = crate::overlay::animation_targets(&FlightPlan::from(plan));
            assert_eq!(targets.len(), 1);
        }

        #[test]
        fn a_spawn_entrances_early_chase_picture_is_taken_without_a_release() {
            use crate::window_snapshot::test_snapshot;
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let spawn = rect(300.0, 200.0, 400.0, 300.0);
            let mut running = RunningAnimation {
                tiles: vec![stacked(wid(9), spawn, slot, Some(0), false)],
                final_frames: vec![(wid(9), slot)],
                frames_applied: false,
                started: None,
                duration: Duration::from_millis(350),
                apply_at: 0.0,
                entrances: Vec::new(),
                awaiting: Vec::new(),
                hold_deadline: None,
                destination_refreshed: false,
                refresh_targets: Vec::new(),
                harvested: HashSet::new(),
                focus: None,
                plan: FlightPlan::empty(),
                _clock: None,
            };
            running.tiles[0].snapshot = test_snapshot(spawn.size);
            assert_eq!(running.claim(wid(9), &test_snapshot(spawn.size)), None, "still spawn-sized");
            assert_eq!(running.claim(wid(9), &test_snapshot(slot.size)), Some(Claimed::Refreshed));
            assert!(running.tiles[0].snapshot.fits(slot.size));
            assert_eq!(running.claim(wid(77), &test_snapshot(slot.size)), None, "no such tile");
        }

        /// Property (seed 157, 200 runs).
        #[test]
        fn entrance_plan_travels_exactly_when_it_can() {
            let mut rng = Gen(157);
            for run in 0..RUNS {
                let slot = rng.on_screen();
                let spawn = match rng.below(4) {
                    0 => None,
                    1 => Some(rect(rng.pt(0.0, 1000.0), rng.pt(0.0, 800.0), 0.0, rng.pt(0.0, 500.0))),
                    _ => Some(rng.on_screen()),
                };
                let usable = rng.coin();
                let budget = rng.coin();
                let decision = entrance_plan(spawn, slot, usable, budget);
                let can = spawn.is_some_and(|f| f.size.width > 0.0 && f.size.height > 0.0) && usable && budget;
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

        /// The 50/50 pair with Settings over them (`model/z_group.rs`).
        #[test]
        fn band_plan_puts_the_floating_container_behind_the_strip_unless_it_holds_focus() {
            use crate::motion::z_group::{GROUP_STRIDE, tile_depth};
            let (left, right) = (rect(4.0, 32.0, 860.0, 1081.0), rect(868.0, 32.0, 856.0, 1081.0));
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let far = column(2.0);
            let v = CGPoint::new(-300.0, 0.0);
            // Left half focused: the pair stands, a far column slides, Settings floats.
            let mut tiles = vec![
                stacked(wid(90), left, left, Some(0), false),
                stacked(wid(5830), settings, settings, Some(1), true),
                stacked(wid(89), right, right, Some(2), false),
                stacked(wid(91), far, shifted(far, v.x, v.y), Some(3), false),
            ];
            let mut companion = stacked(wid(900), left, left, None, false);
            companion.companion = true;
            tiles.push(companion);
            restack(&mut tiles, Some(wid(90)));
            let plan = FlightPlan::from(plan_from_tiles(&tiles));
            let still = plan.groups[0].key;
            let moving = plan.groups[1].key;

            let banding = band_plan(&plan, &tiles, Some(wid(90)));
            assert!(!banding.floating_in_front);
            assert_eq!(banding.group_order, vec![still, moving], "the focused group first");
            assert_eq!(banding.within[&wid(90)], 0, "the focused window leads its container");
            assert_eq!(banding.within[&wid(89)], tile_depth(Some(2), false, StackGroup::Tiled, StackGroup::Tiled));
            assert_eq!(banding.within[&wid(5830)], tile_depth(Some(1), false, StackGroup::Floating, StackGroup::Floating));
            let anchor = tiles.iter().find(|t| t.window == wid(90)).unwrap().depth;
            assert_eq!(banding.within[&wid(900)], anchor % GROUP_STRIDE, "a companion takes its window's depth");

            restack(&mut tiles, Some(wid(5830)));
            let banding = band_plan(&plan, &tiles, Some(wid(5830)));
            assert!(banding.floating_in_front);
            assert_eq!(banding.within[&wid(5830)], 0);
            assert_eq!(banding.group_order, vec![still, moving], "no strip group holds focus: shallowest first");
        }

        /// Property P3 (seed 163, 200 runs): `container_z - within` is `-tile_depth` exactly.
        #[test]
        fn container_bands_plus_within_depths_reproduce_tile_depth() {
            use crate::motion::z_group::{container_z, tile_depth};
            let mut rng = Gen(163);
            for run in 0..RUNS {
                let count = 1 + rng.below(8) as usize;
                let mut tiles: Vec<OverlayTile> = (0..count)
                    .map(|i| {
                        let f = rng.on_screen();
                        let order = if rng.below(6) == 0 { None } else { Some(rng.below(40) as usize) };
                        stacked(wid(i as u32 + 1), f, f, order, rng.coin())
                    })
                    .collect();
                let focus = if rng.coin() { Some(wid(1 + rng.below(count as u64) as u32)) } else { None };
                restack(&mut tiles, focus);
                let plan = FlightPlan::from(plan_from_tiles(&tiles));
                let banding = band_plan(&plan, &tiles, focus);
                let focused_group = focus_group(focus, tiles.iter().map(|t| (t.window, t.floating)));
                assert_eq!(banding.floating_in_front, focused_group == StackGroup::Floating, "seed 163 run {run}");
                let tag = format!("seed 163 run {run}");
                let mut strip_total: Vec<f64> = Vec::new();
                let mut floating_total: Vec<f64> = Vec::new();
                for tile in &tiles {
                    let group = group_of(tile.floating);
                    let within = banding.within[&tile.window];
                    let total = container_z(group, focused_group) - within as f64;
                    let expected = -(tile_depth(tile.server_order, focus == Some(tile.window), group, focused_group) as f64);
                    assert_eq!(total, expected, "{tag}: {:?}", tile.window);
                    assert_eq!(total, -(tile.depth as f64), "{tag}: restack agrees");
                    if tile.floating { floating_total.push(total) } else { strip_total.push(total) }
                }
                for f in &floating_total {
                    for s in &strip_total {
                        if focused_group == StackGroup::Tiled {
                            assert!(f < s, "{tag}: floating {f} in front of strip {s}");
                        } else {
                            assert!(f > s, "{tag}: floating {f} behind strip {s}");
                        }
                    }
                }
                // Every occupied strip container is ordered once; the floating one never is.
                let mut order = banding.group_order.clone();
                order.sort_by_key(|k| format!("{k:?}"));
                order.dedup();
                assert_eq!(order.len(), banding.group_order.len(), "{tag}");
                assert!(!banding.group_order.contains(&GroupKey::Floating), "{tag}");
                for g in plan.groups.iter().filter(|g| !g.members.is_empty()) {
                    assert!(banding.group_order.contains(&g.key), "{tag}: {:?} unordered", g.key);
                }
            }
        }

        #[test]
        fn a_plan_rebuilt_from_tiles_matches_the_plan_from_requests() {
            let mut rng = Gen(139);
            for run in 0..RUNS {
                let requests = random_requests(&mut rng);
                let expected = reflow_plan(&requests, DISPLAY);
                let tiles: Vec<OverlayTile> = requests
                    .iter()
                    .map(|&(window, start, end, floating)| {
                        stacked(window, to_overlay_space(start, DISPLAY), to_overlay_space(end, DISPLAY), None, floating)
                    })
                    .collect();
                let rebuilt = plan_from_tiles(&tiles);
                assert_eq!(rebuilt, expected, "seed 139 run {run}");
            }
        }

        /// Property (seed 137, 200 runs), Requirement 11.4: `fly` installs what `animation_targets` names
        /// and nothing else.
        #[test]
        fn animation_targets_name_every_moving_piece_once_and_no_rigid_member() {
            use crate::overlay::{AnimationTarget, animation_targets};
            let mut rng = Gen(137);
            for run in 0..RUNS {
                let requests = random_requests(&mut rng);
                let mut plan = reflow_plan(&requests, DISPLAY);
                // A switch now and then: the floating container itself travels.
                let switch = run % 5 == 0 && !plan.floating.is_empty();
                if switch {
                    plan.floating_travel = CGPoint::new(0.0, -DISPLAY.size.height);
                    for (_, from, to) in plan.floating.iter_mut() {
                        *to = *from;
                    }
                }
                let flight = FlightPlan::from(plan.clone());
                let targets = animation_targets(&flight);
                let tag = format!("seed 137 run {run}");

                let mut containers: Vec<GroupKey> = Vec::new();
                let mut tiles: Vec<WindowId> = Vec::new();
                for target in &targets {
                    match *target {
                        AnimationTarget::Container { key, from, to } => {
                            assert!(!containers.contains(&key), "{tag}: {key:?} named twice");
                            containers.push(key);
                            let travel = match key {
                                GroupKey::Floating => plan.floating_travel,
                                key => plan.groups.iter().find(|g| g.key == key).unwrap().travel,
                            };
                            assert_eq!(from, CGPoint::new(0.0, 0.0), "{tag}: a fresh flight installs at the origin");
                            assert_eq!(to, travel, "{tag}");
                            assert!(travel.x != 0.0 || travel.y != 0.0, "{tag}: a zero-travel container");
                        }
                        AnimationTarget::Tile { window, .. } => {
                            assert!(!tiles.contains(&window), "{tag}: {window:?} named twice");
                            tiles.push(window);
                        }
                    }
                }
                for group in &plan.groups {
                    for member in &group.members {
                        assert!(!tiles.contains(&member.window), "{tag}: rigid {:?} as a tile", member.window);
                    }
                    assert_eq!(
                        containers.contains(&group.key),
                        !group.is_still(),
                        "{tag}: group {:?} travel {:?}",
                        group.key,
                        group.travel
                    );
                }
                assert_eq!(containers.contains(&GroupKey::Floating), switch, "{tag}");
                let mut expected: Vec<WindowId> = plan
                    .changing
                    .iter()
                    .chain(&plan.entrances)
                    .map(|(w, _, _)| *w)
                    .chain(plan.floating.iter().filter(|(_, f, t)| !f.same_as(*t)).map(|(w, _, _)| *w))
                    .collect();
                expected.sort();
                tiles.sort();
                assert_eq!(tiles, expected, "{tag}: tile targets");
            }
        }
    }
}
