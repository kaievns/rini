//! Drives the capture-based animation overlay.
//!
//! Owns the overlay and the snapshot cache, and runs on the main thread because Core Animation
//! requires it.
//!
//! One animation: capture the participating windows, build a tile each, show the overlay, let the
//! caller place the real windows underneath while they are covered, move the tiles over the duration,
//! then hide the overlay. The frame clock is time-based rather than a frame counter, so a late frame
//! skips instead of slowing the animation down.
//!
//! Every movement — layout changes and strip travel alike — becomes one group of per-tile Core
//! Animation animations committed in a single transaction (`begin_group`); the tick loop only
//! paces the mid-flight orchestration. See `docs/animation-smoothness.md`.
//!
//! Measurements behind all of this are in `docs/capture-overlay-research.md`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::MainThreadMarker;
use tracing::{debug, warn};

use crate::actor;
use crate::actor::app::WindowId;
use crate::model::HiddenWindowPlacement;
use crate::sys::geometry::SameAs;
use crate::sys::run_loop::RepeatingTimer;
use crate::sys::window_server::WindowServerId;
use crate::ui::snapshot_service::{SnapshotService, SnapshotTarget};
use crate::ui::window_snapshot::{
    SnapshotCache, WindowSnapshot, capture_via_framed_with_dressing, capture_via_skylight,
};
use crate::ui::workspace_overlay::{OverlayTile, WorkspaceOverlay};

pub(crate) mod plan;

/// One window's fixed place on the strip surface.
///
/// The surface holds every window across every workspace involved in a movement, laid out as one
/// continuous plane: x is the strip position, y is the workspace stacked below the one above it.
/// A group movement translates every window on it by the viewport's travel.
#[derive(Debug, Clone)]
pub struct StripWindow {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// Position on the strip surface, never interpolated.
    pub frame: CGRect,
    /// Held still while the strip moves under it.
    ///
    /// A floating window does not belong to the strip, so a strip scroll must not carry it along. It does
    /// belong to a workspace, so a switch between workspaces DOES move it, and that path leaves this false.
    pub pinned: bool,
    /// Off the strip, and so in the other z-order group. Separate from `pinned`, which is about whether the
    /// strip carries the window along: a workspace switch moves floating windows without unpinning them.
    pub floating: bool,
}

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
    /// Refresh the cached snapshot of one window, for windows that are off-strip and so cannot be
    /// captured usefully at switch time.
    RefreshSnapshot { window: WindowId, server_id: WindowServerId, size: CGSize },
    /// Drop snapshots for windows that no longer exist, so the cache cannot grow without bound.
    ForgetWindow(WindowId),
    /// Slide every currently visible window in from an offset, purely to evaluate animation quality
    /// by eye. Does not touch any real window, so it is safe to fire at any time.
    DebugSlide { dx: f64, dy: f64, duration: Duration },
    /// Move the whole strip surface — every window involved, translated by the same travel — so a
    /// long jump scrolls past everything in between instead of cutting to the destination.
    ///
    /// Drawn as one rigid group in one container ("Strip movements" in
    /// `docs/animation-smoothness.md`); the visual destinations are distinct from `final_frames`,
    /// because a window leaving the screen animates off it while its real frame goes to a park.
    AnimateStrip {
        windows: Vec<StripWindow>,
        from_offset: CGPoint,
        to_offset: CGPoint,
        /// Real screen frames to apply once the overlay is covering them.
        final_frames: Vec<(WindowId, CGRect)>,
        /// The window that will hold focus once this settles, drawn in front of the rest.
        focus: Option<WindowId>,
        duration: Duration,
    },
    /// Nudge the strip surface by `overshoot` and bring it back: a command ran into an end of
    /// the strip or of the workspace stack. The real windows stay where they are; `final_frames`
    /// is the layout they already sit at. Rides an in-flight movement additively when one is
    /// running. See "Edge bounce" in `docs/animation-smoothness.md`.
    BounceStrip {
        windows: Vec<StripWindow>,
        overshoot: CGPoint,
        final_frames: Vec<(WindowId, CGRect)>,
        focus: Option<WindowId>,
        duration: Duration,
    },
    /// One frame of the running animation. Posted by the run loop timer, not by any other actor.
    Tick,
    /// The layout passes have settled; start the clock. Posted by the coalesce timer.
    StartMoving,
    /// No flight began for `SETTLE_BEFORE_CAPTURES` after a lift: the captures the flight owes run
    /// now. Posted by the quiet timer.
    Quiet,
    /// A background capture has landed. Posted by the snapshot service, not by another actor.
    SnapshotsReady,
    /// A framed recapture has landed: a chase's reveal or the destination refresh. Posted by the
    /// capture thread, not by another actor. `settled` is the chase's gate (`chase_settled`): the
    /// window has repainted. Only settled pictures may satisfy a reveal hold or replace a resizing
    /// tile's picture: an unsettled capture of a resized-but-unpainted window is stable-looking
    /// garbage. What reaches a tile is `should_swap_mid_flight`'s call.
    PictureReady { window: WindowId, snapshot: WindowSnapshot, settled: bool },
    /// A hairline harvest finished on its background thread. Harvested OFF the capture service's
    /// completion queue, because the framed capture behind it is proxied through that same
    /// machinery and deadlocks it (see `snapshot_service`); this event carries the result back.
    DressingReady { window: WindowId, dressing: crate::ui::edge_dressing::EdgeDressing },
    /// Recapture the bar, now that nothing is animating over it. Posted by the refresh timer.
    RefreshBar,
    /// Recapture this window because focus has just moved to or from it, whatever its cached picture says.
    ///
    /// A window renders differently when it is focused, and none of it is a size change: measured on a
    /// 1pt window border, 65 of 255 focused against 42 unfocused. The size test that guards the ordinary
    /// warm cannot see that, so a picture taken while a window was unfocused stayed forever and its tile
    /// popped to the focused rendering at the handover.
    RefreshFocus(SnapshotTarget),
    /// Capture every managed window that SkyLight cannot serve, so the cache is warm before the next
    /// animation. Only queues background work, so it is safe to call at any time.
    ///
    /// Targets come from the reactor because only it knows each window's real [`WindowId`].
    WarmWindows(Vec<SnapshotTarget>),
    /// Warm from the window server rather than from rini's own window table. Only for the debug
    /// command, where there is no reactor-supplied window set.
    WarmCache,
}

pub type Sender = actor::Sender<Event>;
pub type Receiver = actor::Receiver<Event>;

/// Tick interval. Nothing is drawn on ticks — Core Animation carries every movement — so this only
/// paces the mid-flight orchestration: frame placement, destination recaptures, teardown.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// How far the surface gives when a command pushes past an end, in points. Enough to read as
/// the view straining against a stop, small enough that no column leaves its place.
pub const EDGE_BOUNCE_OVERSHOOT: f64 = 36.0;

/// The surface's nudge for a push in `direction`: the way the view was pushed, so the content
/// moves the opposite way, as it would have had there been anything further. Focus right at the
/// last column pulls the strip left; the next workspace at the bottom of the stack pulls the
/// row up.
pub fn edge_bounce_overshoot(direction: crate::layout_engine::Direction) -> CGPoint {
    use crate::layout_engine::Direction;
    match direction {
        Direction::Left => CGPoint::new(EDGE_BOUNCE_OVERSHOOT, 0.0),
        Direction::Right => CGPoint::new(-EDGE_BOUNCE_OVERSHOOT, 0.0),
        Direction::Up => CGPoint::new(0.0, EDGE_BOUNCE_OVERSHOOT),
        Direction::Down => CGPoint::new(0.0, -EDGE_BOUNCE_OVERSHOOT),
    }
}

/// How long to keep collecting windows before the animation starts moving.
///
/// The reactor arranges a layout over several passes, and treating each as its own animation restarted
/// the motion. The overlay goes up immediately and the clock starts once the passes settle, so windows
/// joining in between cannot pop. One frame is enough and is imperceptible.
const COALESCE_WINDOW: Duration = Duration::from_millis(25);

/// How far into a movement to recapture the window being switched into. Once per flight, at the
/// midpoint: the app has repainted as focused by then, and the real windows are not yet placed.
/// See "Mid-flight passes" in `docs/animation-smoothness.md`.
const REFRESH_DESTINATION_AT: f64 = 0.5;

/// A refresh landing at or after this progress is cached only: a cut this close to lift reads as
/// end-of-flight flicker.
const REFRESH_APPLY_BEFORE: f64 = 0.6;

/// How long after an animation to recapture the bar.
///
/// A bar composite measures 31ms median, so it cannot be paid at the start of a switch. Long enough after
/// the overlay hides that the compositor has dropped it out of the framebuffer, and long enough that a
/// burst of switches only pays it once, at the end.
const BAR_REFRESH_DELAY: Duration = Duration::from_millis(250);

/// Which of `tiles` to recapture mid-flight: the two ends of a focus change, and nothing else.
///
/// A focus change has two ends: the window being switched into needs its FOCUSED rendering, and
/// the window being left needs its unfocused one — with only the destination recaptured the
/// departing tile kept its focused look for the whole flight, reading as two active windows. Only
/// those two, and only when focus moved: a translucent window's two captures differ by the
/// wallpaper behind it, so recapturing whatever was frontmost cut those tiles on every flight,
/// focus change or not (seen 2026-09-16 2:05, two Ghostty tiles on every strip pan).
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

/// The destination refresh's requests: exactly one ScreenCaptureKit target per wanted window that
/// the pass knows a server id and size for, and the windows those targets cover, in order. One
/// route, so the refresh compares like with like against the cache `warm_windows` filled.
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

/// How far through a move-only layout flight the real windows are placed. Late enough that the
/// overlay is certainly covering them, early enough that the Accessibility writes land before it
/// lifts. Resizes and strips place earlier: `apply_frames_at`, "The apply point" in the doc.
const APPLY_FRAMES_AT: f64 = 0.75;

/// How a fresh group of tiles begins moving.
enum GroupStart {
    /// Wait one `COALESCE_WINDOW` for the reactor's layout passes to settle, then move. Right for
    /// layout changes, which arrive as several passes per keystroke.
    Coalesced,
    /// Move now. Right for strip movements, which arrive exactly once per keystroke and whose
    /// keypress-to-motion latency is the thing the eye notices most.
    Immediate,
}

/// How much larger than the window it traces a border window may be, per axis. JankyBorders draws
/// its stroke on a sibling window a few points larger than the traced one (2x the stroke width,
/// plus rounding); 8pt covers any plausible stroke without reaching the next column over.
const COMPANION_EXPANSION: f64 = 8.0;

/// How far the centers may disagree. The border window is centered on what it traces.
const COMPANION_CENTER_SLACK: f64 = 4.0;

/// The unmanaged window tracing `frame` as its border, if any.
///
/// Border tools (JankyBorders and kin) draw each border as its own window hugging the window it
/// traces. Those are real windows with real pixels, so the animation carries them as companion
/// tiles instead of trying to redraw the border itself — a drawn border is an approximation, and
/// any approximation flickers against the real one at the handover.
///
/// The trace test is geometric: same center, same-or-slightly-larger size. Candidates must already
/// exclude every managed window, or a stacked twin would match its sibling. A parked window never
/// traces and is never traced: every parked window shares the park's frame, so a window arriving
/// from the park matched another parked window there and flew in wearing its picture.
fn companion_of(
    frame: CGRect,
    candidates: &[(WindowServerId, CGRect)],
    display: CGRect,
) -> Option<(WindowServerId, CGRect)> {
    if HiddenWindowPlacement::is_off_screen(display, frame) {
        return None;
    }
    let center = |r: CGRect| {
        (r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0)
    };
    let (cx, cy) = center(frame);
    candidates
        .iter()
        .filter(|(_, candidate)| !HiddenWindowPlacement::is_off_screen(display, *candidate))
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

/// Where each tile of a strip movement starts and ends on screen, in overlay coordinates.
///
/// The strip surface is fixed; the viewport travels from `from_offset` to `to_offset`, so every
/// unpinned window translates by the opposite of that travel. A pinned window stands still — and a
/// standing tile still has to exist, because the overlay is opaque and anything it omits vanishes.
fn strip_travel(
    frame: CGRect,
    from_offset: CGPoint,
    to_offset: CGPoint,
    pinned: bool,
) -> (CGRect, CGRect) {
    if pinned {
        return (frame, frame);
    }
    let at = |offset: CGPoint| {
        CGRect::new(
            CGPoint::new(frame.origin.x - offset.x, frame.origin.y - offset.y),
            frame.size,
        )
    };
    (at(from_offset), at(to_offset))
}

struct RunningAnimation {
    tiles: Vec<OverlayTile>,
    /// Where each window must end up, in display coordinates. Sent to the reactor once the overlay is
    /// covering them, rather than applied up front.
    final_frames: Vec<(WindowId, CGRect)>,
    frames_applied: bool,
    /// `None` while still collecting windows. The animation is on screen but not yet moving.
    started: Option<Instant>,
    duration: Duration,
    /// Progress at which the real windows are placed: earlier when a resize is in flight.
    apply_at: f64,
    /// Windows waiting for a first picture. Each also holds in `awaiting`; the tile is composed
    /// when the picture lands (`claim`), or joins late with the remaining flight (`admit`).
    entrances: Vec<PendingEntrance>,
    /// Windows whose pixels are still being rendered — a grow's reveal, an entrance's first
    /// picture — with the size that counts as ready: the destination's, for both. The flight
    /// holds at frame zero until this empties or `hold_deadline` passes: the only truthful fill
    /// for a grow is a capture of the window at its new size.
    awaiting: Vec<(WindowId, CGSize)>,
    /// When to stop waiting for reveal pixels and fly with the cropped placeholder
    /// (`reveal_hold_limit`, at most `HOLD_CAP`).
    hold_deadline: Option<Instant>,
    /// Whether the window being focused has been recaptured. See `refresh_destination_among`.
    destination_refreshed: bool,
    /// The windows that refresh recaptured. Only their tiles may take a picture mid-flight, and
    /// only before `REFRESH_APPLY_BEFORE`; every other landing is cached for the next flight.
    refresh_targets: Vec<WindowId>,
    /// Windows whose hairline landed this flight (with a chase or refresh capture), so `finish`
    /// harvests the rest of the animated set once and nothing twice.
    harvested: HashSet<WindowId>,
    /// The window gaining focus, from the latest pass that named one. Its group is the one
    /// `restack` bands in front, for every tile in the flight whichever pass composed it.
    focus: Option<WindowId>,
    /// The flight as rigid pieces: what `install` composed and `fly` animates. Rebuilt from
    /// `tiles` while the flight is still collecting passes. See "The overlay engine" in
    /// `docs/animation-smoothness.md`.
    plan: plan::FlightPlan,
    /// Dropped when the animation ends, which invalidates the timer and stops the wakeups.
    _clock: Option<RepeatingTimer>,
}

/// A window that should join the animation as soon as it has a picture.
///
/// A window that just opened has never been captured. Rather than letting it pop in when the
/// overlay lifts, the flight holds at frame zero for its first picture and composes it as a tile
/// growing from nothing at its destination, in the survivors' transaction. See "Entrances are
/// holds" in `docs/animation-smoothness.md`.
#[derive(Debug, Clone)]
struct PendingEntrance {
    window: WindowId,
    /// Destination, in the overlay's coordinate space.
    to: CGRect,
    floating: bool,
}

/// Where an entering window grows in from: zero width at its own left edge, full height.
///
/// A resize from nothing to its final width, matching how every other column movement reads —
/// the crop-drawn tile reveals content rightward as the frame widens. Centred zero-size zoom was
/// tried first and read as the window inflating, which nothing else on the strip does.
fn entrance_from(to: CGRect) -> CGRect {
    CGRect::new(to.origin, CGSize::new(0.0, to.size.height))
}

/// The earlier apply point for an animation that resizes a window. A resize behind the overlay
/// costs three synchronous round trips into the owning app (see `flush_frames` in `actor/app.rs`),
/// so it needs more runway than a move to land before the overlay lifts.
const APPLY_FRAMES_AT_RESIZE: f64 = 0.5;

/// The apply point for a strip movement: frame zero. Pure moves behind an opaque overlay, and 17
/// serialized AX writes across Electron apps take longer than half a flight (24 of 162 flights
/// lifted with every window 1700-2600pt from its tile). See "The apply point" in
/// `docs/animation-smoothness.md`.
const APPLY_FRAMES_AT_STRIP: f64 = 0.0;

/// Which path composed a flight. See "The apply point" in `docs/animation-smoothness.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlightKind {
    /// A per-window layout pass: moves and resizes.
    Layout,
    /// A strip movement: pure translations.
    Strip,
}

/// Which apply point an animation needs.
fn apply_frames_at(kind: FlightKind, any_resize: bool) -> f64 {
    match (kind, any_resize) {
        (_, true) => APPLY_FRAMES_AT_RESIZE,
        (FlightKind::Layout, false) => APPLY_FRAMES_AT,
        (FlightKind::Strip, false) => APPLY_FRAMES_AT_STRIP,
    }
}

/// What a tile is doing when a picture of its window lands mid-flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileState {
    /// No tile for this window in the flight.
    NotTiled,
    /// The flight holds for this window's first picture: an entrance reservation.
    Awaiting,
    /// A grow waiting for its reveal: held at frame zero, or flying the placeholder because its
    /// picture cannot cover the destination. `fits`: the landed picture covers it.
    Reveal { fits: bool },
    /// An ordinary moving tile. `fits`: the picture covers the destination; `resizing`: the tile
    /// changes size in flight.
    Moving { fits: bool, resizing: bool },
    /// A moving tile whose picture is the flight's own destination refresh.
    MovingRefreshTarget { fits: bool, resizing: bool },
}

/// What to do with a picture that landed while a flight is running. Every picture is cached
/// first; this decides whether it also reaches the overlay.
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

/// How an incoming picture compares with the one cached for its window, judged before the cache
/// absorbs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CacheComparison {
    /// Renders the same within thumbprint tolerance. Only bitmap pairs can be judged; anything
    /// else counts as different.
    renders_like_cached: bool,
    /// Captured by the same route (`SnapshotSource`) as the cached picture. Different routes
    /// render a translucent window differently, so a route change alone reads as a change.
    same_source: bool,
}

/// Whether a picture landing mid-flight may change what a tile draws. `progress` is `None`
/// before the flight starts moving. A moving tile keeps its picture unless it is waiting for
/// one: a placeholder takes its settled reveal, the destination refresh target its recapture,
/// both only early. A resizing tile still needs a settled picture: an unsettled one can be the
/// resized-but-unpainted surface. The refresh also needs `same_source`: a picture from another
/// capture route differs from the cached one by route alone, and swapping it ping-pongs the tile
/// between two renderings every flight. See "Mid-flight passes" in
/// `docs/animation-smoothness.md`.
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

/// Whether a flight in `phase` may start `kind` of capture work now. Between frame zero and lift
/// only the chases and the one moving refresh may; see "Capture work in flight" in
/// `docs/animation-smoothness.md`.
fn capture_work_allowed(phase: FlightPhase, kind: CaptureKind) -> bool {
    match phase {
        FlightPhase::Idle => true,
        FlightPhase::FrameZero => matches!(kind, CaptureKind::Chase | CaptureKind::NeedsCapture),
        FlightPhase::Holding => matches!(kind, CaptureKind::Chase),
        FlightPhase::Moving => matches!(kind, CaptureKind::Chase | CaptureKind::Refresh),
    }
}

/// Parks warm targets asked for mid-flight, one per window: a later request for the same window
/// replaces the earlier one, since it carries the newer size.
fn defer_warm(deferred: &mut Vec<SnapshotTarget>, targets: Vec<SnapshotTarget>) {
    for target in targets {
        match deferred.iter_mut().find(|held| held.window == target.window) {
            Some(held) => *held = target,
            None => deferred.push(target),
        }
    }
}

/// Which animated windows `finish` harvests a hairline for: each at most once per flight. Skipped:
/// harvested with a chase or refresh, re-requested (the landing harvests), or already dressed.
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

/// Whether the desktop render in hand can back the next overlay, or `finish` should ask for a new
/// one: missing, sized for another display, or older than the picture staleness bound.
fn desktop_render_wanted(render: Option<(Duration, (f64, f64))>, display: (f64, f64)) -> bool {
    match render {
        None => true,
        Some((age, covered)) => {
            !crate::ui::window_snapshot::spans_display(covered, display)
                || crate::ui::window_snapshot::picture_is_stale(age)
        }
    }
}

/// Whether an in-flight merge leaves the already-applied frames stale. `changed`: a tile was
/// retargeted or joined; `frames_changed`: any final frame differs, tiled or not. A parked
/// window has no tile, so its frame change counts too. See "Mid-flight passes" in
/// `docs/animation-smoothness.md`.
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

/// Measures every tiled window's real frame against its intended one. A park is excluded: macOS
/// clamps it, so its error is the clamp, not the flight. Pure, so the report can be checked on
/// plain rects. See "Real windows land before lift" in `docs/animation-smoothness.md`.
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
        if crate::model::HiddenWindowPlacement::is_off_screen(display, *intended) {
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

/// Whether a chase capture counts as the window's settled rendering: it matches the previous
/// one, or it differs from the picture cached before the resize (the app has repainted). See "A
/// grow holds, then reveals" in `docs/animation-smoothness.md`.
fn chase_settled(prev: Option<&[u8]>, print: &[u8], pre_resize: Option<&[u8]>) -> bool {
    use crate::ui::edge_dressing::renderings_match;
    prev.is_some_and(|previous| renderings_match(previous, print))
        || pre_resize.is_some_and(|before| !renderings_match(before, print))
}

/// The thumbprint of a snapshot's bitmap; `None` for a surface, which cannot be compared.
fn bitmap_thumbprint(snapshot: &WindowSnapshot) -> Option<Vec<u8>> {
    match &snapshot.image {
        crate::ui::window_snapshot::SnapshotImage::Bitmap(image) => {
            crate::ui::edge_dressing::thumbprint(image)
        }
        _ => None,
    }
}

/// The longest a flight stands still at frame zero for a reveal. A slower app flies with the
/// stretched placeholder. See "A grow holds, then reveals" in `docs/animation-smoothness.md`.
const HOLD_CAP: Duration = Duration::from_millis(300);

/// How long a grow may hold at frame zero waiting for its reveal pixels, from the flight's
/// duration: 0.4·d with a 300ms floor, capped at `HOLD_CAP`.
fn reveal_hold_limit(duration: Duration) -> Duration {
    duration.mul_f64(0.4).max(Duration::from_millis(300)).min(HOLD_CAP)
}

/// How long a holding flight still waits before flying with the placeholder: the time to its
/// deadline, or `None` once that has passed. See "A grow holds, then reveals" in the doc.
fn hold_wait(hold_deadline: Option<Instant>, now: Instant) -> Option<Duration> {
    let deadline = hold_deadline?;
    (now < deadline).then(|| (deadline - now).max(Duration::from_millis(10)))
}

/// How often the chase thread polls a growing window's real frame (a cheap window-server read;
/// the capture itself only runs once the size is there), and how many times before giving up:
/// about a second in all. See "A grow holds, then reveals" in `docs/animation-smoothness.md`.
const REVEAL_CHASE_INTERVAL: Duration = Duration::from_millis(8);
const REVEAL_CHASE_ATTEMPTS: usize = 125;

/// What became of a tile offered to an animation in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admitted {
    /// Same window, same destination: a redundant layout pass. Nothing changes, and crucially
    /// nothing restarts — rapid presses produce a stream of these, and restarting on them is what
    /// held animations up forever.
    Redundant,
    /// Same window, new destination: the tile bends toward it mid-flight.
    Retargeted,
    /// A window this animation had not seen yet.
    Joined,
}

/// The merge decision, separated from the bookkeeping so it can be tested on plain rects.
fn merge_action(current_to: Option<CGRect>, incoming_to: CGRect) -> Admitted {
    match current_to {
        Some(to) if to.same_as(incoming_to) => Admitted::Redundant,
        Some(_) => Admitted::Retargeted,
        None => Admitted::Joined,
    }
}

/// Folds a later pass's destinations into the flight's. Latest frame per window wins. Returns
/// whether any window's destination is new or different.
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

/// Points a flight's reserved entrances at a later pass's destinations. An entrance has no tile
/// yet, so `merge_pass` cannot retarget it, and its `to` was fixed at reservation; a pan merging
/// into the open left the newcomer at its pre-pan slot while its neighbours scrolled. Returns how
/// many moved. See "Mid-flight passes" in `docs/animation-smoothness.md`.
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

/// Writes every tile's destination back from the merged plan, so the non-overlay readers
/// (`report_handover_error`, the next merge's redundancy test) see where the flight really ends:
/// a member the pass did not compose rides its group all the same.
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

/// How far a strip movement carries every unpinned tile: `strip_travel`'s `to - from`.
fn strip_pan_travel(from_offset: CGPoint, to_offset: CGPoint) -> CGPoint {
    CGPoint::new(from_offset.x - to_offset.x, from_offset.y - to_offset.y)
}

/// The frames a coalescing merge must send again, if any: frames already placed at frame zero are
/// stale once a later pass moves a window, and `step` will not place them a second time. See
/// "Resizes through the overlay" in `docs/animation-smoothness.md`.
fn reapply_set(
    frames_applied: bool,
    in_flight: bool,
    changed: bool,
    final_frames: &[(WindowId, CGRect)],
) -> Option<Vec<(WindowId, CGRect)>> {
    (frames_applied && !in_flight && changed).then(|| final_frames.to_vec())
}

/// How long a tile joining a flight already in motion travels: what is left of the flight, so it
/// lands with its neighbours and never outlives the overlay.
fn late_join_duration(duration: Duration, progress: f64) -> Duration {
    duration.mul_f64((1.0 - progress).max(0.0))
}

/// A newly opened window's place in the flight: the entrance reservation, and the reveal hold entry
/// it adds to `awaiting`. An entrance is a hold: the flight waits at frame zero for the window's
/// first picture like a grow waits for its reveal pixels.
fn entrance_reservation(
    window: WindowId,
    to: CGRect,
    floating: bool,
) -> (PendingEntrance, Option<(WindowId, CGSize)>) {
    (PendingEntrance { window, to, floating }, Some((window, to.size)))
}

/// How long after a lift the flight's captures wait for the user to stop. The warm of the
/// animated set (8-15 ScreenCaptureKit captures) and the desktop render ran the instant the
/// overlay lifted and took 600-800ms; a press inside that window flew the next flight against a
/// busy compositor, which stalled it 50-130ms at a time. Back-to-back presses now capture nothing
/// until the last one lands. See "Capture work in flight" in `docs/animation-smoothness.md`.
const SETTLE_BEFORE_CAPTURES: Duration = Duration::from_millis(400);

/// The capture work a lift leaves for the quiet period.
#[derive(Default)]
struct AfterFlight {
    targets: Vec<SnapshotTarget>,
    harvested: HashSet<WindowId>,
}

/// How long past its clock a flight waits for the render server to present the last frame and
/// for the real windows to land before lifting anyway. See "Real windows land before lift" in
/// `docs/animation-smoothness.md`.
const LIFT_GRACE: Duration = Duration::from_millis(350);

/// Whether the overlay lifts now: the clock has run out AND the render server presents every layer
/// at its destination AND every visible real window is where its tile finished; or the clock ran
/// out `LIFT_GRACE` ago. Lifting over windows still travelling showed them jump into place.
/// The flight's clock once a bounce of `bounce` joins it: long enough that the lift waits for the
/// return leg, never shorter than it was. `started` is when the flight began moving; a flight
/// still collecting keeps at least the bounce.
fn clock_for_bounce(started: Option<Instant>, duration: Duration, bounce: Duration) -> Duration {
    let needed = started.map_or(bounce, |s| s.elapsed() + bounce);
    duration.max(needed)
}

fn lift_now(clock_done: bool, settled: bool, landed: bool, overdue: bool) -> bool {
    clock_done && ((settled && landed) || overdue)
}

/// How many newly opened windows one pass captures synchronously at their spawn frame (16-24ms
/// each); the rest take a reservation. See "A window that opens travels from its spawn frame" in
/// `docs/animation-smoothness.md`.
const MAX_SYNC_ENTRANCE_CAPTURES: usize = 4;

/// How a newly opened window enters a flight.
#[derive(Debug, Clone, Copy, PartialEq)]
enum EntranceDecision {
    /// Its tile travels from the frame macOS showed it at to its slot.
    Travel { from: CGRect, to: CGRect },
    /// No usable picture at spawn: a reservation, held for the first picture (`entrance_reservation`).
    Reserve(&'static str),
}

/// `Travel` iff the window server reports a frame with size, the spawn capture is usable and the
/// pass has capture budget left. `from` is the spawn frame, never the zero-width `entrance_from`.
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

/// What a fresh flight does at frame zero: whether it holds (the real frames go out now, under
/// the covering overlay), which windows the reveal chase follows (holds and spawn entrances), and
/// which frames go out now: every final frame when holding, else the newcomers' slots alone so
/// their chase can capture them at slot size.
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

/// The tile for a reserved entrance whose picture has landed: growing from zero width at its own
/// left edge, frontmost (`server_order: Some(0)`, a window is raised on open) with the focused
/// shadow, since a window that just opened is about to hold focus.
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
    /// Taken by a composed tile standing at frame zero (a newcomer's slot-size picture); no hold
    /// was involved and nothing is released.
    Refreshed,
}

/// Whether a composed pass is worth an overlay flight at all. False only when nothing drawable
/// moves and no flight is running: see "Layout changes" in `docs/animation-smoothness.md`.
fn worth_flying(moving_drawable: bool, running: bool) -> bool {
    moving_drawable || running
}

/// Depth for every tile in the flight, banded by z-group (`tile_depth` in `model/z_group.rs`):
/// the focused window, then the rest of its group, then the other group, the window server's
/// order kept within a band. The strip is one z-order group, so with a strip focus (or none)
/// every floating tile is behind every strip tile, whichever pass composed it. Companions keep
/// the depth of the window they trace. The real windows are put in the same order by the
/// reactor's regroup (`strip_regroup`), so both ends of a flight match. See "Mid-flight passes"
/// in `docs/animation-smoothness.md`.
fn restack(tiles: &mut [OverlayTile], focus: Option<WindowId>) {
    let focused_group = focus_group(focus, tiles.iter().map(|t| (t.window, t.floating)));
    for tile in tiles.iter_mut().filter(|t| !t.companion) {
        tile.depth = crate::model::z_group::tile_depth(
            tile.server_order,
            focus == Some(tile.window),
            group_of(tile.floating),
            focused_group,
        );
    }
}

/// The flight's z-order as containers: whether the floating container is in front, every tile's
/// depth inside its container, and the strip containers' order (the one holding focus first).
/// `container_z - within` reproduces `-tile_depth`, so the overlay draws what `restack` and the
/// reactor's regroup agree on. See "The overlay engine" in `docs/animation-smoothness.md`.
fn band_plan(
    plan: &plan::FlightPlan,
    tiles: &[OverlayTile],
    focus: Option<WindowId>,
) -> plan::Banding {
    use crate::model::z_group::{GROUP_STRIDE, StackGroup, tile_depth};
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
        strip.push((plan::GroupKey::StripLoose, holds_focus, shallowest));
    }
    strip.sort_by_key(|(_, holds_focus, shallowest)| (!*holds_focus, *shallowest));
    plan::Banding {
        floating_in_front: focused_group == StackGroup::Floating,
        within,
        strip_order: strip.into_iter().map(|(key, _, _)| key).collect(),
    }
}

/// The window server ids a border companion may never be: the pass's windows plus every window
/// rini has a picture of or owes one to. A border window belongs to a border tool, which rini never
/// manages, so nothing managed can be a companion. Synthetic (companion) ids carry pid 0 and are
/// left out, or a border seen once could never be matched again.
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
fn group_of(floating: bool) -> crate::model::z_group::StackGroup {
    if floating {
        crate::model::z_group::StackGroup::Floating
    } else {
        crate::model::z_group::StackGroup::Strip
    }
}

/// The group the window gaining focus belongs to, which decides which group is drawn in front.
///
/// Falls back to the strip when the focus target is not among the windows being animated, since
/// that is where focus lands for every movement the strip itself makes.
fn focus_group(
    focus: Option<WindowId>,
    mut windows: impl Iterator<Item = (WindowId, bool)>,
) -> crate::model::z_group::StackGroup {
    let Some(focus) = focus else { return crate::model::z_group::StackGroup::Strip };
    windows
        .find(|(window, _)| *window == focus)
        .map(|(_, floating)| group_of(floating))
        .unwrap_or(crate::model::z_group::StackGroup::Strip)
}

impl RunningAnimation {
    /// Progress from the clock, not from a frame count, so a late frame skips ahead instead of
    /// stretching the animation.
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

    /// Past the clock by more than `LIFT_GRACE`: the overlay lifts whether or not the render
    /// server reports the tiles settled, so a stuck presentation cannot hold it up.
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

    /// What `window` is doing in this flight when a picture of it lands, for
    /// `should_swap_mid_flight`. Entrances and holds come first; a tile flying a placeholder
    /// (its picture cannot cover its destination) is still a reveal in waiting.
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
        if crate::ui::window_snapshot::outgrows(tile.snapshot.coverage.covered, tile.to.size) {
            return TileState::Reveal { fits };
        }
        let resizing = crate::ui::window_snapshot::is_a_resize(tile.from.size, tile.to.size);
        if self.refresh_targets.contains(&window) {
            TileState::MovingRefreshTarget { fits, resizing }
        } else {
            TileState::Moving { fits, resizing }
        }
    }

    /// A later pass carrying reveal holds. A grow can only extend a hold, not stop a flight: one
    /// already moving keeps the placeholder-then-re-key path, since yanking it back to frame zero
    /// is worse. Returns the frames a held merge must apply now, under the covering overlay: the
    /// app can only rerender once its real frame is set.
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

    /// The frames the flight still owes the reactor at `progress`: everything, once the apply
    /// point is reached and nothing was placed. `None` before it or once they went out.
    fn frames_due(&mut self, progress: f64) -> Option<Vec<(WindowId, CGRect)>> {
        if progress < self.apply_at || self.frames_applied {
            return None;
        }
        self.frames_applied = true;
        Some(self.final_frames.clone())
    }

    /// Takes a settled picture for a window this flight is holding for. A reserved entrance becomes
    /// a tile at zero width in the frame-zero composition, so it flies in the same transaction as
    /// the survivors; a grow gets its reveal picture. `None` when the flight is not holding for
    /// the window: already moving, not awaiting it, or the picture does not cover the destination.
    /// An entrance needs the fit like a grow: its real frame went to the slot at frame zero, so
    /// the chase captures it at slot size. A spawn-size picture drawn over the slot was a hole.
    fn claim(&mut self, window: WindowId, snapshot: &WindowSnapshot) -> Option<Claimed> {
        if self.started.is_some() {
            return None;
        }
        let Some(position) = self.awaiting.iter().position(|(w, _)| *w == window) else {
            // Not a hold: a newcomer travelling from its spawn frame, whose chase landed before
            // the flight moved. Its tile takes the slot-size picture; nothing is released.
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

    /// Takes the first picture of a reserved entrance after the flight has started moving: the
    /// hold deadline passed, or the window joined a pass merged in flight. Returns the banded tile
    /// and how long it travels, which is what is left of the flight.
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

    /// After an in-flight merge: the frames already requested are stale if any destination
    /// changed, tiled or not, so `step` asks again at the apply point.
    fn absorb_in_flight_change(&mut self, changed: bool, frames_changed: bool) {
        if mark_stale_on_untiled_change(changed, frames_changed) {
            self.frames_applied = false;
        }
    }

    /// Folds one later pass into the flight's tiles and focus, for the non-overlay readers; the
    /// overlay follows `merge_plans`. Depths are rebanded by the flight's latest focus. Returns
    /// what became of each tile, in order.
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

    /// Adds or retargets one window without disturbing anything already moving, reporting which of
    /// the two happened so the caller knows whether any real work follows.
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
                // Keep the original start so a window already moving is not yanked backwards, and
                // take the newer destination so the animation ends where the window really goes.
                existing.to = tile.to;
                existing.snapshot = tile.snapshot;
                existing.floating = tile.floating;
                existing.server_order = tile.server_order;
                // Only a companion's depth is final here; the rest are restacked by `restack`.
                existing.depth = tile.depth;
                existing.companion = tile.companion;
                existing.focused = tile.focused;
            }
            Admitted::Joined => self.tiles.push(tile),
        }
        action
    }
}

/// The pictures that only make sense for the display the overlay is on.
///
/// One struct rather than four fields, and forgotten as a unit, because a display change used to clear only
/// the bar. The overlay then drew an external display's desktop, 3008x1692, behind a built-in display's
/// strips on a 1728x1117 overlay.
#[derive(Default)]
struct DisplayPictures {
    /// Whatever the backdrop is currently showing. The per-window path reuses it rather than capturing: a
    /// desktop composite measures 13ms to 36ms, which is a frame or two of lag on every window focus
    /// change, while re-applying a held picture is a pointer assignment.
    shown: Option<WindowSnapshot>,
    /// The desktop as ScreenCaptureKit rendered it, which is the only source that reliably includes the
    /// wallpaper. Held rather than re-requested per animation because it costs about 40ms.
    desktop: Option<WindowSnapshot>,
    /// The last usable picture of the bar. Held because the bar can only be captured while the overlay is
    /// not covering it, so a switch chained onto one already in flight has to reuse this one.
    bar: Option<WindowSnapshot>,
    /// Whether a usable desktop has ever been drawn behind the strips. Until one has, even a capture
    /// missing its wallpaper is worth drawing, because the alternative is the bare black window.
    drawn_once: bool,
}

impl DisplayPictures {
    /// Drops every held picture. Assigns the whole struct so a new field cannot be left behind.
    fn forget(&mut self) {
        *self = Self::default();
    }
}

pub struct WorkspaceAnimation {
    rx: Receiver,
    /// Used by the frame timer to post `Tick` back into this actor's own queue, so frames arrive
    /// through the same path as every other event and need no separate locking.
    tx: Sender,
    mtm: MainThreadMarker,
    overlay: Option<WorkspaceOverlay>,
    cache: SnapshotCache,
    /// Full-size captures for windows SkyLight cannot serve. Results are collected into `cache`
    /// rather than read directly, so a capture landing mid-animation cannot change what is drawn.
    service: SnapshotService,
    display: Option<(CGRect, f64)>,
    /// Which display the overlay is on, so the desktop can be captured for that screen. Kept beside
    /// `display` rather than folded into it because only the desktop capture needs it.
    display_id: Option<u32>,
    running: Option<RunningAnimation>,
    /// Fires once after the layout passes settle, to start the animation moving.
    coalesce: Option<RepeatingTimer>,
    /// Fires once, `SETTLE_BEFORE_CAPTURES` after a lift, unless a flight begins first.
    quiet: Option<RepeatingTimer>,
    /// The capture work the last flights owe, run at `Quiet`: the animated set to warm and the
    /// windows whose hairline landed in flight.
    after_flight: Option<AfterFlight>,
    /// Windows from the most recent animation, so the post-animation refresh uses real ids.
    last_animated: Vec<SnapshotTarget>,
    /// Warms asked for during a flight, one per window, requested at `finish`. See "Capture work
    /// in flight" in `docs/animation-smoothness.md`.
    deferred_warm: Vec<SnapshotTarget>,
    /// Whether the desktop render was missing or stale at composition; re-rendered at `finish`.
    deferred_desktop: bool,
    /// The focus the previous flight landed on. The mid-flight refresh recaptures only when the
    /// current flight's focus differs (`refresh_targets`). Kept across a flight that names no
    /// focus, which is a flight that did not move it.
    last_focus: Option<WindowId>,
    /// Everything held that is a picture of one particular display.
    pictures: DisplayPictures,
    /// Fires once after an animation, to recapture the bar away from the critical path.
    bar_refresh: Option<RepeatingTimer>,
    /// Used to ask the reactor to place real windows once they are hidden behind the overlay.
    reactor_tx: Option<actor::Sender<crate::actor::reactor::Event>>,
}

impl WorkspaceAnimation {
    pub fn new(rx: Receiver, tx: Sender, mtm: MainThreadMarker) -> Self {
        // The service completes captures on a background queue, so it wakes this actor through the
        // same channel every other event arrives on rather than touching the cache itself.
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
            reactor_tx: None,
        }
    }

    /// Gives the actor a way back to the reactor, for placing real windows mid-animation.
    pub fn set_reactor(&mut self, reactor_tx: actor::Sender<crate::actor::reactor::Event>) {
        self.reactor_tx = Some(reactor_tx);
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
            Event::AnimateStrip {
                windows,
                from_offset,
                to_offset,
                final_frames,
                focus,
                duration,
            } => {
                self.start_strip(windows, from_offset, to_offset, final_frames, focus, duration)
            }
            Event::BounceStrip { windows, overshoot, final_frames, focus, duration } => {
                self.start_bounce(windows, overshoot, final_frames, focus, duration)
            }
            Event::RefreshSnapshot { window, server_id, size } => {
                self.refresh_snapshot(window, server_id, size)
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
            // Straight to the service, with no size test in the way. Background work, so a focus change
            // costs nothing on the main thread.
            Event::RefreshFocus(target) => self.service.request(vec![target]),
        }
    }

    /// Moves completed background captures into the cache.
    ///
    /// `SnapshotCache::insert` refuses to replace a usable capture with a clipped one, so a result
    /// that lands late cannot downgrade what is already held.
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
        // Hairlines for the batch. Mid-flight the batch is cached without one (the worn ring
        // carries over) and `finish` harvests the animated set instead.
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
            // So a hold keeps waiting for its chase; only a late entrance (`admit`) or the
            // refresh's own ScreenCaptureKit route reaches a tile. Everything else waits in the
            // cache. See "Mid-flight passes" in `docs/animation-smoothness.md`.
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

    /// Offers a landed picture to the running flight per `should_swap_mid_flight`. The cache
    /// has it already; this only decides whether the overlay sees it too.
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
            // An unsettled capture of a held window can be its unpainted surface; the chase's
            // settled one is the reveal.
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

    /// Queues background captures for a set of windows the reactor identified.
    ///
    /// Already-held windows are skipped by the service, and the cache keeps what it has unless
    /// something better arrives, so calling this after every switch settles rather than re-capturing.
    ///
    /// During a flight nothing is requested: the targets wait in `deferred_warm` for `finish`.
    /// Returns the windows requested now.
    fn warm_windows(&mut self, targets: Vec<SnapshotTarget>) -> Vec<WindowId> {
        if !capture_work_allowed(self.phase(), CaptureKind::Warm) {
            defer_warm(&mut self.deferred_warm, targets);
            return Vec::new();
        }
        let wanted: Vec<SnapshotTarget> = targets
            .into_iter()
            // Drawable is not enough: the picture also has to match the size the window is now. A window
            // resized from 859pt to 1147pt keeps a perfectly usable 859pt picture, and this used to skip
            // it forever, so it was dropped from every animation as the wrong shape and visibly vanished
            // for the length of each one. `target.size` is the size the layout just gave it.
            //
            // Nor is fitting enough: a fitting picture was kept FOREVER, so an off-strip window's
            // tile showed old content on every animation and snapped to the live window at each
            // handover. Age alone re-warms now.
            .filter(|target| {
                let cached = self.cache.usable(target.window);
                crate::ui::window_snapshot::needs_capture(
                    cached.map(|snapshot| snapshot.coverage),
                    (target.size.width, target.size.height),
                ) || cached.is_some_and(|snapshot| {
                    crate::ui::window_snapshot::picture_is_stale(snapshot.taken.elapsed())
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

    /// Queues background captures for every window on the display that SkyLight cannot serve.
    ///
    /// Cheap to call repeatedly: the service drops targets that are already in flight, and the cache
    /// keeps what it has until something better arrives.
    fn warm_cache(&mut self) {
        let Some((display_frame, _)) = self.display else {
            warn!("no display geometry yet; cannot warm the snapshot cache");
            return;
        };
        let windows = crate::sys::window_server::visible_windows_on_display(display_frame);
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
        // Warm on the first geometry, and after any change, so the very first switch has pixels
        // rather than being the one that fills the cache for later switches.
        if first || changed {
            self.warm_cache();
            // The desktop capture is the backdrop's only reliable source, and it takes about 40ms,
            // so it has to be in hand before the first switch rather than requested during one.
            self.warm_desktop();
            // Anything in flight was requested for the display we just left, and the desktop render is
            // sized to the display it was taken of.
            self.service.invalidate();
            self.pictures.forget();
            self.arm_bar_refresh();
        }
    }

    /// Creates the overlay on first use and keeps it forever. Creation costs about 112ms against a
    /// 14ms steady-state show, so it must not be paid per animation.
    fn ensure_overlay(&mut self) -> Option<&mut WorkspaceOverlay> {
        if self.overlay.is_none() {
            let (frame, scale) = self.display?;
            match WorkspaceOverlay::new(frame, scale, self.mtm) {
                Some(overlay) => self.overlay = Some(overlay),
                None => {
                    warn!("could not create the animation overlay; animations will be skipped");
                    return None;
                }
            }
        }
        self.overlay.as_mut()
    }

    /// Recaptures both ends of a focus change mid-flight and swaps their tiles.
    ///
    /// Runs once per movement, at `REFRESH_DESTINATION_AT`. By that point the
    /// reactor has shown the destination and moved focus, so a fresh capture gets the app's
    /// FOCUSED rendering for the window being switched into and the dimmed one for the window
    /// being left — which is what the real windows will look like when the overlay lifts.
    /// Without this the tiles slide with whatever the pictures held: the destination arrives
    /// unfocused and snaps at the handover, and the departing window keeps its focused look for
    /// the whole flight, reading as two active windows.
    ///
    /// Only the two ends of the focus change (`refresh_targets`): a flight that moves focus
    /// nowhere, a strip pan say, recaptures nothing. No visibility filter: during a slide the
    /// destination is mid-scroll and only partly on screen; a clipped capture is rejected by the
    /// cache anyway.
    ///
    /// One capture route only: the ScreenCaptureKit service, the same route `warm_windows` fills
    /// the cache from (`refresh_targets` in `refresh_requests`). Racing it against a framed
    /// SkyLight capture swapped the tile twice or three times per flight, since the two routes
    /// render a translucent window differently. See "Mid-flight passes" in
    /// `docs/animation-smoothness.md`.
    fn refresh_destination_among(&mut self, tiles: &[(WindowId, WindowServerId, CGSize)]) {
        let current = self.running.as_ref().and_then(|running| running.focus);
        let windows: Vec<WindowId> = tiles.iter().map(|(w, _, _)| *w).collect();
        let wanted = refresh_targets(self.last_focus, current, &windows);
        if wanted.is_empty() {
            return;
        }
        let (wanted, requests) = refresh_requests(tiles, &wanted);
        // Only this route's result may reach the tile; nothing else landing mid-flight does.
        if let Some(running) = self.running.as_mut() {
            running.refresh_targets = wanted.clone();
        }
        debug!(windows = wanted.len(), "destination refresh requested");
        self.service.request(requests);
    }

    /// Harvests hairlines for `windows` on one plain thread. The service's completion queue must
    /// not make capture calls (see `snapshot_service`), and this actor's thread should not spend
    /// 16-24ms per window either; results come back as `DressingReady` events.
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
                        crate::ui::edge_dressing::harvest_edge_dressing(server_id, scale)
                    else {
                        continue;
                    };
                    _ = tx.send(Event::DressingReady { window, dressing });
                }
            })
            .ok();
    }

    /// Takes a finished hairline harvest: onto the cached snapshot, and onto a tile in flight.
    fn dressing_ready(&mut self, window: WindowId, dressing: crate::ui::edge_dressing::EdgeDressing) {
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
        // Compared before the cache absorbs the newcomer: a swap whose picture renders the same
        // as the one on screen is a cut for nothing. Swaps are hard cuts — a crossfade veil was
        // tried and rejected, since stacking two copies of a translucent window pulses its net
        // opacity — so the cheapest smoothness is not cutting at all.
        let comparison = self.compare_with_cached(window, &snapshot);
        if snapshot.dressing.is_some() {
            if let Some(running) = self.running.as_mut() {
                running.harvested.insert(window);
            }
        }
        self.cache.insert(window, snapshot.clone());
        self.offer_mid_flight(window, &snapshot, settled, comparison);
    }

    /// How an incoming picture compares with the cached one: same capture route, and rendering
    /// the same within thumbprint tolerance. With nothing cached there is no other rendering to
    /// ping-pong against, so the source counts as the same and the rendering as different.
    fn compare_with_cached(&self, window: WindowId, incoming: &WindowSnapshot) -> CacheComparison {
        use crate::ui::window_snapshot::SnapshotImage;
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
            crate::ui::edge_dressing::thumbprint(old),
            crate::ui::edge_dressing::thumbprint(new),
        ) {
            (Some(a), Some(b)) => crate::ui::edge_dressing::renderings_match(&a, &b),
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

    /// Chases the first truthful picture for a holding grow or entrance: one thread per window,
    /// polling the real frame — a cheap window-server read — then one framed capture per attempt,
    /// hairline included, until `chase_settled`. Not `capture_via_skylight` polling: that lost the
    /// race against the hold deadline. See "A grow holds, then reveals" in
    /// `docs/animation-smoothness.md`.
    fn chase_reveal_pictures(&self, awaiting: &[(WindowId, CGSize)]) {
        if !capture_work_allowed(self.phase(), CaptureKind::Chase) {
            return;
        }
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        for (window, size) in awaiting.iter().copied() {
            let server_id = WindowServerId::from(window);
            let tx = self.tx.clone();
            // The picture the tile flies from: a capture that no longer renders like it is the
            // app's repaint at the new size. An entrance has none.
            let pre_resize = self.cache.get(window).and_then(bitmap_thumbprint);
            std::thread::Builder::new()
                .name("reveal-chase".to_string())
                .spawn(move || {
                    // The frame resizes instantly; the app's PIXELS lag behind it. A capture taken
                    // between the two is a half-painted surface — delivering one flew the whole
                    // reveal with garbage — so a capture only counts once `chase_settled` says so.
                    // One framed capture per attempt, hairline included.
                    let mut last_print: Option<Vec<u8>> = None;
                    for _ in 0..REVEAL_CHASE_ATTEMPTS {
                        std::thread::sleep(REVEAL_CHASE_INTERVAL);
                        let Some(info) = crate::sys::window_server::get_window(server_id) else {
                            continue;
                        };
                        let frame_fits = crate::ui::window_snapshot::fits_frame(
                            (info.frame.size.width, info.frame.size.height),
                            (size.width, size.height),
                        );
                        if !frame_fits {
                            last_print = None;
                            continue;
                        }
                        let Some(snapshot) =
                            crate::ui::window_snapshot::capture_via_framed_with_dressing(
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

    /// Takes a landed picture for a window a holding flight is waiting on: a grow's reveal pixels,
    /// or a reserved entrance's first picture. Returns whether the hold claimed it.
    fn claim_reveal(&mut self, window: WindowId, snapshot: &WindowSnapshot) -> bool {
        let Some(running) = self.running.as_mut() else { return false };
        let Some(claimed) = running.claim(window, snapshot) else { return false };
        // Redraw frame zero with the new picture: the tiles are standing still, so this is a plain
        // recompose. A grow's crop grid now maps the final-size picture — the reveal — and an
        // entrance stands at zero width until `start_moving` flies everything together.
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

    /// Frame zero again, for a flight still collecting passes: the plan is rebuilt from the merged
    /// tiles and installed. Nothing is animating yet, so this is a plain recompose.
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
    /// moving: the late fallback behind `claim_reveal`. The tile grows from zero width for what is
    /// left of the flight, so it lands with its neighbours. Returns whether the picture was taken.
    fn admit_entrance(&mut self, window: WindowId, snapshot: &WindowSnapshot) -> bool {
        let Some(running) = self.running.as_mut() else { return false };
        let Some((tile, duration)) = running.admit(window, snapshot) else { return false };
        running.plan.entrances.push((tile.window, tile.from, tile.to));
        let banding = band_plan(&running.plan, &running.tiles, running.focus);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.add_tile(&tile, plan::GroupKey::StripLoose, &banding, duration);
        }
        debug!(pid = window.pid, idx = window.idx.get(), "window entered mid-flight");
        true
    }

    fn refresh_snapshot(&mut self, window: WindowId, server_id: WindowServerId, size: CGSize) {
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        if let Some(snapshot) = capture_via_skylight(server_id, (size.width, size.height), scale) {
            self.cache.insert(window, snapshot);
        }
    }

    /// The snapshot to draw for one window, from the cache only.
    ///
    /// Any usable picture, whatever its shape: a picture that no longer matches the frame is drawn
    /// cropped (`ContentMode::Crop` — corners and bands intact, seam absorbing the difference),
    /// which beats dropping the tile. Rapid preset cycling used to drop the resized window
    /// entirely because its cached picture lagged one press behind. A window with nothing cached
    /// at all gets an entrance reservation instead.
    fn snapshot_for(&mut self, request: &AnimationRequest) -> Option<WindowSnapshot> {
        self.cache.usable(request.window).cloned()
    }

    /// Does this window appear on screen at ANY point during the animation?
    ///
    /// The whole path is sampled, not just its ends: a window that sweeps across mid-animation is
    /// exactly what conveys how far the strip travelled, and testing endpoints alone excluded it.
    fn is_worth_animating(&self, from: CGRect, to: CGRect, display: CGRect) -> bool {
        worth_animating(from, to, display)
    }

    /// Tiles for the border windows tracing the windows being animated (JankyBorders and kin).
    ///
    /// Each anchor is the window's real frame in display space plus its tile's from/to/depth. The
    /// border window rides at the same relative offset for the whole flight and lands exactly
    /// where the real border window reappears — its own pixels, so there is nothing to mismatch at
    /// the handover. A drawn border was tried first and rejected: any approximation flickers
    /// against the real one.
    ///
    /// Returns the tiles plus a warm target per matched border, picture or not: borders recolor
    /// with focus, so they are refreshed after every flight the way windows are.
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
        // Every window rini manages is excluded, not only the pass's: two Chrome windows on
        // different workspaces share one park frame, and the one outside the pass matched the one
        // arriving as its "border" (the park clamps to 41pt visible, past `is_off_screen`), so the
        // arriving window flew in wearing the other's picture.
        let managed = managed_server_ids(
            exclude,
            self.cache.iter().map(|(window, _)| *window),
            self.deferred_warm.iter().chain(self.after_flight.iter().flat_map(|a| a.targets.iter())).map(|t| t.window),
        );
        let candidates: Vec<(WindowServerId, CGRect)> =
            crate::sys::window_server::visible_windows_on_display(display)
                .into_iter()
                .filter(|(id, _)| !managed.contains(&id.as_u32()))
                .collect();
        let mut claimed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut tiles = Vec::new();
        let mut targets = Vec::new();
        for &(real, from, to, depth) in anchors {
            let Some((server_id, frame)) = companion_of(real, &candidates, display) else { continue };
            // One border traces one window: stacked twins share a frame and must not all claim
            // the same border window.
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
                // Like a window with no picture: skipped this flight, warmed for the next.
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

        // Every window's destination, whether or not it has a picture: one with no snapshot is not drawn
        // but still has to be placed. Windows standing still are excluded, because asking an application
        // to move a window to where it already is costs a round trip and invites another layout pass.
        let final_frames: Vec<(WindowId, CGRect)> = windows
            .iter()
            .filter(|request| is_moving(request.from, request.to))
            .map(|request| (request.window, request.to))
            .collect();

        // Front-to-back order straight from the window server, so the overlay stacks tiles the way
        // the screen is actually stacked.
        let depths = crate::sys::window_server::front_to_back_depths();

        let any_resize = windows.iter().any(|request| {
            crate::ui::window_snapshot::is_a_resize(request.from.size, request.to.size)
        });
        let apply_at = apply_frames_at(FlightKind::Layout, any_resize);

        let mut tiles = Vec::with_capacity(windows.len());
        let mut skipped = 0usize;
        let mut offscreen = 0usize;
        let mut needs_capture: Vec<SnapshotTarget> = Vec::new();
        let mut entrances: Vec<PendingEntrance> = Vec::new();
        let mut awaiting: Vec<(WindowId, CGSize)> = Vec::new();
        // Newly opened windows travelling from their spawn frame: their loose tiles, the chase
        // that lands their slot-size picture, and the slots to request at frame zero.
        let mut spawn_entrances: Vec<(WindowId, CGRect, CGRect)> = Vec::new();
        let mut chase: Vec<(WindowId, CGSize)> = Vec::new();
        let mut entrance_frames: Vec<(WindowId, CGRect)> = Vec::new();
        let mut sync_captures = 0usize;
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        // Real frame per drawn window; depths are filled in after the restack.
        let mut starts: Vec<(WindowId, CGRect)> = Vec::new();
        // The resolved `(start, end, floating)` per drawn window, for `reflow_plan`.
        let mut resolved: Vec<(WindowId, CGRect, CGRect, bool)> = Vec::new();
        // The pass's layout frames, for `neighbour_travel`: a window leaving for a park or coming
        // back from one moves by the vector of the strip window nearest it, not to the edge.
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
            // Parked slivers are excluded on the way in AND on the way out: a window arriving from
            // off-strip has no visible starting point, and one leaving has no visible destination.
            if !self.is_worth_animating(start, end, display_frame) {
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
            // Anything SkyLight could not serve at full size needs a real capture before it can be
            // animated. Queue it now so the next switch has pixels, even if this one does not.
            if snapshot
                .as_ref()
                .is_none_or(|s| s.source == crate::ui::window_snapshot::SnapshotSource::SkyLight
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
                    // A grow whose picture cannot cover the destination holds for the reveal:
                    // the truthful pixels only exist once the app renders at the new size.
                    if crate::ui::window_snapshot::outgrows(
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
                // No picture at all: almost always a window that just opened, since anything that
                // has ever been on a workspace was warmed. macOS is already showing it at its
                // spawn frame, so it is captured there and its tile travels from that frame to
                // its slot; the chase replaces the stretched picture once the app renders at slot
                // size. With no frame or no usable capture it takes a reservation instead. See "A
                // window that opens travels from its spawn frame" in `docs/animation-smoothness.md`.
                None => {
                    // Only a frame on this display counts as a spawn: a parked window with a
                    // cold cache is not a newcomer, and capturing off screen is slow.
                    let spawn = crate::sys::window_server::get_window(request.server_id)
                        .map(|info| info.frame)
                        .filter(|f| !HiddenWindowPlacement::is_off_screen(display_frame, *f));
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
                            // Already at its slot (a cold cache, not an open): an ordinary still
                            // tile, no chase.
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
        // Stacked here so the companions can anchor to their windows' depths; `begin_group`
        // restacks the whole flight once this pass has merged.
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
        // Remember the real ids so the refresh after this animation, and the one triggered when
        // nothing was drawable, both use keys an animation will actually look up. Companions
        // included: a border recolors when focus moves, so its picture is refreshed whenever its
        // window's is.
        self.last_animated = windows
            .iter()
            .map(|request| SnapshotTarget {
                window: request.window,
                server_id: request.server_id,
                size: request.to.size,
            })
            .chain(companion_targets)
            .collect();

        // A pass where nothing drawable moves has no overlay to hide behind: place the windows at
        // once rather than raising the overlay over them. A flight in progress still merges, so
        // its fresh destinations are not yanked out from under the running overlay.
        let moving_drawable = tiles.iter().any(|tile| is_moving(tile.from, tile.to));
        if !worth_flying(moving_drawable, self.running.is_some()) {
            self.request_frames(final_frames);
            // Warm anyway, or this deadlocks: the cache only ever filled when an animation
            // completed, and no animation could run with an empty cache.
            let targets = std::mem::take(&mut self.last_animated);
            self.warm_windows(targets);
            return;
        }

        // The pass as rigid pieces: grouped by translation vector, with the park rule already
        // folded into `start`/`end`. Ghosts and companions ride by their own vectors.
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

    /// Runs one plan through the shared animation machinery: merge into a flight already running
    /// (`merge_plans`), or dress the overlay and install a fresh one. Every animated movement ends
    /// up here, which is what lets any of them chain onto any other. See "The overlay engine" in
    /// `docs/animation-smoothness.md`.
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
        // Merge FIRST, before the empty check: a pass with nothing drawable can still carry fresh
        // destinations for a flight in progress, and placing its frames immediately would yank
        // real windows out from behind the running overlay.
        //
        // Merge rather than replace: the reactor lays a layout out over several passes, and a
        // later pass can also change where a window is going.
        if self.running.is_some() {
            let in_flight;
            let hold_frames: Option<Vec<(WindowId, CGRect)>>;
            let frames_changed;
            let focus_before: Option<WindowId>;
            let display = self.display.map(|(frame, _)| frame);
            {
                let running = self.running.as_mut().expect("checked above");
                in_flight = running.started.is_some();
                // A resize joining mid-flight needs the earlier apply point just as much, and a
                // window still waiting for its first picture keeps its reservation. Its hold
                // entry rides in `awaiting` like a grow's.
                running.apply_at = running.apply_at.min(apply_at);
                for entrance in entrances {
                    if !running.entrances.iter().any(|e| e.window == entrance.window) {
                        running.entrances.push(entrance);
                    }
                }
                // A reservation still waiting for its picture takes the pass's slot; everything
                // with a tile is the plan's business below.
                if let Some(display) = display {
                    retarget_entrances(&mut running.entrances, &final_frames, display);
                }
                // Merged before the hold reads them, so a held merge re-requests the frames this
                // pass brought, not the ones it replaced.
                frames_changed = merge_final_frames(&mut running.final_frames, final_frames);
                hold_frames = running.extend_hold(&awaiting, in_flight, duration, Instant::now());
                // Merged and restacked before the overlay sees any tile, so the copies handed to
                // it below carry their flight-level depths.
                focus_before = running.focus;
                running.merge_pass(tiles, focus);
            }
            if in_flight {
                // Containers bend from where they are drawn; members changing hands are
                // reparented at their presented frame; a pan carries every group. Nothing is
                // retargeted per tile unless it is loose. See "Mid-flight passes" in
                // `docs/animation-smoothness.md`.
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
                    // A real change restarts the orchestration clock so the frame placement and
                    // the teardown cover the flights that just began; without this the overlay
                    // lifts while a retargeted container is still travelling.
                    running.started = Some(Instant::now());
                    running.duration = duration;
                }
                running.absorb_in_flight_change(changed, frames_changed);
                // A grow joining mid-flight cannot hold, but it can still get its truthful
                // pixels: the chase lands them as `Swap("reveal")` on the placeholder tile, which
                // re-keys the grid from the presented state. A newcomer's slot goes out now so
                // the chase has something to capture.
                let (_, chase_set, _) = frame_zero_work(&awaiting, &chase, &[], &[]);
                if !entrance_frames.is_empty() {
                    self.request_frames(entrance_frames);
                }
                if !chase_set.is_empty() {
                    self.chase_reveal_pictures(&chase_set);
                }
            } else {
                // Still collecting behind the coalesce window: compose statically at frame zero,
                // exactly as a fresh start does. The animations are installed once by
                // `start_moving`.
                self.recompose();
                let reapply = self.running.as_mut().and_then(|running| {
                    // A held merge already carries the merged frames below; one request per pass.
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
                // A newcomer from spawn joining a collecting flight: its slot goes out now and its
                // chase starts, unless a hold below sends every frame anyway.
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

        // The layout path has already decided the pass is worth flying; this guards the strip
        // path, where a pan with no usable picture leaves nothing to draw.
        if tiles.is_empty() {
            self.request_frames(final_frames);
            // Warm anyway, or this deadlocks: the cache only ever filled when an animation completed,
            // and no animation could run with an empty cache.
            let targets = std::mem::take(&mut self.last_animated);
            self.warm_windows(targets);
            return;
        }

        // A flight beginning inside the quiet period keeps the compositor to itself: the owed
        // captures wait for this flight's lift.
        self.quiet = None;
        // The backdrop and bar, or the overlay shows a bare black window behind the tiles. Cheap in
        // the steady state: the cached render is a clone.
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
        // Composed as rigid pieces at frame zero.
        let plan = plan::FlightPlan::from(plan);
        overlay.install(&plan, &tiles, &band_plan(&plan, &tiles, focus));
        // Shown at once, holding the windows exactly where they already are, so the real windows can
        // be placed underneath without the jump being visible.
        overlay.show();

        // Frames come from the run loop. Posting Tick into our own queue keeps every frame on the
        // same path as other events, so there is no second code path to reason about.
        let tx = self.tx.clone();
        let clock = RepeatingTimer::every(FRAME_INTERVAL, move || {
            _ = tx.send(Event::Tick);
        });
        if clock.is_none() {
            warn!("could not start the frame clock; drawing the final frame directly");
        }

        // A holding flight applies the real frames NOW: the overlay is already covering the
        // windows, so the app can rerender at its new size while the tiles stand still — the
        // rerender is exactly what the hold is waiting for. A newcomer's slot goes out now in
        // either case, so its chase captures the window at slot size and the picture fits.
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

        // With no clock the animation would never advance, so land it immediately rather than
        // leaving the overlay up over a frozen picture.
        if self.running.as_ref().is_some_and(|running| running._clock.is_none()) {
            if let Some(running) = self.running.as_mut() {
                running.started = Some(Instant::now());
            }
            self.step_to_end();
            return;
        }

        match start {
            // Strip movements arrive once per keystroke, and chained presses merge through the
            // running-flight path, so there is nothing to coalesce and keypress-to-motion latency
            // is the thing the eye notices most.
            GroupStart::Immediate => self.start_moving(),
            // Layout changes arrive as several passes; start moving once they settle.
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
    fn start_strip(
        &mut self,
        windows: Vec<StripWindow>,
        from_offset: CGPoint,
        to_offset: CGPoint,
        final_frames: Vec<(WindowId, CGRect)>,
        focus: Option<WindowId>,
        duration: Duration,
    ) {
        // Remember these before anything can fail, so a window with no picture is still placed and
        // still queued for a background capture.
        self.last_animated = windows
            .iter()
            .map(|w| SnapshotTarget {
                window: w.window,
                server_id: w.server_id,
                size: w.frame.size,
            })
            .collect();

        let depths = crate::sys::window_server::front_to_back_depths();
        let mut tiles = Vec::with_capacity(windows.len());
        let mut missing = 0usize;
        let mut misshapen = 0usize;
        let mut needs_capture: Vec<SnapshotTarget> = Vec::new();
        // Real frame per drawn window; depths are filled in after the restack.
        let mut starts: Vec<(WindowId, CGRect)> = Vec::new();
        for window in &windows {
            let (from, to) = strip_travel(window.frame, from_offset, to_offset, window.pinned);
            match self.cache.usable(window.window).cloned() {
                Some(snapshot) => {
                    // A picture of the wrong shape is stretched to the frame rather than dropped.
                    // Dropping it left a hole the size of a window in an opaque overlay, so the
                    // window appeared to vanish for the whole animation, which is far worse than
                    // 350ms of a stretched picture. It should be rare: `warm_windows` recaptures
                    // anything whose picture no longer fits.
                    if !snapshot.fits(window.frame.size) {
                        misshapen += 1;
                    }
                    // The border rides only where the window genuinely is: an arriving row's
                    // window sits parked, its real border parked with it, so no companion matches
                    // — matching reality, where the border reappears once its tool catches up.
                    if let Some(info) = crate::sys::window_server::get_window(window.server_id) {
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
                // No usable picture. The window is still placed by final_frames, and warmed once
                // the movement settles.
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
            "strip group animation"
        );

        // One rigid piece for the strip, from the windows that have a picture; companions are
        // adopted by their own vectors. See "Strip movements" in
        // `docs/animation-smoothness.md`.
        let drawn: Vec<StripWindow> = windows
            .iter()
            .filter(|w| tiles.iter().any(|t| t.window == w.window && !t.companion))
            .cloned()
            .collect();
        let mut plan = plan::strip_plan(&drawn, from_offset, to_offset);
        for tile in &tiles {
            if plan.member(tile.window).is_none() {
                plan.adopt(tile);
            }
        }

        // Chaining needs no special handling here: a strip movement arriving while anything is in
        // flight merges through `begin_group`, and each tile bends from its PRESENTATION position
        // toward its new destination.
        // A strip movement never resizes and never carries a brand-new window: entrances and the
        // early apply point are the layout path's concerns.
        self.begin_group(
            tiles,
            final_frames,
            duration,
            "strip",
            GroupStart::Immediate,
            apply_frames_at(FlightKind::Strip, false),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            focus,
            Some(strip_pan_travel(from_offset, to_offset)),
            plan,
        );
    }

    /// Nudges the strip surface by `overshoot` and back. With a flight in progress the bounce is
    /// added to it: an additive animation on every container, and the clock extended so the lift
    /// waits for the return. Otherwise a flight with no travel is composed from `windows` (every
    /// tile at rest) and the bounce is its only motion. See "Edge bounce" in
    /// `docs/animation-smoothness.md`.
    fn start_bounce(
        &mut self,
        windows: Vec<StripWindow>,
        overshoot: CGPoint,
        final_frames: Vec<(WindowId, CGRect)>,
        focus: Option<WindowId>,
        duration: Duration,
    ) {
        if self.running.is_none() {
            let at_rest = CGPoint::new(0.0, 0.0);
            self.start_strip(windows, at_rest, at_rest, final_frames, focus, duration);
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

    /// Starts an animation that is on screen but not yet moving: the clock, and the movements.
    ///
    /// This is the moment the tiles are handed to Core Animation, all in one transaction, so the
    /// passes collected behind the coalesce window travel as one group from one beat.
    fn start_moving(&mut self) {
        // Dropping the timer stops it repeating; it only ever needed to fire once.
        self.coalesce = None;
        // A grow holds at frame zero until its reveal pixels land or the deadline passes: flying
        // without them draws the old picture stretched (`placeholder_mode`) until the chase lands
        // it early or caches it. The nudge timer re-fires StartMoving at the deadline, so a slow
        // app costs the capped hold and nothing more.
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
            // Nothing is drawn here; Core Animation carries the tiles (see `animate_tiles`). The
            // tick only paces the mid-flight work, so a late tick delays a recapture or the frame
            // placement, never the motion.
            let place_now = running.frames_due(progress);
            let refresh_now = running.take_refresh(progress);
            (running.is_done(), place_now, refresh_now)
        };
        // The render server runs a frame or so behind the actor's clock: lifting on the clock alone
        // showed the real windows one frame ahead of their tiles, a jerk at the end of every flight.
        let clock_done = done;
        let mut done = false;
        if clock_done {
            let settled = self.overlay.as_ref().is_none_or(WorkspaceOverlay::settled);
            let handover = self.handover();
            let landed = handover.as_ref().is_none_or(|report| report.count_over == 0);
            let overdue = self.running.as_ref().is_some_and(RunningAnimation::overdue);
            done = lift_now(clock_done, settled, landed, overdue);
            if done && let Some(running) = self.running.as_ref() {
                // The acceptance metric for the end of a flight: how long past its clock the lift
                // waited for the render server and the real windows. `landed=false` or
                // `settled=false` means the grace ran out.
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
                        // A border companion must not claim the one mid-flight recapture: the
                        // window being switched into is what the eye is on.
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
    /// Parks are excluded (macOS clamps them). This is the handover shift, measured rather than
    /// eyeballed. See "Real windows land before lift" in `docs/animation-smoothness.md`.
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
                let info = crate::sys::window_server::get_window(
                    crate::sys::window_server::WindowServerId::new(window.idx.get()),
                )?;
                Some((*window, info.frame))
            })
            .collect();
        Some(handover_report(&running.final_frames, &tiled, &real, display_frame))
    }

    /// Asks for a fresh desktop render in the background, or defers it to `finish` while a flight
    /// is up (see "Capture work in flight" in `docs/animation-smoothness.md`).
    ///
    /// Cheap to call: the service ignores the request when one is already in flight, and the desktop
    /// changes rarely enough that a capture a few seconds old is indistinguishable from a fresh one.
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

    /// The desktop to draw behind the moving strips.
    ///
    /// Prefers the SkyLight composite, which is cheap enough to capture here, but only when it actually
    /// contains the wallpaper. Otherwise falls back to the cached ScreenCaptureKit render, which always
    /// does. See "The wallpaper is not reliably a window" in `docs/capture-overlay-research.md`.
    fn capture_backdrop(&mut self) -> Option<WindowSnapshot> {
        let (display_frame, scale) = self.display?;
        let display_size = (display_frame.size.width, display_frame.size.height);

        // A render asked for here lands mid-flight, so it is asked for at `finish` instead and
        // this switch draws what is in hand. See "Capture work in flight" in the doc.
        let in_hand = self
            .pictures
            .desktop
            .as_ref()
            .map(|render| (render.taken.elapsed(), render.coverage.covered));
        if desktop_render_wanted(in_hand, display_size) {
            self.deferred_desktop = true;
        }

        // The ScreenCaptureKit render first, because it is the compositor's own output and therefore matches
        // the real desktop exactly. Measured against the SkyLight composite of the same desktop: identical
        // everywhere below the top band, and up to 26 of 255 different inside it, where the widgets' and the
        // menu bar's vibrancy live. That band shows through the bar, so a composite there flickers every
        // time the overlay appears.
        //
        // Size-checked: a render requested while the overlay was on the other display can land afterwards,
        // and drawing it sizes the backdrop layer to ITS size, which showed the external display's wallpaper
        // zoomed into the built-in display's overlay.
        if let Some(rendered) = self.pictures.desktop.clone().filter(|rendered| {
            crate::ui::window_snapshot::spans_display(rendered.coverage.covered, display_size)
        }) {
            self.pictures.drawn_once = true;
            return Some(rendered);
        }

        // No render yet, which is the first switch after starting or after moving to another display. A
        // composite of the desktop's own windows is right everywhere except that top band, and it is
        // available synchronously, so it covers the gap rather than leaving the overlay black.
        let desktop = crate::sys::window_server::desktop_backdrop_windows(display_frame);
        let composite = crate::ui::window_snapshot::capture_composite_via_skylight(
            &desktop.windows,
            display_size,
            scale,
        );
        let usable = composite.filter(|snapshot| {
            crate::ui::window_snapshot::is_backdrop_worth_drawing(
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

    /// Records what the overlay was dressed with. Kept because the backdrop going black is only ever
    /// diagnosable after the fact: it depends on which capture route served the desktop and what size it
    /// covered, neither of which can be recovered from a screenshot.
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

    /// The bar's picture to draw for this animation, and where it sits in the overlay's coordinates.
    ///
    /// Held rather than captured here: a bar composite measures 31ms median, which is two frames on the
    /// main thread before the overlay can even be shown, and the per-window path runs on every window
    /// focus change. [`Self::refresh_bar`] pays it after an animation instead. Only the very first one
    /// captures inline, since the alternative is a switch with no bar at all.
    fn bar_picture(&mut self) -> (Option<WindowSnapshot>, Option<CGRect>) {
        let Some((display_frame, _)) = self.display else { return (None, None) };
        let strip = crate::sys::window_server::bar_strip(display_frame);
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

    /// Asks for the bar to be recaptured once things have settled.
    ///
    /// Not straight after the overlay hides: the alpha change is applied by the compositor, so a capture
    /// taken in the same breath still reads the overlay's own pixels back out of the framebuffer. The
    /// delay also means a burst of switches captures once, at the end, rather than between each pair.
    fn arm_bar_refresh(&mut self) {
        let tx = self.tx.clone();
        self.bar_refresh = RepeatingTimer::every(BAR_REFRESH_DELAY, move || {
            _ = tx.send(Event::RefreshBar);
        });
    }

    /// Recaptures the bar, for the next animation to draw.
    ///
    /// Captured on its own rather than lifted out of the desktop picture, because the bar's translucency
    /// is per-pixel alpha, measured at 224 of 255, and a bar-only capture keeps it. The strips then show
    /// through the bar as they scroll under it, which a flattened bar-over-desktop could not do: that
    /// covered them at the bar's edge.
    ///
    /// SkyLight reads the framebuffer, so this is a no-op while the overlay is on top of the bar. The
    /// previous picture is kept in that case, and one that comes back the wrong size for the strip is
    /// rejected the same way a window's is.
    fn refresh_bar(&mut self) {
        let Some((display_frame, scale)) = self.display else { return };
        if self.overlay.as_ref().is_some_and(WorkspaceOverlay::is_visible) {
            return;
        }
        let strip = crate::sys::window_server::bar_strip(display_frame);
        let Some(bounds) = strip.bounds else { return };
        let fresh = crate::ui::window_snapshot::capture_composite_via_skylight(
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
        let Some(tx) = &self.reactor_tx else {
            warn!("no reactor channel; cannot place windows at their final frames");
            return;
        };
        debug!(count = frames.len(), "placing real windows behind the overlay");
        _ = tx.send(crate::actor::reactor::Event::ApplyOverlayFrames(frames));
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
            // Free the tile contents rather than hold window pictures that are no longer drawn.
            overlay.release_tiles();
        }
        // Dropping the animation drops its timer, which stops the wakeups.
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

        // The captures this flight owes run once the user has stopped for `SETTLE_BEFORE_CAPTURES`;
        // a flight beginning first cancels the timer and the work carries over to its own lift.
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

    /// The capture work the last flights left: the animated set's warm and hairlines, the warms
    /// deferred in flight, the desktop render. Event-driven, once per quiet period, never on a
    /// repeating timer.
    fn after_flight_captures(&mut self) {
        self.quiet = None;
        if self.running.is_some() {
            return;
        }
        let Some(after) = self.after_flight.take() else { return };
        let animated: Vec<WindowId> = after.targets.iter().map(|target| target.window).collect();
        let requested =
            if after.targets.is_empty() { Vec::new() } else { self.warm_windows(after.targets) };
        // The animated set's hairlines, once: a re-warmed window is harvested when its picture
        // lands, a chased or refreshed one already was, and a dressed one keeps its ring.
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

    /// Writes every cached picture to `<temp dir>/rini-snapshots/<pid>-<idx>.ppm`, so a picture
    /// can be checked against the window it is keyed to. Debug command only.
    fn dump_cache_for_inspection(&self) {
        let dir = std::env::temp_dir().join("rini-snapshots");
        if let Err(error) = std::fs::create_dir_all(&dir) {
            warn!(?error, ?dir, "cannot create the snapshot dump directory");
            return;
        }
        let mut written = 0usize;
        for (window, snapshot) in self.cache.iter() {
            let Some((w, h, rgb)) = snapshot_rgb(snapshot) else { continue };
            let path = dir.join(format!("{}-{}.ppm", window.pid, window.idx.get()));
            let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
            bytes.extend(rgb);
            if std::fs::write(&path, bytes).is_ok() {
                written += 1;
            }
        }
        if let Some((w, h, rgb)) = self.pictures.shown.as_ref().and_then(snapshot_rgb) {
            let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
            bytes.extend(rgb);
            let _ = std::fs::write(dir.join("desktop.ppm"), bytes);
        }
        debug!(written, ?dir, "cache dumped for inspection");
    }

    /// Slides every window currently on screen in from an offset. For judging animation quality by
    /// eye without touching a single real window, so it can be run at any time without risk.
    fn debug_slide(&mut self, dx: f64, dy: f64, duration: Duration) {
        self.dump_cache_for_inspection();
        let Some((display_frame, _)) = self.display else {
            warn!("no display geometry yet; cannot run the debug slide");
            return;
        };
        let windows = crate::sys::window_server::visible_windows_on_display(display_frame);
        if windows.is_empty() {
            warn!("no visible windows found for the debug slide");
            return;
        }
        let requests: Vec<AnimationRequest> = windows
            .into_iter()
            .map(|(server_id, frame)| AnimationRequest {
                window: synthetic_window_id(server_id),
                server_id,
                // The debug slide works from the window server and knows nothing about the layout, so
                // everything it finds is treated as being on the strip.
                floating: false,
                from: CGRect::new(
                    CGPoint::new(frame.origin.x + dx, frame.origin.y + dy),
                    frame.size,
                ),
                to: frame,
            })
            .collect();
        debug!(count = requests.len(), dx, dy, "running debug slide");
        // No focus target: the debug slide moves everything and changes nothing about focus.
        self.start(requests, None, duration);
    }
}

/// What fraction of `frame`'s area lies inside `display`.
pub(crate) fn on_screen_fraction(frame: CGRect, display: CGRect) -> f64 {
    let area = frame.size.width * frame.size.height;
    if area <= 0.0 {
        return 0.0;
    }
    let overlap_w = (frame.origin.x + frame.size.width).min(display.origin.x + display.size.width)
        - frame.origin.x.max(display.origin.x);
    let overlap_h = (frame.origin.y + frame.size.height).min(display.origin.y + display.size.height)
        - frame.origin.y.max(display.origin.y);
    if overlap_w <= 0.0 || overlap_h <= 0.0 {
        return 0.0;
    }
    (overlap_w * overlap_h) / area
}

/// Where a window really is right now, preferring the window server over the caller's idea of it.
///
/// The reactor arranges a layout over several passes and marks each window as being at its target as
/// soon as it schedules it, so on a later pass the frame it reports as current is the previous pass's
/// DESTINATION rather than where the window actually sits. A tile built from that starts in the wrong
/// place, which reads as the animation being misaligned with the real windows.
///
/// The window server always knows the truth, and asking it is a read rather than a round trip into
/// the owning application.
fn actual_start(request: &AnimationRequest, display: CGRect, travel: Option<CGPoint>) -> CGRect {
    let real = match crate::sys::window_server::get_window(request.server_id) {
        Some(info) if info.frame.size.width > 0.0 && info.frame.size.height > 0.0 => {
            Some(info.frame)
        }
        _ => None,
    };
    resolve_start(real, request.from, request.to, display, travel)
}

/// The tile's start from the window server's answer (`real`, `None` when it had none) and the
/// request's `from`/`to`. Pure, so the park remap can be tested on plain rects.
///
/// A parked window comes back along the strip's own movement: `to` translated back by
/// `travel`, the vector its neighbours make this pass (`neighbour_travel`). Without a moving
/// neighbour it enters from the display edge on the park's side (`entry_frame`).
fn resolve_start(
    real: Option<CGRect>,
    from: CGRect,
    to: CGRect,
    display: CGRect,
    travel: Option<CGPoint>,
) -> CGRect {
    use crate::model::HiddenWindowPlacement as Park;
    // A park is judged from both frames, before the synthetic test: apps clamp the real frame past
    // the park threshold, and the server may already report the slot. See docs/animation-smoothness.md.
    let parked_real = real.is_some_and(|real| Park::is_off_screen(display, real));
    let parked_from = Park::is_off_screen(display, from);
    if parked_real || parked_from {
        return match travel {
            Some(d) => translated(to, CGPoint::new(-d.x, -d.y)),
            None => {
                let park = if parked_real { real.unwrap_or(from) } else { from };
                Park::entry_frame(park, to, display)
            }
        };
    }
    let real = real.unwrap_or(from);
    if start_is_synthetic(real, from, to) {
        return from;
    }
    real
}

/// The tile's visual destination: a window leaving for a corner park travels with the strip,
/// `start` translated by `travel` (`neighbour_travel`), the mirror of `resolve_start`. With no
/// moving neighbour it exits past the display edge on the park's side. See "Layout changes" in
/// `docs/animation-smoothness.md`.
fn resolve_end(start: CGRect, to: CGRect, display: CGRect, travel: Option<CGPoint>) -> CGRect {
    use crate::model::HiddenWindowPlacement as Park;
    if Park::is_off_screen(display, to) && !Park::is_off_screen(display, start) {
        return match travel {
            Some(d) => translated(start, d),
            None => Park::entry_frame(to, start, display),
        };
    }
    to
}

/// The vector the rigid strip moves by this pass, as the window at `subject` sees it: the
/// `to - from` of the nearest (by centre x of `from`) strip window that is on screen at both
/// ends and actually moves. `None` when no such neighbour exists, which is the edge fallback's
/// cue. `others` is `(from, to, floating)` for every OTHER request of the pass.
///
/// A displaced window aimed at the display edge covered a different distance from the window
/// beside it under one duration and one curve, so the two ran at different speeds and
/// overlapped (seen 2026-09-15). Sharing the neighbour's vector is what makes them one body.
fn neighbour_travel(
    subject: CGRect,
    others: &[(CGRect, CGRect, bool)],
    display: CGRect,
) -> Option<CGPoint> {
    use crate::model::HiddenWindowPlacement as Park;
    others
        .iter()
        .filter(|(_, _, floating)| !floating)
        .filter(|(from, to, _)| {
            !Park::is_off_screen(display, *from) && !Park::is_off_screen(display, *to)
        })
        // Moving by origin, not `is_moving`: a neighbour that only resizes has no travel to lend.
        .filter(|(from, to, _)| {
            (to.origin.x - from.origin.x).abs() >= 0.5 || (to.origin.y - from.origin.y).abs() >= 0.5
        })
        .min_by(|(a, _, _), (b, _, _)| {
            let da = (a.mid().x - subject.mid().x).abs();
            let db = (b.mid().x - subject.mid().x).abs();
            da.total_cmp(&db)
        })
        .map(|(from, to, _)| CGPoint::new(to.origin.x - from.origin.x, to.origin.y - from.origin.y))
}

/// `frame` moved by `by`, same size.
fn translated(frame: CGRect, by: CGPoint) -> CGRect {
    CGRect::new(CGPoint::new(frame.origin.x + by.x, frame.origin.y + by.y), frame.size)
}

/// The subject frame `neighbour_travel` measures from for one request: the slot it leaves when
/// its destination is a park, otherwise the slot it arrives at.
fn travel_subject(from: CGRect, to: CGRect, display: CGRect) -> CGRect {
    if crate::model::HiddenWindowPlacement::is_off_screen(display, to) { from } else { to }
}

/// Is a request's start a deliberate fiction rather than drift to correct?
///
/// A window already sitting at its destination has no drift, so a request that still asks for
/// motion can only be a synthetic start (the debug slide, which invents one). Overriding it with
/// the real frame made `from` equal `to`, which silently killed the whole movement.
fn start_is_synthetic(real: CGRect, from: CGRect, to: CGRect) -> bool {
    real.same_as(to) && !from.same_as(to)
}

/// How much of a window has to be on screen for the overlay to bother with it.
///
/// A window being moved needs a real share, or every parked sliver becomes a tile. A window standing still
/// needs only to be visible at all, because whatever shows of it turns into wallpaper otherwise.
fn min_on_screen(moving: bool) -> f64 {
    if moving { 0.25 } else { f64::MIN_POSITIVE }
}

/// The smallest visible extent that still reads as a window rather than a sliver.
///
/// The share test alone starved wide windows: a 1720pt window showing 400pt is under a quarter by
/// area yet is exactly the "column peeking in" a scrolling layout is made of. Anything showing at
/// least this much in both axes is drawn.
const MIN_VISIBLE_EXTENT: f64 = 80.0;

/// How much of `frame` shows on `display`, as the overlap's width and height.
fn on_screen_extent(frame: CGRect, display: CGRect) -> (f64, f64) {
    let w = (frame.origin.x + frame.size.width).min(display.origin.x + display.size.width)
        - frame.origin.x.max(display.origin.x);
    let h = (frame.origin.y + frame.size.height).min(display.origin.y + display.size.height)
        - frame.origin.y.max(display.origin.y);
    (w.max(0.0), h.max(0.0))
}

/// Does a window travelling `from` → `to` appear on `display` at ANY point? The whole path is
/// sampled, not just its ends: a window sweeping across mid-animation is exactly what conveys how
/// far the strip travelled, and testing endpoints alone excluded it.
fn worth_animating(from: CGRect, to: CGRect, display: CGRect) -> bool {
    /// Enough that a window cannot cross the display between two samples: the fastest realistic
    /// travel is a few display widths.
    const SAMPLES: usize = 11;

    let area = from.size.width * from.size.height;
    if area <= 0.0 {
        return false;
    }
    let moving = is_moving(from, to);
    (0..SAMPLES).any(|step| {
        let t = step as f64 / (SAMPLES - 1) as f64;
        let at = crate::ui::workspace_overlay::lerp_rect(from, to, t);
        shows_enough(at, display, moving)
    })
}

/// Whether enough of the window shows at `at` for a tile to be worth drawing there.
fn shows_enough(at: CGRect, display: CGRect, moving: bool) -> bool {
    if on_screen_fraction(at, display) >= min_on_screen(moving) {
        return true;
    }
    let (w, h) = on_screen_extent(at, display);
    moving && w.min(h) >= MIN_VISIBLE_EXTENT
}

/// Whether a request actually moves its window.
///
/// Requests with the same start and end are there to be drawn, not moved. Half a point, because the layout
/// rounds to whole points.
fn is_moving(from: CGRect, to: CGRect) -> bool {
    (to.origin.x - from.origin.x).abs() >= 0.5
        || (to.origin.y - from.origin.y).abs() >= 0.5
        || (to.size.width - from.size.width).abs() >= 0.5
        || (to.size.height - from.size.height).abs() >= 0.5
}

/// A stable [`WindowId`] derived from a window server id.
///
/// The debug paths work from the window server rather than from rini's own window table, so they need
/// a key that is consistent between capturing and drawing. Using pid 0 keeps these clear of real
/// window ids, which always carry a real pid.
fn synthetic_window_id(server_id: WindowServerId) -> WindowId {
    WindowId { pid: 0, idx: std::num::NonZeroU32::new(server_id.as_u32().max(1)).unwrap() }
}

/// A snapshot's pixels as packed RGB, downsampled by four in each axis, for the debug dump.
fn snapshot_rgb(snapshot: &WindowSnapshot) -> Option<(usize, usize, Vec<u8>)> {
    use crate::ui::window_snapshot::SnapshotImage;
    let step = 4usize;
    let mut sample = |width: usize, height: usize, stride: usize, base: *const u8| {
        let (w, h) = (width / step, height / step);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                // BGRA, premultiplied: blue, green, red.
                let px = unsafe { base.add(y * step * stride + x * step * 4) };
                unsafe { rgb.extend_from_slice(&[*px.add(2), *px.add(1), *px]) };
            }
        }
        (w, h, rgb)
    };
    match &snapshot.image {
        SnapshotImage::Surface(surface) => {
            use objc2_io_surface::IOSurfaceLockOptions;
            if unsafe { surface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) } != 0 {
                return None;
            }
            let out = sample(
                surface.width(),
                surface.height(),
                surface.bytes_per_row(),
                surface.base_address().as_ptr() as *const u8,
            );
            unsafe { surface.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
            Some(out)
        }
        SnapshotImage::Bitmap(image) => {
            use objc2_core_graphics::{CGDataProvider, CGImage};
            let provider = CGImage::data_provider(Some(image))?;
            let data = CGDataProvider::data(Some(&provider))?;
            // SAFETY: the data is immutable and outlives this read.
            let bytes = unsafe { data.as_bytes_unchecked() }.to_vec();
            let (w, h) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
            let stride = CGImage::bytes_per_row(Some(image));
            if bytes.len() < stride * h {
                return None;
            }
            Some(sample(w, h, stride, bytes.as_ptr()))
        }
    }
}

/// Converts a display-space rect into the overlay's own coordinate space.
///
/// The overlay's layer tree has its origin at the overlay's top-left, not the display's, so a window
/// frame has to have the overlay's origin subtracted. Skipping this puts every tile off by the menu
/// bar inset, which reads as the whole animation being shifted down.
pub fn to_overlay_space(frame: CGRect, overlay_frame: CGRect) -> CGRect {
    CGRect::new(
        CGPoint::new(
            frame.origin.x - overlay_frame.origin.x,
            frame.origin.y - overlay_frame.origin.y,
        ),
        frame.size,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in display, for tests that need a screen to judge parks against.
    const DISPLAY: CGRect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize { width: 1728.0, height: 1117.0 },
    };

    /// The rigidity property the old canvas layer guaranteed structurally: every unpinned window
    /// of a strip movement translates by exactly the same vector, so tiles animated per-tile from
    /// these rects cannot drift apart. If this ever fails, the strip tears.
    #[test]
    fn a_strip_movement_translates_every_window_by_the_same_vector() {
        let from_offset = CGPoint::new(0.0, 0.0);
        let to_offset = CGPoint::new(-861.0, 1117.0);
        let frames = [
            rect(4.0, 32.0, 859.0, 1081.0),
            rect(867.0, 32.0, 859.0, 1081.0),
            rect(4.0, 1149.0, 1720.0, 1081.0), // the row below, mid-jump
        ];
        for frame in frames {
            let (from, to) = strip_travel(frame, from_offset, to_offset, false);
            assert_eq!(from, frame, "at rest the viewport offset is zero");
            assert_eq!(to.origin.x - from.origin.x, 861.0);
            assert_eq!(to.origin.y - from.origin.y, -1117.0);
            assert_eq!(to.size, frame.size, "a strip movement never resizes");
        }
    }

    /// The debug slide invents a start for a window already at rest; correcting that "drift" from
    /// the window server made from equal to and silently killed the whole movement.
    #[test]
    fn an_invented_start_for_a_window_at_rest_is_honoured() {
        let at_rest = rect(4.0, 32.0, 859.0, 1081.0);
        let offset = rect(-396.0, 32.0, 859.0, 1081.0);
        assert!(start_is_synthetic(at_rest, offset, at_rest));
    }

    /// The case `actual_start` exists for: the reactor reports a window at its DESTINATION while
    /// it really sits elsewhere. The real frame differs from the destination, so it is drift, and
    /// the window server's answer must win.
    #[test]
    fn real_drift_is_not_synthetic() {
        let reported = rect(4.0, 32.0, 859.0, 1081.0);
        let destination = rect(867.0, 32.0, 859.0, 1081.0);
        let really_at = rect(400.0, 32.0, 859.0, 1081.0);
        assert!(!start_is_synthetic(really_at, reported, destination));
    }

    /// A request with no motion at all has nothing to honour either way.
    #[test]
    fn a_standing_request_is_not_synthetic() {
        let frame = rect(4.0, 32.0, 859.0, 1081.0);
        assert!(!start_is_synthetic(frame, frame, frame));
    }

    /// A resize behind the overlay costs three synchronous round trips into the owning app, so it
    /// gets more runway before the overlay lifts; a plain move keeps the late point that hides the
    /// real windows longer.
    #[test]
    fn a_resize_places_the_real_windows_earlier() {
        assert!(apply_frames_at(FlightKind::Layout, true) < apply_frames_at(FlightKind::Layout, false));
        assert_eq!(apply_frames_at(FlightKind::Layout, false), APPLY_FRAMES_AT);
        assert_eq!(apply_frames_at(FlightKind::Layout, true), APPLY_FRAMES_AT_RESIZE);
    }

    /// An entering window is a resize from zero to its final width: full height, anchored at its
    /// own left edge, revealing rightward — not a centred zoom, which nothing else on the strip
    /// does.
    #[test]
    fn an_entrance_is_a_resize_from_zero_width() {
        let to = rect(100.0, 32.0, 859.0, 1081.0);
        let from = entrance_from(to);
        assert_eq!(from.origin.x, 100.0);
        assert_eq!(from.origin.y, 32.0);
        assert_eq!(from.size.width, 0.0);
        assert_eq!(from.size.height, 1081.0);
    }

    /// The measured miss: a 1720pt Kiro column at x=1439 on a 1728pt display shows 289pt of real
    /// content but only 17% of its area, so the fraction rule read it as a parked sliver and the
    /// overlay painted desktop over it for the length of the animation.
    #[test]
    fn a_wide_window_with_a_real_share_visible_is_drawn() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let kiro = rect(1439.0, 32.0, 1720.0, 1081.0);
        assert!(shows_enough(kiro, display, true));
    }

    /// A park as the layout asks for it shows 40pt at most, and must stay skipped or every park
    /// becomes a tile. Apps clamp the real frame past that; `resolve_start` handles those.
    #[test]
    fn a_parked_sliver_is_still_skipped() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let parked = rect(1688.0, 32.0, 859.0, 1081.0);
        assert!(!shows_enough(parked, display, true));
        // Standing still, even a sliver is drawn: whatever shows of it turns into wallpaper
        // otherwise.
        assert!(shows_enough(parked, display, false));
    }

    /// A floating window does not belong to the strip: a pan leaves it exactly where it stands,
    /// and a standing tile still exists, because the overlay is opaque and omissions vanish.
    #[test]
    fn a_pinned_window_stands_still() {
        let frame = rect(224.0, 95.0, 1280.0, 960.0);
        let (from, to) = strip_travel(frame, CGPoint::new(100.0, 0.0), CGPoint::new(-4000.0, 0.0), true);
        assert_eq!(from, frame);
        assert_eq!(to, frame);
    }

    /// JankyBorders geometry, from the user's bordersrc: width 1.5, style square, drawn on a
    /// sibling window a few points larger and concentric. That window is the companion; anything
    /// bigger, smaller, or off-center is not.
    #[test]
    fn a_border_window_tracing_a_window_is_its_companion() {
        let window = rect(4.0, 32.0, 859.0, 1081.0);
        let border = (WindowServerId::new(9001), rect(1.0, 29.0, 865.0, 1087.0));
        let neighbor = (WindowServerId::new(9002), rect(867.0, 32.0, 859.0, 1081.0));
        let zoom = (WindowServerId::new(9003), rect(224.0, 95.0, 1280.0, 960.0));
        let found = companion_of(window, &[neighbor, zoom, border], DISPLAY);
        assert_eq!(found.map(|(id, _)| id.as_u32()), Some(9001));
    }

    /// An identical frame also traces (a tool drawing its stroke inward), but a window merely
    /// overlapping, or one much larger, must never be mistaken for a border.
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
    /// Two Chrome windows on different workspaces, both parked at the same frame, which macOS
    /// clamps to 41pt visible (past `is_off_screen`): the one outside the pass is a managed window
    /// rini has a picture of, so it is no candidate. A synthetic companion id (pid 0) stays one.
    #[test]
    fn a_managed_window_is_never_a_border_candidate() {
        let pass: std::collections::HashSet<u32> = [102698].into_iter().collect();
        let cached = [WindowId::new(82799, 102682), WindowId::new(0, 9001)];
        let owed = [WindowId::new(872, 51462)];
        let managed = managed_server_ids(&pass, cached.into_iter(), owed.into_iter());
        assert!(managed.contains(&102698) && managed.contains(&102682) && managed.contains(&51462));
        assert!(!managed.contains(&9001), "a border seen before is still a border");
        // The clamped park itself is not judged off screen, which is why geometry alone failed.
        let park = rect(1727.0, 1076.0, 1720.0, 1081.0);
        assert!(!HiddenWindowPlacement::is_off_screen(DISPLAY, park));
        assert!(companion_of(park, &[(WindowServerId::new(102682), park)], DISPLAY).is_some());
    }

    /// Every parked window shares the park's frame, so a window arriving from the park matched
    /// another parked window as its "border" and flew in wearing that window's picture. A parked
    /// anchor traces nothing, and a parked candidate is never a border.
    #[test]
    fn a_parked_window_neither_traces_nor_is_traced() {
        let park = rect(DISPLAY.size.width - 1.0, DISPLAY.size.height - 1.0, 859.0, 1081.0);
        assert!(HiddenWindowPlacement::is_off_screen(DISPLAY, park));
        let twin = (WindowServerId::new(7), park);
        assert!(companion_of(park, &[twin], DISPLAY).is_none(), "a parked anchor");
        let on_screen = rect(4.0, 32.0, 859.0, 1081.0);
        let border = (WindowServerId::new(8), rect(1.0, 29.0, 865.0, 1087.0));
        assert!(companion_of(on_screen, &[twin, border], DISPLAY).map(|(id, _)| id.as_u32()) == Some(8));
    }

    /// The stability property under rapid presses: a later layout pass confirming a destination
    /// the flight already has must change NOTHING — no retarget, no clock restart — or chained
    /// presses hold the overlay up and re-ease tiles forever. The 0.1pt tolerance is `same_as`'s,
    /// because the layout recomputes destinations bit-for-bit only most of the time.
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

    /// Moving the overlay to another display invalidates every picture it holds, not just the bar. Clearing
    /// them one field at a time is what left an external display's desktop behind a built-in display's
    /// strips, so they go as a unit.
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

        /// A request whose start and end match is in the list to be drawn, not to be moved. Placing it
        /// would be an Accessibility round trip that asks an application to put a window where it
        /// already is, and every such write invites another layout pass.
        #[test]
        fn a_window_that_is_not_going_anywhere_is_not_moving() {
            let frame = CGRect::new(CGPoint::new(502.0, 135.0), CGSize::new(723.0, 879.0));
            assert!(!is_moving(frame, frame));
        }

        /// A window standing still with a sliver on screen still has to be drawn: whatever shows of it
        /// would otherwise be replaced by wallpaper for the length of the animation. A window being moved
        /// needs a real share of the display, or every parked sliver ends up as a tile.
        #[test]
        fn a_still_window_earns_its_tile_with_any_part_on_screen() {
            assert!(min_on_screen(false) < min_on_screen(true));
            assert!(min_on_screen(false) > 0.0, "entirely off screen is still not worth drawing");
            assert_eq!(min_on_screen(true), 0.25);
        }

        #[test]
        fn a_window_that_changes_position_or_size_is_moving() {
            let frame = CGRect::new(CGPoint::new(4.0, 32.0), CGSize::new(859.0, 1081.0));
            let moved = CGRect::new(CGPoint::new(865.0, 32.0), CGSize::new(859.0, 1081.0));
            let lowered = CGRect::new(CGPoint::new(4.0, 1149.0), CGSize::new(859.0, 1081.0));
            let widened = CGRect::new(CGPoint::new(4.0, 32.0), CGSize::new(1720.0, 1081.0));
            assert!(is_moving(frame, moved));
            assert!(is_moving(frame, lowered));
            assert!(is_moving(frame, widened));
        }

        /// The mid-flight recapture is for a focus change and nothing else. Recapturing whatever was
        /// frontmost cut a translucent window's tile on every flight: its two captures differ by
        /// the wallpaper behind it (2026-09-16 2:05, two Ghostty tiles on every strip pan).
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

        /// `refresh_destination_among` end to end minus the service call: a strip pan that leaves
        /// focus where it was requests no capture at all; a focus change requests exactly the
        /// two ends, one ScreenCaptureKit target each.
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

    /// Bug-condition exploration for the regressions from 5877636. Each test names the clause in
    /// `.kiro/specs/exit-entrance-animation-regressions/bugfix.md` it pins. Written to fail on
    /// unfixed code; a failure here is the defect, reproduced.
    mod exploration {
        use super::*;
        use crate::model::HiddenWindowPlacement;
        use crate::ui::window_snapshot::test_snapshot;

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

        /// T1 (1.7). Frames applied at frame zero, then a coalescing pass moves the window: the
        /// merged frame must be sent again or the real window stays where pass 1 put it.
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

        /// T2 (1.4). A picture landing at 60% of the flight must not travel longer than the
        /// overlay stays up, or the cut lands mid-growth.
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

        /// T4 (1.1): pass 1 (a close, focus not among the tiles), then pass 2 (a pan, focus on
        /// the floating window) retargets one strip tile. Depth is banded from the flight's
        /// latest focus for EVERY tile, the redundant ones included: the floating window is never
        /// between two strip tiles. Before the restack, pass 1 banded the strip in front and
        /// pass 2 put only the retargeted tile in the floating band.
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

        /// T5 (1.2, 1.3). Parks the apps clamped past 40pt (Kiro 41pt, Finder 52pt, from the
        /// log) and a park whose window the server already reports at its slot: all must enter
        /// from the edge, never fly in from the bottom corner.
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
                    let expected = HiddenWindowPlacement::entry_frame(*from, *to, display);
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

        /// T7 (1.8). A floating open: one entrance, every drawable tile standing still, no exit,
        /// nothing in flight. There is nothing to animate, so no overlay may go up.
        #[test]
        fn a_pass_where_nothing_drawable_moves_does_not_fly() {
            assert!(
                !worth_flying(false, false),
                "a still-only composition flew, hiding the new window until its picture landed"
            );
        }
    }

    /// Bug-condition exploration for `.kiro/specs/flight-render-stability/bugfix.md`. Each test
    /// names the clause it pins and asserts the FIXED expectation, so it fails on unfixed code; a
    /// failure here is the defect, reproduced.
    mod render_stability_exploration {
        use super::*;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        /// T1 (1.1). A background picture lands on an ordinary moving tile at 40%: nobody asked
        /// for it, so it must go to the cache only. Unfixed: the tile takes it (`Swap`).
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

        /// T2 (1.1, 1.2). The destination refresh lands at 98%, four ticks before lift: too late
        /// to be anything but end-of-flight flicker. Unfixed: `Swap`.
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

        /// T3 (1.2). Warming, the desktop render, a refresh during a hold, and a harvest all
        /// contend with the chase for the window server mid-flight. Unfixed: all allowed.
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

        /// T4 (1.3). A strip switch's frames are pure moves that still take 90ms median to land;
        /// 0.75 of a 300ms flight leaves 75ms. Unfixed: 0.75.
        #[test]
        fn a_strip_movement_applies_frames_by_the_midpoint() {
            let at = apply_frames_at(FlightKind::Strip, false);
            assert!(at <= 0.5, "strip apply point is {at}, leaving too little runway");
        }

        /// T6 (1.3). An in-flight pass that changes only untiled (parked) windows' frames after
        /// the apply point must mark the applied frames stale. Unfixed: only a tile change does.
        #[test]
        fn an_untiled_frame_change_in_flight_marks_frames_stale() {
            assert!(
                mark_stale_on_untiled_change(false, true),
                "frames_applied stays true after an untiled frame change"
            );
        }

        /// T7 (1.3). wsid=108's park at y=1116 on a 1117pt display is clamped by macOS to
        /// y=1051: a 65pt "error" on a window nobody can see, masking a real 3pt miss on the
        /// strip. Unfixed: worst 65, no count.
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

        /// T8 (1.4). A hold is a frozen strip: it must be capped at `HOLD_CAP`, polled every
        /// 8ms, and settle as soon as the print differs from the pre-resize one. Unfixed: 25ms,
        /// two matching prints required. (The cap went 300 -> 150 -> 300: at 150 an entrance's
        /// chase rarely landed in time; see "A grow holds, then reveals" in the doc.)
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

        /// T10 (1.6), inverted. Holding a reserved entrance's frame back made its chase capture
        /// the window at spawn size; drawn over the slot, that picture left a hole (2026-09-15
        /// 3:28:10). So a holding flight sends EVERY frame at frame zero, the newcomer's included,
        /// and the chase then requires the fit like a grow's.
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

    /// Fix checking for `.kiro/specs/flight-render-stability/bugfix.md` 2.x. Change A: a picture
    /// lands on a moving tile only if the tile is waiting for one, or it is the single early
    /// destination refresh.
    mod render_stability_fix {
        use super::preservation::{Gen, RUNS};
        use super::*;
        use crate::ui::window_snapshot::test_snapshot;

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

        /// The rule in 2.1, spelled out independently of the implementation, plus the same-route
        /// gate on the refresh (see "Mid-flight passes" in `docs/animation-smoothness.md`).
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
                // 2.4: a settled reveal landing on the placeholder before 0.6 is hard-swapped,
                // whatever route it came by: the chase's picture is the truth for a grow.
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

        /// 2.1. The full table: every state, settle flag, thumbprint match, route match, and the
        /// progress values on both sides of 0.6, plus `None` for a flight not yet moving.
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
            // fits=true refresh target, not same, same route, {0.3, 0.59}: non-resizing both
            // settle flags, resizing only settled = 3 combinations x 2 progresses; plus the
            // fitting reveal, settled, both `same` flags x both routes x 2 progresses.
            assert_eq!(swaps, 6 + 8, "the table has exactly the early refresh and reveal swaps");
        }

        /// 2.1, 2.4. For random states and progress, `Swap` happens only for the refresh target
        /// or a fitting settled reveal before 0.6, and never for an ordinary moving tile. Seed
        /// 95, 200 runs.
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

        /// The refresh ping-pong (log 22:34:04: three `reason="refresh"` swaps in one strip pan,
        /// alternating between the ScreenCaptureKit and framed routes). A picture from another
        /// route differs by route alone, so it never reaches the tile, whatever the settle flag or
        /// the fit.
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

        /// The refresh's real job: the same route, rendering differently (the focus ring landed),
        /// early. That still swaps.
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

        /// Property: `Swap("refresh")` implies the picture came by the cached picture's route.
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

        /// The refresh asks one route: exactly one ScreenCaptureKit target per wanted window the
        /// pass knows, in the wanted order, and `refresh_targets` names exactly those windows.
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

        /// 2.1, 3.6. One refresh per flight, at 0.5; nothing at 0.0, holding or moving.
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

        /// 2.1. `tile_state` names what a landing finds: a hold or entrance first, then the
        /// refresh target, then a plain tile, then nothing.
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

        // Change B: no capture work between frame zero and lift.

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

        /// The rule in 2.2, spelled out independently of the implementation: idle does anything;
        /// frame zero composes (chase, first captures); a hold only chases; a flight in motion
        /// chases and takes its one refresh.
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

        /// 2.2. For random phases and kinds, work between frame zero and lift is a chase or the
        /// moving refresh, nothing else. Seed 96, 200 runs.
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

        /// 2.2, 3.4. A warm asked for mid-flight is parked once per window, the newest size
        /// winning, and a drain hands the parked set over once.
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

        /// 2.2. The desktop render is re-asked for at `finish` only when the one in hand cannot
        /// back the next overlay: missing, another display's size, or stale.
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

        /// 2.2. A window's hairline is harvested at most once per flight: a chase or refresh that
        /// carried one marks it, a re-warmed window is harvested when its picture lands, and a
        /// dressed one keeps its ring. Duplicates in the animated set collapse.
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

        /// 2.2. A flight tracks what was harvested; a fresh flight has harvested nothing.
        #[test]
        fn a_flight_starts_with_nothing_harvested() {
            let running = flight(None);
            assert!(running.harvested.is_empty());
            assert!(!capture_work_allowed(running.phase(), CaptureKind::Harvest));
        }

        // Change C: real windows land before lift.

        const DISPLAY: CGRect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width: 1728.0, height: 1117.0 },
        };

        /// 2.3, 3.3. Layout keeps 0.75 and 0.5; a strip movement applies at frame zero, or 0.5 with a resize.
        #[test]
        fn apply_points_by_flight_kind() {
            assert_eq!(apply_frames_at(FlightKind::Layout, false), 0.75);
            assert_eq!(apply_frames_at(FlightKind::Layout, true), 0.5);
            assert_eq!(apply_frames_at(FlightKind::Strip, false), APPLY_FRAMES_AT_STRIP);
            assert_eq!(APPLY_FRAMES_AT_STRIP, 0.0, "a strip movement places its windows at frame zero");
            assert_eq!(apply_frames_at(FlightKind::Strip, true), 0.5);
        }

        /// 2.3. Applied frames go stale when a tile changed or any final frame did.
        #[test]
        fn frames_go_stale_on_a_tile_or_an_untiled_change() {
            assert!(!mark_stale_on_untiled_change(false, false));
            assert!(mark_stale_on_untiled_change(true, false));
            assert!(mark_stale_on_untiled_change(false, true));
            assert!(mark_stale_on_untiled_change(true, true));
        }

        /// 2.3. An in-flight pass that only moves a parked (untiled) window's destination clears
        /// `frames_applied`, so `step` re-sends at the apply point. A redundant pass does not.
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

        /// 2.3. The report over the log's cases: wsid=108's 65pt clamp is excluded; a 3446pt
        /// park miss (the leaving window still on screen) is excluded because its intent is the
        /// park; two on-screen misses count both and name the worst; a clean flight reports none.
        #[test]
        fn handover_report_counts_on_screen_misses_only() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            // wsid=108: intended y=1116, clamped by macOS to 1051. Not the flight's error.
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

        /// 2.3. For random flights of on-screen slots and parks with random real frames,
        /// `count_over` is the brute-force count over on-screen intents, and the worst never
        /// comes from a park. Seed 97, 200 runs.
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
                        !crate::model::HiddenWindowPlacement::is_off_screen(DISPLAY, *intended)
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
                        !crate::model::HiddenWindowPlacement::is_off_screen(DISPLAY, worst.1),
                        "seed 97: the worst came from a park"
                    );
                }
                over_seen += expected_over;
            }
            assert!(over_seen > 0, "generator sanity: no misses in {RUNS} runs");
        }

        // Change D: holds are bounded at `HOLD_CAP` and cheap.

        /// The hold bound before Change D, kept here so the cap is checked against it.
        fn reveal_hold_limit_old(duration: Duration) -> Duration {
            duration.mul_f64(0.4).max(Duration::from_millis(300))
        }

        /// 2.4. A capture is settled when it matches the one before it, or when it no longer
        /// renders like the picture cached before the resize. Neither known: not settled.
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

        /// 2.4. The hold is capped for every flight duration; the old formula stays visible in
        /// the function and the cap wins over it. The cap is 300ms: 150 flew most entrances with
        /// no picture at all (see "A grow holds, then reveals" in the doc).
        #[test]
        fn the_hold_is_capped_at_a_blink() {
            for ms in [180u64, 300, 375, 500, 1000] {
                let d = Duration::from_millis(ms);
                assert_eq!(reveal_hold_limit(d), HOLD_CAP, "{ms}ms flight");
                assert!(reveal_hold_limit(d) <= reveal_hold_limit_old(d), "{ms}ms flight");
            }
            assert_eq!(HOLD_CAP, Duration::from_millis(300));
            assert_eq!(REVEAL_CHASE_INTERVAL, Duration::from_millis(8));
            // The same ~1s ceiling as 40 x 25ms.
            assert_eq!(REVEAL_CHASE_INTERVAL * REVEAL_CHASE_ATTEMPTS as u32, Duration::from_secs(1));
        }

        /// 2.4. For random durations the hold never exceeds `HOLD_CAP` and never exceeds the old
        /// bound. Seed 98, 200 runs.
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

        /// 2.4. A tile flying the placeholder (its picture cannot cover its destination) is a
        /// reveal in waiting after the hold timed out and cleared `awaiting`, and after a grow
        /// joined mid-flight; its settled reveal is swapped before 0.6 and cached after.
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

        /// 2.6, inverted. A holding flight sends EVERY final frame at frame zero, the entrance's
        /// slot included: held back, the chase captured the newcomer at spawn size and the picture
        /// left a hole in the slot (2026-09-15 3:28:10). Nothing is owed at the apply point.
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

            // Nothing placed yet (a plain flight, or a stale in-flight merge): everything goes at
            // the apply point, once.
            let mut running = flight(Some(Instant::now()));
            running.final_frames = final_frames.clone();
            assert_eq!(running.frames_due(running.apply_at - 0.01), None);
            assert_eq!(running.frames_due(running.apply_at), Some(final_frames.clone()));
            assert!(running.frames_applied);
            assert_eq!(running.frames_due(1.0), None, "sent once");
        }

        /// 2.6, inverted. An entrance's picture must cover its slot like a grow's reveal must
        /// cover its destination: the real window is at the slot from frame zero, so the chase
        /// can deliver one. A smaller picture is not claimed and the hold goes on.
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

    /// Preservation for `.kiro/specs/flight-render-stability/bugfix.md` 3.x: flights with no
    /// mid-flight arrival, hold, resize, park, or pictureless window. Each assertion pins the
    /// output observed on unfixed code, over generated inputs outside the bug condition.
    ///
    /// 3.1 is a review, not a test. Observed in `src/ui/workspace_overlay.rs`: `opacity` occurs
    /// only in `ShadowStyle` and `setShadowOpacity`; the animated key paths are `position`,
    /// `bounds`, `shadowPath`, `path`, `contentsRect`, so nothing animates `contents` or
    /// `opacity`; all 11 `CATransaction::begin()` calls are followed by `setDisableActions(true)`.
    ///
    /// `finish`, `step`, `start_strip` and `start_moving` need the actor, so P-3.4, 3.6, 3.13,
    /// 3.14 and 3.16 assert on the decisions those paths make: `capture_work_allowed`,
    /// `take_refresh`, `hold_wait`, `frame_zero_work`, `SnapshotCache::usable`.
    mod render_stability_preservation {
        use super::preservation::{DISPLAY, Gen, RUNS, stacked};
        use super::*;
        use crate::model::HiddenWindowPlacement;
        use crate::ui::window_snapshot::{
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

        /// P-3.2. Observed: an awaited window's picture is `Claim` before the flight starts and
        /// `Admit` after, whatever the settle flag, the thumbprint match, or the progress; never
        /// `CacheOnly`. `progress_if_started` is the `None`/`Some` the decision keys on.
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

        /// Outside the bug condition on the swap path. Observed: a window with no tile, a picture
        /// that does not cover the destination, or one rendering like the cached picture is
        /// cached only, for every state and progress.
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

        /// P-3.3. Observed: the layout path's apply points are 0.75 for a move and 0.5 for a
        /// resize, the same as before this spec.
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

        /// P-3.4. `finish` drops the flight before it warms `last_animated`, so the warm and the
        /// desktop render run with no flight. Observed: both are allowed then, and `phase` names
        /// each stage of a flight from `started` and `awaiting` alone.
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

        /// P-3.5. Observed: every landed picture leaves an entry in the cache whatever the swap
        /// decision, and a usable picture is never replaced by a clipped one.
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

        /// P-3.6. Observed on unfixed code: sweeping progress 0 to 1, `take_refresh` fires at
        /// 0.00 and at 0.50, two per flight. The preserved part is the 0.5 slot: exactly one
        /// refresh slot at or after the midpoint. The slot is taken whether or not it has targets;
        /// what it recaptures is `refresh_targets`, at most the two ends of a focus change.
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

        /// P-3.13. Observed: a grow whose picture cannot cover the destination enters `awaiting`
        /// and holds; before the deadline `start_moving` waits for what is left of it, at the
        /// deadline it flies with the placeholder; a fitting reveal landing first is claimed and a
        /// small one is not.
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

        /// P-3.14. `start_strip` draws only `cache.usable` pictures and keeps every window in
        /// `final_frames` and `last_animated`. Observed: a window never captured, or captured as a
        /// sliver, is not usable; the handover report counts only tiled windows; and the warm
        /// after the movement wants a capture for it.
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

                let (from, to) = strip_travel(slot, CGPoint::new(0.0, 0.0), CGPoint::new(861.0, 0.0), false);
                let mut running = flight(None);
                running.tiles.push(stacked(wid(3), from, to, Some(0), false));
                running.final_frames = vec![(wid(1), to), (wid(2), to), (wid(3), to)];
                let tiled: Vec<WindowId> = running.tiles.iter().map(|t| t.window).collect();
                let real: HashMap<WindowId, CGRect> =
                    running.final_frames.iter().copied().collect();
                let report = handover_report(&running.final_frames, &tiled, &real, DISPLAY);
                // A destination past the edge is a park, which the report excludes (2.3).
                let measured = usize::from(!HiddenWindowPlacement::is_off_screen(DISPLAY, to));
                assert_eq!(report.total, measured, "seed 95: only the drawn window is measured");
                assert_eq!(report.count_over, 0);

                let size = (slot.size.width, slot.size.height);
                assert!(needs_capture(None, size), "seed 95: warmed after the movement");
                assert!(needs_capture(Some(clipped(slot.size).coverage), size));
                assert!(!needs_capture(Some(test_snapshot(slot.size).coverage), size));
            }
        }

        /// P-3.16. `start_strip` hands `begin_group` empty `awaiting` and `entrances` and starts
        /// `Immediate`. Observed: such a flight holds for nothing, applies nothing at frame zero,
        /// and has no deadline to wait out.
        #[test]
        fn a_pan_holds_for_nothing() {
            assert_eq!(frame_zero_work(&[], &[], &[], &[]), (false, Vec::new(), Vec::new()));
            let mut rng = Gen(96);
            for _ in 0..RUNS {
                let count = rng.below(4) as u32 + 1;
                let travel = CGPoint::new(rng.pt(-1720.0, 1720.0), 0.0);
                let mut running = flight(None);
                running.apply_at = apply_frames_at(FlightKind::Strip, false);
                for i in 1..=count {
                    let frame = rng.on_screen();
                    let (from, to) = strip_travel(frame, CGPoint::new(0.0, 0.0), travel, false);
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

    /// Preservation for the regressions fix (`.kiro/specs/exit-entrance-animation-regressions`,
    /// bugfix.md 3.x): flights with no open or close. Each assertion pins the output observed on the
    /// code before the fix, over generated inputs outside the bug condition.
    mod preservation {
        use super::*;
        use crate::model::HiddenWindowPlacement;
        use crate::model::z_group::{GROUP_STRIDE, MAX_TILE_DEPTH};
        use crate::ui::window_snapshot::{SnapshotCache, test_snapshot};

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

        /// P-3.1/3.7. Observed: with the window server reporting an on-screen frame that differs
        /// from both the request's start and its destination, the tile starts from the server's
        /// frame. A plain move keeps the late apply point.
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
                if real.same_as(to) || HiddenWindowPlacement::is_off_screen(DISPLAY, real) {
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

        /// P-3.7. Observed: no server answer falls back to the request's start; a server answer
        /// already at the destination honours the request's (synthetic) start.
        #[test]
        fn a_missing_or_synthetic_start_falls_back_to_the_request() {
            let from = rect(4.0, 32.0, 859.0, 1081.0);
            let to = rect(867.0, 32.0, 859.0, 1081.0);
            assert_eq!(resolve_start(None, from, to, DISPLAY, None), from);
            assert_eq!(resolve_start(Some(to), from, to, DISPLAY, None), from);
        }

        /// P-3.1/3.5. Observed: frames never applied early are never re-requested by a coalescing
        /// merge, whatever changed. `merge_action` is the same three-way decision, and
        /// `merge_final_frames` reports a change exactly when the merge retargets.
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

        /// P-3.2/3.6. Observed: a coalescing pass carrying reveal holds and no entrance merges its
        /// holds (latest size per window wins), marks frames applied, sets one deadline of
        /// `reveal_hold_limit`, and hands back the flight's frames to request again.
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

        /// P-3.2. Observed: a hold cannot stop a flight already moving, and a pass with no holds
        /// extends nothing.
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

        /// P-3.3/3.4. Observed: a pan translates a parked window by the viewport's travel like any
        /// other, `from = frame - from_offset`; the park is never remapped to an entry frame here.
        #[test]
        fn a_pan_translates_a_parked_window_without_remapping_it() {
            let mut rng = Gen(33);
            for _ in 0..RUNS {
                let size = CGSize::new(rng.pt(400.0, 1720.0), 1081.0);
                let frame = rng.park(size);
                let from_offset = CGPoint::new(rng.pt(-4000.0, 4000.0), 0.0);
                let to_offset = CGPoint::new(rng.pt(-4000.0, 4000.0), 0.0);
                let (from, to) = strip_travel(frame, from_offset, to_offset, false);
                assert_eq!(from.origin.x, frame.origin.x - from_offset.x);
                assert_eq!(to.origin.x, frame.origin.x - to_offset.x);
                assert_eq!(from.size, frame.size);
                assert_eq!(to.origin.x - from.origin.x, from_offset.x - to_offset.x);
                assert_eq!(from.origin.y, frame.origin.y, "a pan keeps the park's row");
                let entry = HiddenWindowPlacement::entry_frame(frame, to, DISPLAY);
                if from_offset.x.abs() != 1.0 {
                    assert_ne!(from, entry, "the pan path does not consult the park remap");
                }
            }
        }

        /// P-3.8: a restack with a floating focus puts every floating tile in `[0, STRIDE)` and
        /// every strip tile in `[STRIDE, 2*STRIDE)`; with a strip focus the reverse. Seed 38, 200
        /// runs.
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

        /// A focus that is not among the tiles (a close whose focus target is gone, or none at
        /// all) is a strip interaction: the strip is banded in front.
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

        /// The server's order is untrusted input: an absurd order stays inside its band, and the
        /// deepest possible tile still draws in front of the backdrop.
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

        /// P-3.9. Observed: a window closing from a bottom-corner park or from off the strip shows
        /// nothing along its exit path and is not animated; one closing on screen is.
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

        /// P-3.10. Observed: forgetting a window empties its cache entry while a clone taken for an
        /// exit tile stays usable on its own.
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

        /// P-3.11. Observed: a fresh flight with an entrance applies the real frames at frame zero
        /// and chases `(window, to.size)`; a hold does the same for its awaiting set; a plain
        /// flight does neither. The entrance reaches `frame_zero_work` the way `start` sends it:
        /// through the hold entry `entrance_reservation` hands back.
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

    /// Change 3 of `.kiro/specs/exit-entrance-animation-regressions`: depth is banded once per
    /// flight from the flight's latest focus, whichever pass composed the tile, so the strip is
    /// one z-order group in the overlay as it is on the real screen (`model/z_group.rs`). See
    /// "Mid-flight passes" in `docs/animation-smoothness.md`.
    mod flight_restack {
        use super::preservation::{Gen, RUNS, stacked};
        use super::*;
        use crate::model::z_group::{GROUP_STRIDE, MAX_TILE_DEPTH};

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

        /// The 1.1 scenario: pass 1 (a close, focus not among the tiles) bands the strip in
        /// front; pass 2 (a pan, focus on the floating window) retargets one strip tile. Every
        /// tile is rebanded from the new focus, the redundant strip tile included, so the
        /// floating window is in front of BOTH terminals, never between them.
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

        /// A retargeting pass that carries a new server order (the server re-reported the window
        /// after a raise) moves the tile within its band. A redundant tile is untouched by
        /// `merge`, order included, so it keeps its place within the band.
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

        /// An entrance tile (`server_order: Some(0)`: a window is raised on open) leads its own
        /// band, which with a floating focus is the band behind.
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

        /// The rule, as a property: for random tile sets, pass orders, and focus choices (among
        /// the tiles, off them, or none), with a strip focus or none EVERY floating tile is deeper
        /// than EVERY strip tile; with a floating focus the reverse. The focused tile leads.
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

    /// Change 5 of `.kiro/specs/exit-entrance-animation-regressions`: the park remap is decided
    /// from both the server's frame and the request's, before the synthetic-start test.
    mod park_remap {
        use super::preservation::{DISPLAY, Gen, RUNS};
        use super::*;
        use crate::model::HiddenWindowPlacement;

        const SLOT: CGRect = CGRect {
            origin: CGPoint { x: 4.0, y: 32.0 },
            size: CGSize { width: 1720.0, height: 1081.0 },
        };
        const PARK: CGRect = CGRect {
            origin: CGPoint { x: 1727.0, y: 1116.0 },
            size: CGSize { width: 1720.0, height: 1081.0 },
        };
        const RIGHT_EDGE: CGRect = CGRect {
            origin: CGPoint { x: 1728.0, y: 32.0 },
            size: CGSize { width: 1720.0, height: 1081.0 },
        };

        /// Kiro's park shows 41pt: `is_off_screen` says visible, the requested park says parked.
        #[test]
        fn a_41pt_park_enters_from_the_right_edge() {
            let real = rect(1727.0, 1076.0, 1720.0, 1081.0);
            assert!(!HiddenWindowPlacement::is_off_screen(DISPLAY, real));
            assert_eq!(resolve_start(Some(real), PARK, SLOT, DISPLAY, None), RIGHT_EDGE);
        }

        /// Finder's park shows 52pt.
        #[test]
        fn a_52pt_park_enters_from_the_right_edge() {
            let real = rect(1727.0, 1065.0, 859.0, 1081.0);
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let park = rect(1727.0, 1116.0, 859.0, 1081.0);
            assert!(!HiddenWindowPlacement::is_off_screen(DISPLAY, real));
            assert_eq!(
                resolve_start(Some(real), park, slot, DISPLAY, None),
                rect(1728.0, 32.0, 859.0, 1081.0)
            );
        }

        /// The server already reports the slot while the reactor still holds the park: the park
        /// wins over the synthetic-start test, so the tile enters from the edge, not the corner.
        #[test]
        fn a_park_the_server_reports_at_its_slot_enters_from_the_edge() {
            assert_eq!(resolve_start(Some(SLOT), PARK, SLOT, DISPLAY, None), RIGHT_EDGE);
        }

        /// A genuine park (1pt showing) with no server answer still enters from the edge.
        #[test]
        fn a_park_with_no_server_answer_enters_from_the_edge() {
            assert_eq!(resolve_start(None, PARK, SLOT, DISPLAY, None), RIGHT_EDGE);
        }

        /// Both frames on screen and the server already at the destination: the request's start is
        /// a deliberate fiction and is honoured.
        #[test]
        fn a_synthetic_start_on_screen_is_still_honoured() {
            let from = rect(4.0, 32.0, 859.0, 1081.0);
            let to = rect(867.0, 32.0, 859.0, 1081.0);
            assert_eq!(resolve_start(Some(to), from, to, DISPLAY, None), from);
        }

        /// Drift with both frames on screen: the server's frame wins.
        #[test]
        fn drift_on_screen_starts_from_the_servers_frame() {
            let from = rect(4.0, 32.0, 859.0, 1081.0);
            let real = rect(120.0, 32.0, 859.0, 1081.0);
            let to = rect(867.0, 32.0, 859.0, 1081.0);
            assert_eq!(resolve_start(Some(real), from, to, DISPLAY, None), real);
        }

        /// For any corner park as the request's start, whatever the server reports (the park, a
        /// clamped park, or the slot), the tile starts in `to`'s row with `to`'s size, just past
        /// the display edge on the park's side.
        #[test]
        fn any_corner_park_enters_from_its_own_edge() {
            let mut rng = Gen(52);
            for _ in 0..RUNS {
                let to = rng.on_screen();
                let from = rng.park(to.size);
                let clamp = rng.pt(0.0, 60.0);
                let real = match rng.below(3) {
                    0 => Some(from),
                    1 => Some(rect(from.origin.x, from.origin.y - clamp, to.size.width, to.size.height)),
                    _ => Some(to),
                };
                let got = resolve_start(real, from, to, DISPLAY, None);
                let parked_left = from.mid().x < DISPLAY.mid().x;
                let expected_x = if parked_left {
                    DISPLAY.origin.x - to.size.width
                } else {
                    DISPLAY.max().x
                };
                assert_eq!(got.origin.y, to.origin.y, "seed 52: row of {to:?}, got {got:?}");
                assert_eq!(got.size, to.size, "seed 52: size of {to:?}, got {got:?}");
                assert_eq!(got.origin.x, expected_x, "seed 52: park {from:?} real {real:?}, got {got:?}");
            }
        }
    }

    /// The mirror of `park_remap`: a window leaving an on-screen slot for a corner park exits in
    /// its own row past the display edge on the park's side (`resolve_end`), while `final_frames`
    /// keeps the real park. Before, the tile slid diagonally into the corner.
    mod park_exit {
        use super::preservation::{DISPLAY, Gen, RUNS};
        use super::*;

        const SLOT: CGRect = CGRect {
            origin: CGPoint { x: 867.0, y: 32.0 },
            size: CGSize { width: 859.0, height: 1081.0 },
        };
        const RIGHT_PARK: CGRect = CGRect {
            origin: CGPoint { x: 1727.0, y: 1116.0 },
            size: CGSize { width: 859.0, height: 1081.0 },
        };
        const LEFT_PARK: CGRect = CGRect {
            origin: CGPoint { x: -858.0, y: 1116.0 },
            size: CGSize { width: 859.0, height: 1081.0 },
        };

        #[test]
        fn a_window_leaving_for_the_right_park_exits_past_the_right_edge() {
            assert_eq!(
                resolve_end(SLOT, RIGHT_PARK, DISPLAY, None),
                rect(DISPLAY.max().x, SLOT.origin.y, SLOT.size.width, SLOT.size.height)
            );
        }

        #[test]
        fn a_window_leaving_for_the_left_park_exits_past_the_left_edge() {
            assert_eq!(
                resolve_end(SLOT, LEFT_PARK, DISPLAY, None),
                rect(
                    DISPLAY.origin.x - SLOT.size.width,
                    SLOT.origin.y,
                    SLOT.size.width,
                    SLOT.size.height
                )
            );
        }

        #[test]
        fn a_destination_on_screen_is_unchanged() {
            let to = rect(4.0, 32.0, 859.0, 1081.0);
            assert_eq!(resolve_end(SLOT, to, DISPLAY, None), to);
        }

        /// Park to park: nothing shows either way, so the real frame stands.
        #[test]
        fn a_start_already_parked_is_unchanged() {
            assert_eq!(resolve_end(LEFT_PARK, RIGHT_PARK, DISPLAY, None), RIGHT_PARK);
        }

        /// The edge is the display's own, not the global zero.
        #[test]
        fn exit_is_relative_to_the_display_it_happens_on() {
            let display = rect(-670.0, -1692.0, 3008.0, 1692.0);
            let start = rect(-666.0, -1660.0, 859.0, 1081.0);
            let park = rect(2337.0, -1.0, 859.0, 1081.0);
            assert_eq!(
                resolve_end(start, park, display, None),
                rect(2338.0, -1660.0, 859.0, 1081.0)
            );
        }

        /// For any on-screen start and any corner park, the exit is strictly horizontal: the
        /// start's row and size, at the display edge on the park's side.
        #[test]
        fn any_exit_to_a_corner_park_is_horizontal() {
            let mut rng = Gen(53);
            for _ in 0..RUNS {
                let start = rng.on_screen();
                let park = rng.park(start.size);
                let got = resolve_end(start, park, DISPLAY, None);
                let parked_left = park.mid().x < DISPLAY.mid().x;
                let expected_x = if parked_left {
                    DISPLAY.origin.x - start.size.width
                } else {
                    DISPLAY.max().x
                };
                assert_eq!(got.origin.y, start.origin.y, "seed 53: row of {start:?}, got {got:?}");
                assert_eq!(got.size, start.size, "seed 53: size of {start:?}, got {got:?}");
                assert_eq!(got.origin.x, expected_x, "seed 53: park {park:?}, got {got:?}");
            }
        }

        /// Leaving then entering: a window that exited to a park comes back into the same row it
        /// left from, so the round trip is horizontal both ways.
        #[test]
        fn leaving_then_entering_stays_in_the_row() {
            let mut rng = Gen(54);
            for _ in 0..RUNS {
                let slot = rng.on_screen();
                let park = rng.park(slot.size);
                let exit = resolve_end(slot, park, DISPLAY, None);
                let entry = resolve_start(Some(exit), park, slot, DISPLAY, None);
                assert_eq!(entry.origin.y, slot.origin.y, "seed 54: slot {slot:?}, got {entry:?}");
                assert_eq!(entry.size, slot.size, "seed 54: slot {slot:?}, got {entry:?}");
                assert_eq!(entry.origin.x, exit.origin.x, "seed 54: exit {exit:?}, entry {entry:?}");
            }
        }
    }

    /// The strip is one rigid body: a window displaced to a park, or coming back from one, moves
    /// by the same vector as the strip window nearest it (`neighbour_travel`). Aimed at the edge
    /// instead, it covered a different distance under the same duration and curve, so it ran at
    /// its own speed and overlapped its neighbour (seen 2026-09-15). The edge is only the fallback
    /// when nothing beside it moves.
    mod rigid_park {
        use super::preservation::{DISPLAY, Gen, RUNS};
        use super::*;
        use crate::model::HiddenWindowPlacement;

        const W: f64 = 859.0;
        const SLOT_A: CGRect = CGRect {
            origin: CGPoint { x: 4.0, y: 32.0 },
            size: CGSize { width: W, height: 1081.0 },
        };
        const SLOT_B: CGRect = CGRect {
            origin: CGPoint { x: 867.0, y: 32.0 },
            size: CGSize { width: W, height: 1081.0 },
        };
        const RIGHT_PARK: CGRect = CGRect {
            origin: CGPoint { x: 1727.0, y: 1116.0 },
            size: CGSize { width: W, height: 1081.0 },
        };

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        fn moved(frame: CGRect, dx: f64) -> CGRect {
            translated(frame, CGPoint::new(dx, 0.0))
        }

        /// `start()`'s per-request rule on plain rects: the tile's `(from, to)` for request
        /// `index` of `requests` (`(from, to, floating)`), with no server answer.
        fn tile_for(index: usize, requests: &[(CGRect, CGRect, bool)]) -> (CGRect, CGRect) {
            let (from, to, floating) = requests[index];
            let others: Vec<_> = requests
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != index)
                .map(|(_, r)| *r)
                .collect();
            let travel = (!floating)
                .then(|| neighbour_travel(travel_subject(from, to, DISPLAY), &others, DISPLAY))
                .flatten();
            let start = resolve_start(None, from, to, DISPLAY, travel);
            (start, resolve_end(start, to, DISPLAY, travel))
        }

        #[test]
        fn leaving_travels_by_the_neighbours_vector() {
            // A opens a column: A is pushed right by W, B is pushed off to the park.
            let requests = [(SLOT_A, moved(SLOT_A, W), false), (SLOT_B, RIGHT_PARK, false)];
            let (start, to) = tile_for(1, &requests);
            assert_eq!(start, SLOT_B);
            assert_eq!(to, moved(SLOT_B, W), "not the corner, not the edge");
        }

        #[test]
        fn returning_travels_by_the_neighbours_vector() {
            // A column closes: A comes back left by W, B returns from the park to its slot.
            let requests = [(moved(SLOT_A, W), SLOT_A, false), (RIGHT_PARK, SLOT_B, false)];
            let (from, to) = tile_for(1, &requests);
            assert_eq!(to, SLOT_B);
            assert_eq!(from, moved(SLOT_B, W), "enters from where the strip was");
        }

        #[test]
        fn no_moving_neighbour_falls_back_to_the_edge() {
            let requests = [(SLOT_A, SLOT_A, false), (SLOT_B, RIGHT_PARK, false)];
            let (_, to) = tile_for(1, &requests);
            assert_eq!(to, HiddenWindowPlacement::entry_frame(RIGHT_PARK, SLOT_B, DISPLAY));
            let requests = [(SLOT_A, SLOT_A, false), (RIGHT_PARK, SLOT_B, false)];
            let (from, _) = tile_for(1, &requests);
            assert_eq!(from, HiddenWindowPlacement::entry_frame(RIGHT_PARK, SLOT_B, DISPLAY));
        }

        #[test]
        fn a_floating_neighbour_lends_no_travel() {
            let requests = [(SLOT_A, moved(SLOT_A, 300.0), true), (SLOT_B, RIGHT_PARK, false)];
            let (_, to) = tile_for(1, &requests);
            assert_eq!(to, HiddenWindowPlacement::entry_frame(RIGHT_PARK, SLOT_B, DISPLAY));
        }

        #[test]
        fn a_neighbour_that_only_resizes_lends_no_travel() {
            let grown = rect(4.0, 32.0, W + 200.0, 1081.0);
            let requests = [(SLOT_A, grown, false), (SLOT_B, RIGHT_PARK, false)];
            assert_eq!(neighbour_travel(SLOT_B, &requests[..1], DISPLAY), None);
        }

        #[test]
        fn a_parked_neighbour_lends_no_travel() {
            let left_park = rect(-W + 1.0, 1116.0, W, 1081.0);
            let others = [(left_park, SLOT_A, false), (RIGHT_PARK, moved(RIGHT_PARK, -5.0), false)];
            assert_eq!(neighbour_travel(SLOT_B, &others, DISPLAY), None);
        }

        #[test]
        fn the_nearest_neighbour_by_centre_x_wins() {
            let far = rect(4.0, 32.0, 400.0, 1081.0);
            let near = rect(1200.0, 32.0, 400.0, 1081.0);
            let subject = rect(1500.0, 32.0, 200.0, 1081.0);
            let others = [(far, moved(far, 100.0), false), (near, moved(near, -250.0), false)];
            assert_eq!(neighbour_travel(subject, &others, DISPLAY), Some(CGPoint::new(-250.0, 0.0)));
            let others = [(near, moved(near, -250.0), false), (far, moved(far, 100.0), false)];
            assert_eq!(neighbour_travel(subject, &others, DISPLAY), Some(CGPoint::new(-250.0, 0.0)));
        }

        /// N on-screen columns; an open at index k pushes every column from k on by +W and the
        /// last off to a park. Every tile from k on moves by exactly (W, 0); the rest stand still.
        #[test]
        fn an_open_moves_the_displaced_columns_as_one_body() {
            let mut rng = Gen(61);
            for _ in 0..RUNS {
                let n = rng.below(4) as usize + 2;
                let w = rng.pt(300.0, 1680.0 / n as f64 - 4.0);
                let k = rng.below(n as u64 - 1) as usize;
                let columns: Vec<CGRect> =
                    (0..n).map(|i| rect(4.0 + i as f64 * (w + 4.0), 32.0, w, 1081.0)).collect();
                let park = rect(DISPLAY.max().x - 1.0, DISPLAY.max().y - 1.0, w, 1081.0);
                let mut requests: Vec<(CGRect, CGRect, bool)> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let to = if i < k {
                            *c
                        } else if i == n - 1 {
                            park
                        } else {
                            moved(*c, w)
                        };
                        (*c, to, false)
                    })
                    .collect();
                // The newcomer, already placed at slot k.
                requests.push((columns[k], columns[k], false));
                for i in 0..n {
                    let (from, to) = tile_for(i, &requests);
                    let dx = to.origin.x - from.origin.x;
                    let dy = to.origin.y - from.origin.y;
                    let want = if i >= k { w } else { 0.0 };
                    assert_eq!((dx, dy), (want, 0.0), "seed 61: n={n} w={w} k={k} i={i}");
                    assert_eq!(to.size, from.size, "seed 61: n={n} w={w} k={k} i={i}");
                }
            }
        }

        /// The mirror: a close at index k pulls every column from k on back by -W and the parked
        /// one back onto the strip. Every tile from k on moves by exactly (-W, 0), and the
        /// returning tile enters from `to + (W, 0)`.
        #[test]
        fn a_close_pulls_the_displaced_columns_back_as_one_body() {
            let mut rng = Gen(62);
            for _ in 0..RUNS {
                let n = rng.below(4) as usize + 2;
                let w = rng.pt(300.0, 1680.0 / n as f64 - 4.0);
                let k = rng.below(n as u64 - 1) as usize;
                let columns: Vec<CGRect> =
                    (0..n).map(|i| rect(4.0 + i as f64 * (w + 4.0), 32.0, w, 1081.0)).collect();
                let park = rect(DISPLAY.max().x - 1.0, DISPLAY.max().y - 1.0, w, 1081.0);
                let requests: Vec<(CGRect, CGRect, bool)> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let from = if i < k {
                            *c
                        } else if i == n - 1 {
                            park
                        } else {
                            moved(*c, w)
                        };
                        (from, *c, false)
                    })
                    .collect();
                for i in 0..n {
                    let (from, to) = tile_for(i, &requests);
                    let dx = to.origin.x - from.origin.x;
                    let dy = to.origin.y - from.origin.y;
                    let want = if i >= k { -w } else { 0.0 };
                    assert_eq!((dx, dy), (want, 0.0), "seed 62: n={n} w={w} k={k} i={i}");
                    assert_eq!(to.size, from.size, "seed 62: n={n} w={w} k={k} i={i}");
                    if i == n - 1 {
                        assert_eq!(from, moved(columns[i], w), "seed 62: n={n} w={w} k={k}");
                    }
                }
            }
        }
    }

    /// Change 1 of `.kiro/specs/exit-entrance-animation-regressions`: an entrance is a hold. The
    /// flight waits at frame zero for the window's first picture and composes it there, so it
    /// flies in the survivors' transaction; a picture landing after lift-off gets what is left.
    mod entrance_hold {
        use super::preservation::{Gen, RUNS, stacked};
        use super::*;
        use crate::ui::window_snapshot::test_snapshot;

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

        /// Frames are re-requested exactly when they were applied early, the flight is still
        /// coalescing, and a destination changed.
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

        /// A settled picture landing while the flight holds composes the entrance at zero width
        /// in the frame-zero tile set, banded with the others, and shrinks the hold by one.
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

        /// The last hold released hands the flight to `start_moving`; a second picture for the
        /// same window is no longer a hold.
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

        /// A picture that cannot cover the slot is not a claim (its real frame is at the slot from
        /// frame zero, so the chase can deliver one that does; a smaller one drawn over the slot
        /// was a hole), and neither is a flight already moving.
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

        /// After the flight has started, an entrance joins for what is left of it, not the full
        /// duration; before, `admit` does nothing and leaves the reservation to `claim`.
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

        /// Property: for any progress in `[0, 1)`, a late joiner ends no later than the flight, and
        /// at progress 1 it does not travel at all.
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

        /// Property: for random coalescing merges with frames applied early, the frames requested
        /// again are exactly the merged set, latest per window winning, whenever anything changed.
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
    /// drawable moves or a flight is already running. Each case composes
    /// the tiles the way `start` and `start_strip` do and feeds the real `is_moving` verdict in.
    mod still_passes {
        use super::preservation::{Gen, RUNS, stacked};
        use super::*;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        fn moving_drawable(tiles: &[OverlayTile]) -> bool {
            tiles.iter().any(|tile| is_moving(tile.from, tile.to))
        }

        /// 1.8: Zoom opens over the strip. The new window has no picture, so it is an entrance
        /// and not a tile; every strip window is a still request. Nothing drawable moves, so the
        /// window is placed in place and no overlay goes up.
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

        /// A Terminal opening beside a Kiro column: the neighbour is pushed aside, so the pass
        /// flies as a hold for the entrance.
        #[test]
        fn a_strip_open_that_moves_a_neighbour_flies() {
            let before = rect(0.0, 32.0, 1720.0, 1081.0);
            let after = rect(0.0, 32.0, 860.0, 1081.0);
            let tiles = vec![stacked(wid(1), before, after, Some(1), false)];
            let (_, waiting) = entrance_reservation(wid(2), rect(867.0, 32.0, 859.0, 1081.0), false);
            assert!(waiting.is_some());
            assert!(worth_flying(moving_drawable(&tiles), false));
        }

        /// A pan translates every unpinned window by the viewport's travel.
        #[test]
        fn a_pan_flies() {
            let frame = rect(867.0, 32.0, 859.0, 1081.0);
            let (from, to) =
                strip_travel(frame, CGPoint::new(0.0, 0.0), CGPoint::new(867.0, 0.0), false);
            let tiles = vec![stacked(wid(1), from, to, Some(1), false)];
            assert!(worth_flying(moving_drawable(&tiles), false));
        }

        /// A still-only pass arriving while a flight runs still merges: its destinations belong to
        /// the flight, and placing them now would yank windows out from under the overlay.
        #[test]
        fn a_still_pass_joining_a_running_flight_flies() {
            let s1 = rect(0.0, 32.0, 860.0, 1081.0);
            let tiles = vec![stacked(wid(1), s1, s1, Some(1), false)];
            assert!(worth_flying(moving_drawable(&tiles), true));
            assert!(worth_flying(false, true), "even with nothing drawable at all");
        }

        /// Property (P-3.3): over random still and moving mixes, any moving tile is enough to
        /// fly, and a pass is grounded only when every tile stands still and no flight is running.
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
        // The real case: a display frame inset by a 32pt menu bar. A window at y = 32 must land at
        // y = 0 inside the overlay, or the entire animation is drawn 32pt too low.
        let overlay = rect(0.0, 32.0, 1728.0, 1085.0);
        let window = rect(865.0, 32.0, 859.0, 1081.0);
        assert_eq!(to_overlay_space(window, overlay), rect(865.0, 0.0, 859.0, 1081.0));
    }

    /// The surface gives the way the view was pushed: focus right at the last column pulls the
    /// strip left, the next workspace at the bottom pulls the row up.
    #[test]
    fn an_edge_bounce_moves_the_content_the_way_it_would_have_gone() {
        use crate::layout_engine::Direction;
        let o = EDGE_BOUNCE_OVERSHOOT;
        assert_eq!(edge_bounce_overshoot(Direction::Right), CGPoint::new(-o, 0.0));
        assert_eq!(edge_bounce_overshoot(Direction::Left), CGPoint::new(o, 0.0));
        assert_eq!(edge_bounce_overshoot(Direction::Down), CGPoint::new(0.0, -o));
        assert_eq!(edge_bounce_overshoot(Direction::Up), CGPoint::new(0.0, o));
        assert!(o < 100.0, "a nudge, not a scroll");
    }

    /// A bounce joining a flight keeps the overlay up until its return leg is done, and never
    /// shortens a flight that outlasts it.
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
        // Off-strip windows sit at negative x, measured as far as -1680, and must stay to the left
        // of the overlay rather than being clamped into it.
        let overlay = rect(0.0, 32.0, 1728.0, 1085.0);
        assert_eq!(
            to_overlay_space(rect(-1680.0, 32.0, 1720.0, 1081.0), overlay),
            rect(-1680.0, 0.0, 1720.0, 1081.0)
        );
    }

    #[test]
    fn overlay_space_handles_a_second_display_at_an_offset() {
        // A display to the right has windows at large positive x. Without subtracting the overlay
        // origin they would be drawn off the right edge of that display's own overlay.
        let overlay = rect(1728.0, 32.0, 1728.0, 1085.0);
        assert_eq!(
            to_overlay_space(rect(1728.0, 32.0, 859.0, 1081.0), overlay),
            rect(0.0, 0.0, 859.0, 1081.0)
        );
    }

    /// The overlay lifts once the clock is done AND the render server presents every layer at its
    /// destination; a stuck presentation is overridden after `LIFT_GRACE`. Never before the clock.
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
        // Guards a division by zero, and makes `--no-animate` style zero durations resolve at once
        // rather than never finishing.
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
        // Clamped rather than allowed past 1.0, since the easing would otherwise overshoot the
        // target position when a frame arrives late.
        assert_eq!(finished.progress(), 1.0);
        assert!(finished.is_done());
    }

    /// A pass merging into a flight in progress: `merge_plans` retargets containers, reparents
    /// only on a membership change, and carries a pan to every group. The 3:27:20 tear (an open
    /// merged with a pan 56ms later; 22 survivors scrolled 574pt, the newcomer did not) is the
    /// case it exists for. See "Mid-flight passes" in `docs/animation-smoothness.md`.
    mod rigid_strip {
        use super::preservation::{DISPLAY, Gen, RUNS};
        use super::rigid_groups::random_requests;
        use super::*;
        use crate::actor::workspace_animation::plan::*;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        /// The display the 3:27:20 flight was on: an external at a non-zero origin, so overlay
        /// space and display space differ and a retarget that forgot the conversion shows.
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
                Member::Changing { .. } | Member::Entrance { .. } => Some(GroupKey::StripLoose),
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
                        GroupKey::StripLoose => CGPoint::new(0.0, 0.0),
                        key => plan.groups.iter().find(|g| g.key == *key).map(|g| g.travel).unwrap_or(CGPoint::new(0.0, 0.0)),
                    };
                    (*key, CGPoint::new(p.x - travel.x / 2.0, p.y - travel.y / 2.0))
                })
                .collect()
        }

        /// An entrance with a frame in the later pass takes it, converted to overlay space; one
        /// the pass did not place keeps its reservation; one already at the frame is not counted.
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

        /// A pan `d` merging into an open: every group's position moves by `d`, nothing changes
        /// hands, the entrance's destination moves by `d`.
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
            let pan = strip_plan(
                &[
                    StripWindow { window: wid(1), server_id: WindowServerId::new(1), frame: a, pinned: false, floating: false },
                    StripWindow { window: wid(2), server_id: WindowServerId::new(2), frame: shifted(b, CGPoint::new(859.0, 0.0)), pinned: false, floating: false },
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

        /// A pass confirming destinations the flight already has changes nothing.
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

        /// One member of a two-member group is sent elsewhere: it is reparented, the other keeps
        /// the container, and both end where the pass says.
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

        /// The 1:07 switch: the whole row rides one container up by a display height; the layout
        /// pass 16ms later parks two of its windows, whose visual destination is past the display
        /// edge. Those members keep riding the row instead of opening a sideways group (the
        /// zig-zag). A member of a STILL container sent off screen still votes and leaves.
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

        /// A rigid member the pass now resizes leaves its container for `StripLoose` at the frame
        /// it is drawn at, and is retargeted as a resize.
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
            assert_eq!(delta.reparented, vec![(wid(2), group, GroupKey::StripLoose)]);
            assert_eq!(delta.retargeted_tiles, vec![(wid(2), grown)]);
            let Some(Member::Changing { from, to }) = merged.member(wid(2)) else { panic!("loose") };
            assert_eq!(from, overlay_of(b, presented[&group]), "leaves at the presented frame");
            assert_eq!(to, grown);
            assert_eq!(key_of(&merged, wid(1)), Some(group));
        }

        /// A newcomer whose vector matches a group's remaining travel rides it, with `rel` taken
        /// from the container's presented position.
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

        /// A newcomer with a vector no group is still travelling by opens a group of its own.
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

        #[test]
        fn group_travel_after_merge_is_none_only_when_nothing_changed() {
            let presented = CGPoint::new(-200.0, 0.0);
            let old = CGPoint::new(-574.0, 0.0);
            assert_eq!(group_travel_after_merge(presented, old, old), None);
            assert_eq!(group_travel_after_merge(presented, old, CGPoint::new(-574.05, 0.0)), None);
            assert_eq!(
                group_travel_after_merge(presented, old, CGPoint::new(-900.0, 0.0)),
                Some((presented, CGPoint::new(-900.0, 0.0)))
            );
        }

        /// The 3:27:20 case: an open with 22 survivors and one entrance, then a 574pt pan 56ms
        /// later. Every survivor and the entrance end at the pan's frames.
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
            let windows: Vec<StripWindow> = requests
                .iter()
                .map(|(w, _, to, _)| StripWindow {
                    window: *w,
                    server_id: WindowServerId::new(w.idx.get()),
                    frame: to_overlay_space(*to, EXTERNAL),
                    pinned: false,
                    floating: false,
                })
                .collect();
            let pan = strip_plan(&windows, CGPoint::new(-574.0, 0.0), CGPoint::new(0.0, 0.0));
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

        /// A member the pass names by frame but does not compose (a strip pass with no usable
        /// picture for it) rides its group: its destination moves with the container's.
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

        /// Property P1 (seed 149, 200 runs): a random initial plan, then 1-4 random merged passes
        /// (layout passes over a subset with random vectors, pans, a resize, a new window), with
        /// `presented` at the model position or midway. After every merge each named window ends
        /// within 2pt of the pass's destination in the key the delta says (P4); no window is in
        /// two groups; a pan-only step changes no membership and shifts every position by `d`.
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

                    // P4: every window the pass names ends within 2pt of its destination, in the
                    // key the delta says.
                    for w in incoming.windows() {
                        let want = match incoming.member(w).unwrap() {
                            Member::Rigid { key, rel } => overlay_of(rel, incoming.groups.iter().find(|g| g.key == key).unwrap().travel),
                            Member::Changing { to, .. } | Member::Entrance { to, .. } | Member::Floating { to, .. } => to,
                        };
                        let got = dest(&merged, w).unwrap_or_else(|| panic!("{tag}: {w:?} unnamed after merge"));
                        // The one exception to P4: a member sent off the viewport while its
                        // container moves rides the container (`rides_out`); it keeps its key.
                        let rode_out = pan.is_none()
                            && crate::model::HiddenWindowPlacement::is_off_screen(DISPLAY, want)
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

        /// Property P5 (seed 151, 200 runs): merging a plan with itself changes nothing.
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

    /// Task 1 of `.kiro/specs/rigid-strip-groups`: a pass as rigid pieces. `reflow_plan` groups
    /// by translation vector within `GROUP_TOLERANCE`; `strip_plan` is one group. See "Layout
    /// changes" and "Strip movements" in `docs/animation-smoothness.md`.
    mod rigid_groups {
        use super::preservation::{DISPLAY, Gen, RUNS, stacked};
        use super::*;
        use crate::actor::workspace_animation::plan::*;
        use crate::model::HiddenWindowPlacement;
        use crate::model::z_group::StackGroup;
        use crate::ui::window_snapshot::is_a_resize;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        fn shifted(frame: CGRect, dx: f64, dy: f64) -> CGRect {
            CGRect::new(CGPoint::new(frame.origin.x + dx, frame.origin.y + dy), frame.size)
        }

        fn column(i: f64) -> CGRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        fn moving(plan: &ReflowPlan) -> Vec<&StripGroup> {
            plan.groups.iter().filter(|g| !g.members.is_empty() && g.key != GroupKey::STILL).collect()
        }

        fn members(group: &StripGroup) -> Vec<WindowId> {
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
            assert_eq!(groups[0].key, GroupKey::Strip(1));
            assert_eq!(groups[1].key, GroupKey::Strip(2));
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

        /// The park rule feeds grouping: `resolve_end` gives a window leaving for a park its
        /// neighbour's vector, so `reflow_plan` puts the two in one group.
        #[test]
        fn a_window_leaving_for_a_park_rides_its_moving_neighbours_group() {
            let a = column(0.0);
            let b = column(1.0);
            let park = Gen(5).park(a.size);
            assert!(HiddenWindowPlacement::is_off_screen(DISPLAY, park));
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

        /// With no moving neighbour the parked window leaves past the edge (`entry_frame`) and is a
        /// group of one with that vector.
        #[test]
        fn a_window_leaving_for_a_park_alone_is_a_group_of_one() {
            let a = column(1.0);
            let park = rect(DISPLAY.size.width - 1.0, DISPLAY.size.height - 1.0, a.size.width, a.size.height);
            let travel = neighbour_travel(travel_subject(a, park, DISPLAY), &[], DISPLAY);
            assert_eq!(travel, None);
            let a_end = resolve_end(a, park, DISPLAY, travel);
            assert_eq!(a_end, HiddenWindowPlacement::entry_frame(park, a, DISPLAY));
            let still = column(0.0);

            let plan = reflow_plan(&[(wid(1), a, a_end, false), (wid(2), still, still, false)], DISPLAY);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![wid(1)]);
            assert_eq!(groups[0].travel, CGPoint::new(a_end.origin.x - a.origin.x, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![wid(2)]);
        }

        fn strip_window(idx: u32, frame: CGRect, pinned: bool, floating: bool) -> StripWindow {
            StripWindow { window: wid(idx), server_id: WindowServerId::new(idx), frame, pinned, floating }
        }

        /// The 3:27:20 pan: 22 survivors scrolled 574pt. One group, 22 members, one travel.
        #[test]
        fn the_3_27_20_pan_is_one_group_of_twenty_two() {
            let windows: Vec<StripWindow> =
                (0..22).map(|i| strip_window(i + 1, column(i as f64), false, false)).collect();
            let from_offset = CGPoint::new(-574.0, 0.0);
            let to_offset = CGPoint::new(0.0, 0.0);
            let plan = strip_plan(&windows, from_offset, to_offset);
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].members.len(), 22);
            assert_eq!(groups[0].travel, CGPoint::new(-574.0, 0.0));
            assert_eq!(groups[0].travel, strip_pan_travel(from_offset, to_offset));
            for (window, member) in windows.iter().zip(&groups[0].members) {
                let (from, to) = strip_travel(window.frame, from_offset, to_offset, false);
                assert_eq!(member.window, window.window);
                assert_eq!(member.rel, from);
                assert_eq!(overlay_of(member.rel, groups[0].travel), to, "rel plus travel is the destination");
            }
            assert!(plan.groups[0].members.is_empty());
            assert!(plan.floating.is_empty() && plan.changing.is_empty());
            assert_eq!(plan.floating_travel, CGPoint::new(0.0, 0.0));
        }

        /// A pan pins its floating windows: they stand in the floating container, which does not move.
        #[test]
        fn pinned_windows_are_floating_with_zero_travel() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let windows = vec![
                strip_window(1, column(0.0), false, false),
                strip_window(2, settings, true, true),
            ];
            let plan = strip_plan(&windows, CGPoint::new(-574.0, 0.0), CGPoint::new(0.0, 0.0));
            assert_eq!(plan.floating, vec![(wid(2), settings, settings)]);
            assert_eq!(plan.floating_travel, CGPoint::new(0.0, 0.0));
            assert_eq!(plan.member(wid(2)), Some(Member::Floating { from: settings, to: settings }));
            assert_eq!(members(moving(&plan)[0]), vec![wid(1)]);
        }

        /// A switch moves its floating windows by the strip's travel: the container carries them.
        #[test]
        fn a_switch_moves_the_floating_container_by_the_strip_travel() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let windows = vec![
                strip_window(1, column(0.0), false, false),
                strip_window(2, settings, false, true),
            ];
            let from_offset = CGPoint::new(0.0, 0.0);
            let to_offset = CGPoint::new(0.0, 1117.0);
            let plan = strip_plan(&windows, from_offset, to_offset);
            let travel = strip_pan_travel(from_offset, to_offset);
            assert_eq!(travel, CGPoint::new(0.0, -1117.0));
            assert_eq!(moving(&plan)[0].travel, travel);
            assert_eq!(plan.floating_travel, travel);
            assert_eq!(plan.floating, vec![(wid(2), settings, settings)], "the tile itself stands in its container");
            let (_, to) = strip_travel(settings, from_offset, to_offset, false);
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
            assert_eq!(flight.positions[&GroupKey::Strip(1)], CGPoint::new(-100.0, 0.0));
            assert_eq!(flight.positions[&GroupKey::StripLoose], CGPoint::new(0.0, 0.0));
            assert_eq!(flight.positions[&GroupKey::Floating], CGPoint::new(0.0, 0.0));
            assert_eq!(flight.next_key, 2);
            assert!(PlanDelta::default().is_empty());
        }

        /// A random pass: 1-12 windows with vectors from a palette of 1-4 distinct vectors (at
        /// least 4pt apart on some axis, and from zero) plus ±1pt jitter, some still, some
        /// resizing, some floating.
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

        /// Property (seed 131, 200 runs): partition; every member within 2pt of its group's
        /// travel; members of different groups more than 2pt apart on some axis; the still group
        /// is `groups[0]` at zero; no changing or floating member in a group.
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
                    assert_eq!(group.key, GroupKey::Strip(i as u16), "{tag}: keys in plan order");
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

        /// The 2026-09-15 3:28:10 open: the newcomer's slot at x=867, the neighbour shifted right
        /// by 859. One moving group with the neighbour, and the still group.
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

        /// A preset resize of the middle column: the left neighbour stands, the middle changes,
        /// the right neighbour shifts by the width change and is the one moving group.
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

        /// A close: every survivor to the right shifts left by the closed width, as one group.
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

        /// A close: the closed window is not in the pass at all; the survivors are one group. A
        /// pass is worth flying when something drawable moves or a flight runs, and only then.
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

        /// A pass with only a floating move: no groups, one floating member, nothing rigid.
        #[test]
        fn a_floating_only_pass_has_no_groups_and_one_floating_member() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let plan = reflow_plan(&[(wid(1), settings, shifted(settings, 40.0, 20.0), true)], DISPLAY);
            assert!(moving(&plan).is_empty());
            assert!(plan.groups[0].members.is_empty());
            assert_eq!(plan.floating, vec![(wid(1), settings, shifted(settings, 40.0, 20.0))]);
            let flight = FlightPlan::from(plan);
            let targets = crate::ui::workspace_overlay::animation_targets(&flight);
            assert_eq!(targets.len(), 1, "the floating tile flies on its own");
        }

        /// A grow still holds: `outgrows` on the picture against the destination puts the window
        /// in `awaiting`, and `extend_hold` on a collecting flight takes it.
        #[test]
        fn a_grow_still_enters_awaiting() {
            use crate::ui::window_snapshot::{outgrows, test_snapshot};
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

        /// `entrance_plan`: `Travel` from the spawn frame, or a reservation with its reason.
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

        /// `frame_zero_work`: holding sends every frame; a spawn entrance alone sends its slot and
        /// chases; nothing pending sends nothing.
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

        /// A flight composed with a spawn entrance does not hold: `awaiting` is empty, the chase
        /// set is not, there is no hold deadline, and the entrance's tile is a reveal in waiting
        /// while its spawn picture does not cover the slot.
        #[test]
        fn a_spawn_entrance_flies_without_a_hold_and_is_a_reveal_in_waiting() {
            use crate::ui::window_snapshot::test_snapshot;
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
            let targets = crate::ui::workspace_overlay::animation_targets(&FlightPlan::from(plan));
            assert_eq!(targets.len(), 1);
        }

        /// A spawn entrance's chase landing before the flight moves replaces its picture without
        /// releasing anything; a picture that does not cover the slot is refused.
        #[test]
        fn a_spawn_entrances_early_chase_picture_is_taken_without_a_release() {
            use crate::ui::window_snapshot::test_snapshot;
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

        /// Property (seed 157, 200 runs): `entrance_plan` is `Travel` iff the spawn frame has
        /// size, the picture is usable and the budget is left; `Travel` carries the spawn frame
        /// and the slot, and the spawn has positive width.
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

        /// The 50/50 pair with Settings over them (`model/z_group.rs`): with a strip focus the
        /// floating container is behind; with Settings focused it is in front. The strip container
        /// holding the focus comes first; a companion carries its window's depth.
        #[test]
        fn band_plan_puts_the_floating_container_behind_the_strip_unless_it_holds_focus() {
            use crate::model::z_group::{GROUP_STRIDE, tile_depth};
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
            assert_eq!(banding.strip_order, vec![still, moving], "the focused group first");
            assert_eq!(banding.within[&wid(90)], 0, "the focused window leads its container");
            assert_eq!(banding.within[&wid(89)], tile_depth(Some(2), false, StackGroup::Strip, StackGroup::Strip));
            assert_eq!(banding.within[&wid(5830)], tile_depth(Some(1), false, StackGroup::Floating, StackGroup::Floating));
            let anchor = tiles.iter().find(|t| t.window == wid(90)).unwrap().depth;
            assert_eq!(banding.within[&wid(900)], anchor % GROUP_STRIDE, "a companion takes its window's depth");

            restack(&mut tiles, Some(wid(5830)));
            let banding = band_plan(&plan, &tiles, Some(wid(5830)));
            assert!(banding.floating_in_front);
            assert_eq!(banding.within[&wid(5830)], 0);
            assert_eq!(banding.strip_order, vec![still, moving], "no strip group holds focus: shallowest first");
        }

        /// Property P3 (seed 163, 200 runs): for random tiles and a random focused group, the
        /// container's band less the tile's within-band depth is `-tile_depth` exactly, so every
        /// floating tile is behind every strip tile with a strip focus and in front with a floating one.
        #[test]
        fn container_bands_plus_within_depths_reproduce_tile_depth() {
            use crate::model::z_group::{container_z, tile_depth};
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
                        if focused_group == StackGroup::Strip {
                            assert!(f < s, "{tag}: floating {f} in front of strip {s}");
                        } else {
                            assert!(f > s, "{tag}: floating {f} behind strip {s}");
                        }
                    }
                }
                // Every occupied strip container is ordered once; the floating one never is.
                let mut order = banding.strip_order.clone();
                order.sort_by_key(|k| format!("{k:?}"));
                order.dedup();
                assert_eq!(order.len(), banding.strip_order.len(), "{tag}");
                assert!(!banding.strip_order.contains(&GroupKey::Floating), "{tag}");
                for g in plan.groups.iter().filter(|g| !g.members.is_empty()) {
                    assert!(banding.strip_order.contains(&g.key), "{tag}: {:?} unordered", g.key);
                }
            }
        }

        /// `plan_from_tiles` on the merged tiles is the same partition `reflow_plan` gives the
        /// requests they came from: a coalescing merge recomposes frame zero without the requests.
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

        /// Property (seed 137, 200 runs): `fly` installs what `animation_targets` names and
        /// nothing else, so no rigid member is a tile target, every non-still group is a
        /// container target exactly once, and every changing / entrance / moving floating member
        /// is a tile target exactly once (Requirement 11.4).
        #[test]
        fn animation_targets_name_every_moving_piece_once_and_no_rigid_member() {
            use crate::ui::workspace_overlay::{AnimationTarget, animation_targets};
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
